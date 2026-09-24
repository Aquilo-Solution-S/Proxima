//! Backend-owned [`UnitOfWork`]: one transaction, several Engine writes.

use super::Engine;
use super::memory_authoring::PreparedDerived;
use crate::access::Relation;
use crate::authz::{AuthzContext, SystemAuthority, SystemAuthorityBinding};
use crate::error::ProtocolError;
use crate::storage_ports::{
    HostStateCommand, HostStateOutcome, HostStateParticipantDescriptor, HostStateReplyKind,
    HostStateRequest, HostStateWriteOrigin, HostStateWritePermit, SidecarSessionRead, WriteSession,
};
use crate::verbs::fact_ingest::{CitationSpec, FactIngestOutcome, FactWriteCommand};
use crate::verbs::goal_write::{
    CreateGoalAtomicRequest, GoalCreateRequest, GoalDraft, GoalReplayRequest, GoalWriteOutcome,
};
use crate::verbs::query::SidecarAtom;
use crate::{
    DerivedMemoryOutcome, EntityKind, FactPayload, GoalPayload, MemoryId, Owner, SchemaId,
    SidecarPayload,
};

/// One typed observation addressed to an explicit destination owner.
/// Natural-key handle reuse includes earlier writes in the same transaction.
/// Facts declare reference pins, never derivation origins.
#[derive(Clone)]
pub struct FactWrite<'a, P: FactPayload> {
    owner: Owner,
    source_id: &'a str,
    payload: &'a P,
    observed_at: Option<time::OffsetDateTime>,
    citation: Option<CitationSpec>,
    handle: Option<crate::SeriesHandle>,
    lexical_language: Option<String>,
    refs: Vec<MemoryId>,
}

impl<P: FactPayload> std::fmt::Debug for FactWrite<'_, P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FactWrite")
            .field("source_id", &self.source_id)
            .field("schema_id", &P::SCHEMA_ID)
            .field("has_citation", &self.citation.is_some())
            .field("owner", &self.owner)
            .field("refs", &self.refs.len())
            .finish_non_exhaustive()
    }
}

impl<'a, P: FactPayload> FactWrite<'a, P> {
    /// Typed sidecar Fact from `source_id`. Observe now unless the
    /// caller overrides.
    ///
    /// The lexical language starts as
    /// [`crate::lexical_language::LEXICAL_LANGUAGE_DEPLOYMENT_DEFAULT`]:
    /// a write built here has NAMED the deployment's configuration, which
    /// is a choice, where a draft carrying no language has made none.
    /// A schema whose contract declares `LanguagePolicy::PerRow` accepts
    /// the first and refuses the second, so this builder cannot produce
    /// the refusable state — [`Self::lexical_language`] overrides it.
    #[must_use]
    pub fn new(owner: Owner, source_id: &'a str, payload: &'a P) -> Self {
        Self {
            owner,
            source_id,
            payload,
            observed_at: None,
            citation: None,
            handle: None,
            lexical_language: Some(
                crate::lexical_language::LEXICAL_LANGUAGE_DEPLOYMENT_DEFAULT.to_owned(),
            ),
            refs: Vec::new(),
        }
    }

    /// Observation time stamped on the receipt.
    #[must_use]
    pub const fn observed_at(mut self, observed_at: time::OffsetDateTime) -> Self {
        self.observed_at = Some(observed_at);
        self
    }

    /// Opaque cited-object / mapping hint (`CitationPlan::DraftHint`).
    #[must_use]
    pub fn citation(mut self, citation: CitationSpec) -> Self {
        self.citation = Some(citation);
        self
    }

    /// Reuse this series handle. Unset ⇒ NK lookup, then mint.
    #[must_use]
    pub const fn handle(mut self, handle: crate::SeriesHandle) -> Self {
        self.handle = Some(handle);
        self
    }

    /// Stamp a resolved lexical language on the memory row.
    #[must_use]
    pub fn lexical_language(mut self, lexical_language: impl Into<String>) -> Self {
        self.lexical_language = Some(lexical_language.into());
        self
    }

    /// Observation-neutral reference pins (write-act, visit).
    #[must_use]
    pub fn refs(mut self, refs: impl IntoIterator<Item = MemoryId>) -> Self {
        self.refs.extend(refs);
        self
    }
}

/// A [`FactWrite`] that owns its source id and payload, so the write can
/// move onto a task the caller does not own ([`Engine::ingest_fact_detached`]).
struct OwnedFactWrite<P: FactPayload> {
    owner: Owner,
    source_id: String,
    payload: P,
    observed_at: Option<time::OffsetDateTime>,
    citation: Option<CitationSpec>,
    handle: Option<crate::SeriesHandle>,
    lexical_language: Option<String>,
    refs: Vec<MemoryId>,
}

impl<P: FactPayload + Clone> FactWrite<'_, P> {
    fn to_owned_write(&self) -> OwnedFactWrite<P> {
        OwnedFactWrite {
            owner: self.owner,
            source_id: self.source_id.to_owned(),
            payload: self.payload.clone(),
            observed_at: self.observed_at,
            citation: self.citation.clone(),
            handle: self.handle,
            lexical_language: self.lexical_language.clone(),
            refs: self.refs.clone(),
        }
    }
}

impl<P: FactPayload> OwnedFactWrite<P> {
    fn as_write(&self) -> FactWrite<'_, P> {
        FactWrite {
            owner: self.owner,
            source_id: &self.source_id,
            payload: &self.payload,
            observed_at: self.observed_at,
            citation: self.citation.clone(),
            handle: self.handle,
            lexical_language: self.lexical_language.clone(),
            refs: self.refs.clone(),
        }
    }
}

/// One transaction the Engine can attach several authorized writes to.
/// Drop without [`Self::commit`] rolls the transaction back.
///
/// The transaction is not opened by [`Engine::unit_of_work`]. Authorization
/// and embedding run before persistence; automatic natural-key selection
/// runs inside the storage transaction.
/// [`crate::storage_ports::WriteSessionFactory::begin`] happens on the first
/// write, advisory lock, forget, or host-state command. A multi-derived batch
/// ([`Self::derive_memories`]) embeds every text before that begin, so
/// a file of N chunks does not hold a pool slot across N provider RTTs.
///
/// Host-state commands ([`Self::apply_host_state`]) run on the same
/// transaction as Fact writes. Do not hold the unit open across broker or
/// provider network I/O. A participant error poisons the unit: later writes
/// fail and [`Self::commit`] refuses so a partial host operation cannot land.
pub struct UnitOfWork<'a> {
    engine: &'a Engine,
    authorization: UnitAuthorization<'a>,
    session: Option<Box<dyn WriteSession>>,
    committed: bool,
    /// Set when a host-state participant returns a storage error after the
    /// session opened. Commit is then refused and drop rolls the unit back.
    poisoned: bool,
    /// Memory `t`s written in this transaction. Later writes may cite them
    /// before commit; `authorize_entry_read` only sees committed rows.
    written: Vec<MemoryId>,
    /// Session-visible `(t, kind)` pairs. A declaration must still agree
    /// with the kind of a row written earlier in this transaction.
    written_kinds: Vec<(MemoryId, EntityKind)>,
    /// Resolved destinations, for owner-constrained pending Goal assignments.
    written_owners: Vec<(MemoryId, Owner)>,
}

enum UnitAuthorization<'a> {
    Ordinary(&'a AuthzContext),
    HostState {
        owner: Owner,
        descriptor: HostStateParticipantDescriptor,
    },
}

/// Uncloneable capability minted for one booted engine's actual, frozen
/// host-state participant registration.
pub struct HostStateMaintenanceAuthority {
    system_binding: SystemAuthorityBinding,
    descriptor: HostStateParticipantDescriptor,
    _private: (),
}

impl std::fmt::Debug for HostStateMaintenanceAuthority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostStateMaintenanceAuthority")
            .field("descriptor", &self.descriptor)
            .finish_non_exhaustive()
    }
}

/// One-owner host-state maintenance unit. It shares the ordinary unit's
/// lazy session, poisoning, commit, and rollback behavior while exposing no
/// cognitive write or read methods.
pub struct HostStateUnitOfWork<'a> {
    inner: UnitOfWork<'a>,
}

fn validate_registration(
    registry: &crate::verbs::schema::FlavorRegistryFrozen,
    descriptor: HostStateParticipantDescriptor,
) -> Result<(), ProtocolError> {
    if descriptor.participant_id().as_str().is_empty() {
        return Err(ProtocolError::invalid_argument(
            "host_state",
            "registered host-state participant id is empty",
        ));
    }
    if descriptor.tables().is_empty() {
        return Err(ProtocolError::invalid_argument(
            "host_state",
            "registered host-state participant declares no state surfaces",
        ));
    }
    for (index, table) in descriptor.tables().iter().enumerate() {
        if table.as_str().is_empty() {
            return Err(ProtocolError::invalid_argument(
                "host_state",
                "registered host-state participant declares an empty state surface",
            ));
        }
        if descriptor.tables()[..index].contains(table) {
            return Err(ProtocolError::invalid_argument(
                "host_state",
                format!(
                    "registered host-state participant declares {} more than once",
                    table.as_str()
                ),
            ));
        }
        if !registry.is_declared_state_surface(table.as_str()) {
            return Err(ProtocolError::invalid_argument(
                "host_state",
                format!(
                    "registered table {} is not a declared FlavorContract.state_surfaces binding",
                    table.as_str()
                ),
            ));
        }
    }
    Ok(())
}

fn validate_host_state_command<C: HostStateCommand>(
    engine: &Engine,
    descriptor: HostStateParticipantDescriptor,
) -> Result<(), ProtocolError> {
    if C::PARTICIPANT_ID != descriptor.participant_id() {
        return Err(ProtocolError::invalid_argument(
            "host_state",
            format!(
                "host-state participant {} is not the registered participant {}",
                C::PARTICIPANT_ID.as_str(),
                descriptor.participant_id().as_str()
            ),
        ));
    }
    if C::TABLES.is_empty() {
        return Err(ProtocolError::invalid_argument(
            "host_state",
            "host-state command declares no state surfaces",
        ));
    }
    for (index, table) in C::TABLES.iter().enumerate() {
        if C::TABLES[..index].contains(table) {
            return Err(ProtocolError::invalid_argument(
                "host_state",
                "host-state command declares a state surface more than once",
            ));
        }
        if !engine.registry().is_declared_state_surface(table.as_str()) {
            return Err(ProtocolError::invalid_argument(
                "host_state",
                format!(
                    "table {} is not a declared FlavorContract.state_surfaces binding",
                    table.as_str()
                ),
            ));
        }
        if !descriptor.tables().contains(table) {
            return Err(ProtocolError::invalid_argument(
                "host_state",
                format!(
                    "host-state command names {}, which the registered participant does not declare",
                    table.as_str()
                ),
            ));
        }
    }
    Ok(())
}

impl std::fmt::Debug for HostStateUnitOfWork<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostStateUnitOfWork")
            .field("inner", &self.inner)
            .finish()
    }
}

impl std::fmt::Debug for UnitOfWork<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UnitOfWork")
            .field("committed", &self.committed)
            .field("poisoned", &self.poisoned)
            .field("open", &self.session.is_some())
            .finish_non_exhaustive()
    }
}

impl Engine {
    /// Open a backend-owned write unit. The Postgres transaction starts on
    /// the first write (see [`UnitOfWork`]).
    ///
    /// # Errors
    ///
    /// Storage faults from beginning the transaction (deferred to the first
    /// write).
    #[allow(clippy::unused_async)] // async is part of the port contract, not of this body
    pub async fn unit_of_work<'a>(
        &'a self,
        authz: &'a AuthzContext,
    ) -> Result<UnitOfWork<'a>, ProtocolError> {
        Ok(UnitOfWork {
            engine: self,
            authorization: UnitAuthorization::Ordinary(authz),
            session: None,
            committed: false,
            poisoned: false,
            written: Vec::new(),
            written_kinds: Vec::new(),
            written_owners: Vec::new(),
        })
    }

    /// Mint the host-only maintenance capability from this engine's boot
    /// witness and its backend's actual frozen participant registration.
    ///
    /// `None` means the backend has no participant. Invalid or undeclared
    /// registrations fail closed before the runtime is exposed.
    ///
    /// # Errors
    /// Returns a forbidden error for a foreign boot witness, or an invalid
    /// argument error when the backend registration is malformed or names
    /// undeclared state surfaces.
    pub fn host_state_maintenance_authority(
        &self,
        system_authority: &SystemAuthority,
    ) -> Result<Option<HostStateMaintenanceAuthority>, ProtocolError> {
        if !system_authority.authorizes(&self.system_authority_binding) {
            return Err(ProtocolError::forbidden(
                "system authority belongs to a different engine boot",
            ));
        }
        let Some(descriptor) = self.storage.write_session.host_state_descriptor() else {
            return Ok(None);
        };
        validate_registration(&self.registry, descriptor)?;
        Ok(Some(HostStateMaintenanceAuthority {
            system_binding: system_authority.binding(),
            descriptor,
            _private: (),
        }))
    }

    /// Open the host-only, one-owner transaction surface. Engine identity
    /// and the live storage descriptor are checked before any session can
    /// begin.
    ///
    /// # Errors
    /// Returns a forbidden error when the capability belongs to another
    /// engine boot or the backend registration changed, and an invalid
    /// argument error for a malformed or undeclared registration.
    pub fn host_state_unit_of_work<'a>(
        &'a self,
        authority: &HostStateMaintenanceAuthority,
        owner: Owner,
    ) -> Result<HostStateUnitOfWork<'a>, ProtocolError> {
        if authority.system_binding != self.system_authority_binding {
            return Err(ProtocolError::forbidden(
                "host-state maintenance authority belongs to a different engine",
            ));
        }
        if self.storage.write_session.host_state_descriptor() != Some(authority.descriptor) {
            return Err(ProtocolError::forbidden(
                "host-state participant registration changed after authority minting",
            ));
        }
        validate_registration(&self.registry, authority.descriptor)?;
        Ok(HostStateUnitOfWork {
            inner: UnitOfWork {
                engine: self,
                authorization: UnitAuthorization::HostState {
                    owner,
                    descriptor: authority.descriptor,
                },
                session: None,
                committed: false,
                poisoned: false,
                written: Vec::new(),
                written_kinds: Vec::new(),
                written_owners: Vec::new(),
            },
        })
    }

    /// Persist one typed observation and commit it atomically.
    ///
    /// # Errors
    /// Returns authorization, schema, citation, reference, or storage errors.
    pub async fn ingest_fact<P: FactPayload + Clone>(
        &self,
        authz: &AuthzContext,
        spec: FactWrite<'_, P>,
    ) -> Result<FactIngestOutcome, ProtocolError> {
        let mut uow = self.unit_of_work(authz).await?;
        let outcome = uow.ingest_fact(spec).await?;
        uow.commit().await?;
        Ok(outcome)
    }

    /// [`Self::ingest_fact`] on a spawned task, for the Fact that records a
    /// side effect already done upstream (a sent mail, a pushed commit): the
    /// write must land even when the request that caused it is dropped.
    ///
    /// The write is copied and spawned on the first poll, so once this
    /// future has been polled, dropping it (client disconnect, handler
    /// cancellation) no longer cancels the write; it only stops waiting for
    /// the result. `deadline` bounds how long the spawned task works on the
    /// write: past it the write is abandoned and rolls back, unless its
    /// COMMIT was already sent, in which case the outcome is unknown. Every
    /// error the write returns is logged, whether or not anyone still waits.
    ///
    /// The task is not drained at shutdown: a write still in flight when the
    /// runtime stops is cancelled. Retrying the same write (same owner,
    /// source id and payload) is safe after an unknown outcome: Fact ingest
    /// is keyed on the payload's receipt key, so a write that did land
    /// replays (`idempotent_replay`).
    ///
    /// # Errors
    /// Returns [`Self::ingest_fact`]'s errors, and `Internal` when the write
    /// missed `deadline` or its task did not finish (panicked, or the runtime
    /// shut down).
    pub async fn ingest_fact_detached<P: FactPayload + Clone>(
        self: &std::sync::Arc<Self>,
        authz: &AuthzContext,
        spec: FactWrite<'_, P>,
        deadline: std::time::Duration,
    ) -> Result<FactIngestOutcome, ProtocolError> {
        let engine = std::sync::Arc::clone(self);
        let authz = authz.clone();
        let write = spec.to_owned_write();
        let schema_id = P::SCHEMA_ID;
        let task = tokio::spawn(tracing::Instrument::instrument(
            async move {
                let ingest = engine.ingest_fact(&authz, write.as_write());
                let Ok(result) = tokio::time::timeout(deadline, ingest).await else {
                    tracing::warn!(
                        schema_id,
                        ?deadline,
                        "detached fact ingest missed its deadline; outcome unknown, \
                         retrying the same write replays"
                    );
                    return Err(ProtocolError::internal(format!(
                        "detached ingest of {schema_id} missed its {deadline:?} deadline"
                    )));
                };
                if let Err(error) = &result {
                    tracing::warn!(
                        schema_id,
                        error = %error.message,
                        "detached fact ingest failed; the side effect it records is unrecorded"
                    );
                }
                result
            },
            tracing::Span::current(),
        ));
        match task.await {
            Ok(result) => result,
            Err(error) => Err(ProtocolError::internal(format!(
                "detached ingest of {schema_id} did not finish: {error}"
            ))),
        }
    }
}

impl UnitOfWork<'_> {
    fn authz(&self) -> Result<&AuthzContext, ProtocolError> {
        match self.authorization {
            UnitAuthorization::Ordinary(authz) => Ok(authz),
            UnitAuthorization::HostState { .. } => Err(ProtocolError::forbidden(
                "host-state unit does not authorize ordinary cognitive writes",
            )),
        }
    }

    async fn ensure_session(&mut self) -> Result<&mut Box<dyn WriteSession>, ProtocolError> {
        if self.committed {
            return Err(ProtocolError::internal("unit of work already committed"));
        }
        if self.poisoned {
            return Err(ProtocolError::internal(
                "unit of work failed after a host-state error; drop it or call commit to abort",
            ));
        }
        if self.session.is_none() {
            let factory = &self.engine.storage().write_session;
            let session = match &self.authorization {
                UnitAuthorization::Ordinary(authz) => factory.begin(authz.owner_scope()).await,
                UnitAuthorization::HostState { owner, descriptor } => {
                    let permit = HostStateWritePermit::new(
                        *owner,
                        descriptor.participant_id(),
                        descriptor.tables(),
                        HostStateWriteOrigin::Maintenance,
                    );
                    factory.begin_host_state(&permit).await
                }
            }
            .map_err(|err| ProtocolError::internal(err.to_string()))?;
            self.session = Some(session);
        }
        self.session
            .as_mut()
            .ok_or_else(|| ProtocolError::internal("unit of work session missing after begin"))
    }

    /// Append a typed derived row in this transaction, inferring its phase from readable origins.
    ///
    /// # Errors
    /// Returns authorization, provenance, schema, embedding, or storage errors.
    pub async fn derive_memory(
        &mut self,
        memory: crate::DerivedMemory,
    ) -> Result<crate::DerivedMemoryOutcome, ProtocolError> {
        let item = self
            .engine
            .prepare_derived_memory(
                self.authz()?,
                memory,
                &self.written_kinds,
                self.session.is_some(),
            )
            .await?;
        self.write_prepared_derived(item).await
    }

    /// Prepare a batch before opening the write transaction, then append every row.
    /// An already-open transaction defers embedding. Drop without commit rolls back the batch.
    /// Use sequential `derive_memory` calls when one result is an input to the next.
    ///
    /// # Errors
    /// Returns authorization, provenance, schema, embedding, or storage errors.
    pub async fn derive_memories(
        &mut self,
        memories: impl IntoIterator<Item = crate::DerivedMemory>,
    ) -> Result<Vec<crate::DerivedMemoryOutcome>, ProtocolError> {
        let mut prepared = Vec::new();
        for memory in memories {
            prepared.push(
                self.engine
                    .prepare_derived_memory(
                        self.authz()?,
                        memory,
                        &self.written_kinds,
                        self.session.is_some(),
                    )
                    .await?,
            );
        }
        let mut outcomes = Vec::with_capacity(prepared.len());
        for item in prepared {
            outcomes.push(self.write_prepared_derived(item).await?);
        }
        Ok(outcomes)
    }

    /// Serialize this transaction against `key` (`pg_advisory_xact_lock`).
    ///
    /// # Errors
    ///
    /// Storage faults from the lock.
    pub async fn advisory_xact_lock(&mut self, key: i64) -> Result<(), ProtocolError> {
        self.ensure_session()
            .await?
            .advisory_xact_lock(key)
            .await
            .map_err(|err| ProtocolError::internal(err.to_string()))
    }

    /// Serialize this transaction against `key` in shared mode
    /// (`pg_advisory_xact_lock_shared`).
    ///
    /// A fenced scope's writers take this; the scope's eraser takes the
    /// exclusive mode. Writers do not wait on each other, the erase waits
    /// for all of them, and every writer after it waits for the erase. It
    /// is NOT the mode for the read-model precondition — see
    /// [`Self::read_own_sidecar`], which needs an exclusive holder to make
    /// the check binding.
    ///
    /// # Errors
    ///
    /// Storage faults from the lock.
    pub async fn advisory_xact_lock_shared(&mut self, key: i64) -> Result<(), ProtocolError> {
        self.ensure_session()
            .await?
            .advisory_xact_lock_shared(key)
            .await
            .map_err(|err| ProtocolError::internal(err.to_string()))
    }

    /// Read `owner`'s own sidecar rows inside THIS transaction.
    ///
    /// The read-model precondition affordance: `advisory_xact_lock` →
    /// read → check → append, all on one snapshot. Opens the transaction
    /// if the unit has not written yet, because a read that is not in the
    /// transaction is exactly the thing this exists to stop being. The
    /// exclusive mode is the one that binds it: under
    /// [`Self::advisory_xact_lock_shared`] a second holder of the same key
    /// reads the same absence and appends the same row.
    ///
    /// `owner` goes through the same write gate as the append that follows
    /// it, and the resolved permit — not `read` — is what scopes the rows.
    /// A predicate can narrow the answer; nothing a caller passes widens it
    /// past the authorized owner. Read-only by construction — see
    /// [`SidecarSessionRead`] for both invariants.
    ///
    /// # Errors
    ///
    /// Authorization or storage faults, or a refusal when the table is not
    /// a registered memory sidecar or declares no owner column.
    pub async fn read_own_sidecar(
        &mut self,
        owner: Owner,
        read: &SidecarSessionRead<'_>,
    ) -> Result<Vec<serde_json::Value>, ProtocolError> {
        let write_permit = self
            .engine
            .authorize_write(self.authz()?, &owner, Relation::Editor)
            .await?;
        self.ensure_session()
            .await?
            .read_own_sidecar(write_permit.owner_write_permit(), read)
            .await
            .map_err(|err| ProtocolError::internal(err.to_string()))
    }

    /// Current owned series head for a sidecar key, read inside THIS
    /// transaction.
    ///
    /// [`Engine::owned_series_handle`] answers the same question against the
    /// pool, which is the right thing before a unit of work is open and the
    /// wrong thing once one is: a lock the session holds does not cover a
    /// read that does not run in it.
    ///
    /// # Errors
    ///
    /// Authorization or storage faults, or a refusal when the table is not
    /// a registered memory sidecar.
    pub async fn owned_series_head_memory_id(
        &mut self,
        owner: Owner,
        schema_id: &SchemaId,
        sidecar_table: &str,
        columns: &[(&str, SidecarAtom)],
    ) -> Result<Option<MemoryId>, ProtocolError> {
        let write_permit = self
            .engine
            .authorize_write(self.authz()?, &owner, Relation::Editor)
            .await?;
        self.ensure_session()
            .await?
            .owned_series_head_memory_id(
                write_permit.owner_write_permit(),
                schema_id,
                sidecar_table,
                columns,
            )
            .await
            .map_err(|err| ProtocolError::internal(err.to_string()))
    }

    /// Execute a typed, authorized host-owned state operation on this unit's
    /// existing write session.
    ///
    /// Owner authority is resolved through the same write gate as Fact
    /// ingest. Declared tables must be [`crate::FlavorContract::state_surfaces`]
    /// bindings; unregistered participants and invalid bindings are refused
    /// before mutation. The Postgres backend then runs the command on the
    /// live transaction — host ops may run before or after a Fact write and
    /// see this unit's uncommitted rows. Other connections do not.
    ///
    /// Do not hold the unit open across broker or provider network I/O.
    ///
    /// # Errors
    ///
    /// Authorization, undeclared state-surface bindings, unregistered
    /// participants, or storage faults. A participant error poisons the
    /// unit: further writes fail and [`Self::commit`] refuses.
    pub async fn apply_host_state<C: HostStateCommand>(
        &mut self,
        command: C,
    ) -> Result<HostStateOutcome<C::Outcome>, ProtocolError> {
        let host_permit = match &self.authorization {
            UnitAuthorization::Ordinary(authz) => {
                let owner = command.owner();
                let permit = self
                    .engine
                    .authorize_write(*authz, &owner, Relation::Editor)
                    .await?;
                self.engine
                    .validate_write_permit(permit.owner_write_permit())?;
                let descriptor = self
                    .engine
                    .storage()
                    .write_session
                    .host_state_descriptor()
                    .ok_or_else(|| {
                        ProtocolError::invalid_argument(
                            "host_state",
                            "no host-state participant is registered",
                        )
                    })?;
                validate_host_state_command::<C>(self.engine, descriptor)?;
                HostStateWritePermit::new(
                    *permit.owner_write_permit().owner(),
                    C::PARTICIPANT_ID,
                    C::TABLES,
                    HostStateWriteOrigin::OwnerAuthorized {
                        principal: authz.principal(),
                    },
                )
            }
            UnitAuthorization::HostState {
                owner: fixed_owner,
                descriptor,
            } => {
                if command.owner() != *fixed_owner {
                    return Err(ProtocolError::invalid_argument(
                        "host_state",
                        "host-state command owner does not match this unit's owner",
                    ));
                }
                validate_host_state_command::<C>(self.engine, *descriptor)?;
                HostStateWritePermit::new(
                    *fixed_owner,
                    C::PARTICIPANT_ID,
                    C::TABLES,
                    HostStateWriteOrigin::Maintenance,
                )
            }
        };
        let request = HostStateRequest::from_command(command);
        self.ensure_session().await?;
        // Poison after the session is live, before dispatch, so a cancelled
        // await after participant SQL cannot be followed by a silent commit.
        self.poisoned = true;
        let dispatched = {
            let session = self.session.as_mut().ok_or_else(|| {
                ProtocolError::internal("unit of work session missing after begin")
            })?;
            session.apply_host_state(&host_permit, request).await
        };
        let reply = match dispatched {
            Ok(reply) => {
                self.poisoned = false;
                reply
            }
            Err(err) => {
                return Err(super::errors::map_write_storage_error(
                    err,
                    "host_state",
                    "host-state participant not registered",
                ));
            }
        };
        match reply.downcast::<C::Outcome>() {
            Ok((kind, value)) => Ok(match kind {
                HostStateReplyKind::Permitted => HostStateOutcome::Permitted(value),
                HostStateReplyKind::AlreadyApplied => HostStateOutcome::AlreadyApplied(value),
                HostStateReplyKind::Refused => HostStateOutcome::Refused(value),
            }),
            Err(err) => {
                self.poisoned = true;
                Err(super::errors::map_write_storage_error(
                    err,
                    "host_state",
                    "host-state participant not registered",
                ))
            }
        }
    }

    /// Persist one typed observation in this transaction.
    /// The destination is authorized before any natural-key lookup.
    ///
    /// # Errors
    /// Returns authorization, schema, citation, reference, or storage errors.
    pub async fn ingest_fact<P: FactPayload + Clone>(
        &mut self,
        spec: FactWrite<'_, P>,
    ) -> Result<FactIngestOutcome, ProtocolError> {
        let permit = self
            .engine
            .authorize_write(self.authz()?, &spec.owner, Relation::Editor)
            .await?;
        let observed_at = spec
            .observed_at
            .unwrap_or_else(time::OffsetDateTime::now_utc);
        let references = self
            .engine
            .resolve_memory_targets(self.authz()?, &spec.refs, &self.written_kinds)
            .await?;
        let mut draft = FactWriteCommand::from_payload(spec.source_id, spec.payload, observed_at)
            .with_additional_references(references)
            .with_handle(spec.handle.map(crate::SeriesHandle::into_inner));
        if let Some(citation) = spec.citation {
            draft = draft.with_citation(citation);
        }
        if let Some(language) = spec.lexical_language {
            draft = draft.with_lexical_language(Some(language));
        }
        let sidecars = [SidecarPayload::fact(spec.payload.clone())];
        let authorized = self
            .engine
            .authorize_fact_ingest_permitted_visible(
                self.authz()?,
                permit,
                draft,
                &sidecars,
                &self.written,
                &self.written_kinds,
            )
            .await?;
        self.engine
            .validate_write_permit(authorized.owner_write_permit())?;
        let embedding_spaces = self
            .engine
            .fact_embedding_spaces(
                authorized.owner_write_permit().owner(),
                authorized.draft().schema_id.as_str(),
            )
            .await?;
        let outcome = self
            .ensure_session()
            .await?
            .ingest_fact_with_typed_sidecar(&authorized, &embedding_spaces)
            .await
            .map_err(|err| {
                super::errors::map_write_storage_error(
                    err,
                    "fact",
                    "fact ingest referenced row not found",
                )
            })?;
        if !outcome.idempotent_replay {
            self.written.push(outcome.memory_id);
            self.written_kinds
                .push((outcome.memory_id, EntityKind::Fact));
            self.written_owners
                .push((outcome.memory_id, *authorized.permit().owner()));
        }
        Ok(outcome)
    }

    async fn write_prepared_derived(
        &mut self,
        item: PreparedDerived,
    ) -> Result<DerivedMemoryOutcome, ProtocolError> {
        let storage_req = item.storage_request();
        let outcome = self
            .ensure_session()
            .await?
            .author_derived(&storage_req, item.write_permit.owner_write_permit())
            .await
            .map_err(|err| {
                super::errors::map_write_storage_error(
                    err,
                    "derived",
                    "derived write referenced row not found",
                )
            })?;
        if !outcome.idempotent_replay {
            self.written.push(outcome.memory_id);
            self.written_kinds.push((outcome.memory_id, item.kind));
            self.written_owners
                .push((outcome.memory_id, *item.write_permit.owner()));
        }
        Ok(DerivedMemoryOutcome {
            memory_id: outcome.memory_id,
            idempotent_replay: outcome.idempotent_replay,
            edge_count: outcome.edge_count,
            embedding_deferred: outcome.embedding_deferred,
        })
    }

    /// Authorize and persist one Goal create in this transaction.
    ///
    /// Uses the same typed request as [`Engine::create_goal`]. Earlier Memory
    /// writes in this transaction can supply assignment and evidence targets.
    ///
    /// # Errors
    ///
    /// Authorization or storage faults.
    pub async fn create_goal<P>(
        &mut self,
        request: GoalCreateRequest<P>,
    ) -> Result<GoalWriteOutcome, ProtocolError>
    where
        P: GoalPayload,
    {
        let permit = self
            .engine
            .authorize_write(self.authz()?, &request.owner, Relation::Editor)
            .await?;
        let request = self.engine.normalize_goal_request(request)?;
        self.create_goal_payload_authorized(request, None, &permit)
            .await
    }

    /// Internal episode/protocol adapter with an optional write-act fact.
    pub(crate) async fn create_goal_from_payload_write(
        &mut self,
        req: super::GoalCreatePayloadWriteRequest,
        write_act_t: Option<MemoryId>,
    ) -> Result<GoalWriteOutcome, ProtocolError> {
        let permit = self
            .engine
            .authorize_write(self.authz()?, &req.owner, Relation::Editor)
            .await?;
        let req = super::GoalCreatePayloadWriteRequest {
            payload: self.engine.normalize_payload_write(req.payload)?,
            ..req
        };
        self.create_goal_payload_authorized(req, write_act_t, &permit)
            .await
    }

    async fn create_goal_payload_authorized(
        &mut self,
        req: super::GoalCreatePayloadWriteRequest,
        write_act_t: Option<MemoryId>,
        permit: &super::pipeline::WritePermit,
    ) -> Result<GoalWriteOutcome, ProtocolError> {
        let draft = GoalDraft::active_from_payload_write(
            *permit.owner(),
            req.payload.clone(),
            req.topology.clone(),
            req.wake.clone(),
            req.authorship.clone(),
            req.request_id.clone(),
        );
        let context = self
            .engine
            .goal_atomic_context(req.author_self_perspective_id);
        let atomic = CreateGoalAtomicRequest {
            draft,
            context,
            write_act_t,
        };
        let replay = self
            .ensure_session()
            .await?
            .resolve_goal_replay(
                GoalReplayRequest::Create(&atomic),
                permit.owner_write_permit(),
            )
            .await
            .map_err(|err| {
                super::errors::map_write_storage_error(
                    err,
                    "goal",
                    "goal write referenced row not found",
                )
            })?;
        if let Some(outcome) = replay {
            return outcome.into_goal().map_err(ProtocolError::internal);
        }
        self.engine
            .validate_goal_topology_authorized_visible(
                self.authz()?,
                permit.owner(),
                &req.topology,
                &self.written,
            )
            .await?;
        self.validate_pending_goal_targets(&req, permit.owner())?;
        if req
            .author_self_perspective_id
            .is_none_or(|id| !self.written.contains(&id))
        {
            self.engine
                .author_self_perspective_authorized(self.authz()?, req.author_self_perspective_id)
                .await?;
        }
        self.engine
            .validate_wake_config_for_write(self.authz()?, req.wake.as_ref())
            .await?;
        let outcome = self
            .ensure_session()
            .await?
            .create_goal(&atomic, permit.owner_write_permit())
            .await
            .map_err(|err| {
                super::errors::map_write_storage_error(
                    err,
                    "goal",
                    "goal write referenced row not found",
                )
            })?;
        Ok(outcome)
    }

    fn validate_pending_goal_targets(
        &self,
        req: &super::GoalCreatePayloadWriteRequest,
        goal_owner: &Owner,
    ) -> Result<(), ProtocolError> {
        let assignment = req.topology.assignment().perspective_id();
        if let Some((_, owner)) = self.written_owners.iter().find(|(id, _)| *id == assignment)
            && owner != goal_owner
        {
            return Err(ProtocolError::forbidden(
                super::pipeline::ENTRY_NOT_FOUND_MESSAGE,
            ));
        }
        if let Some((_, kind)) = self.written_kinds.iter().find(|(id, _)| *id == assignment)
            && *kind != EntityKind::Perspective
        {
            return Err(ProtocolError::invalid_argument(
                "assignment",
                "Goal assignment must reference a Perspective",
            ));
        }
        for evidence in req.topology.evidence() {
            if let Some((_, kind)) = self
                .written_kinds
                .iter()
                .find(|(id, _)| *id == evidence.memory_id())
                && !matches!(kind, EntityKind::Fact | EntityKind::Abstraction)
            {
                return Err(ProtocolError::invalid_argument(
                    "evidence",
                    "Goal evidence must reference a Fact or Abstraction",
                ));
            }
        }
        if let Some(id) = req.author_self_perspective_id
            && let Some((_, kind)) = self
                .written_kinds
                .iter()
                .find(|(written, _)| *written == id)
            && *kind != EntityKind::Perspective
        {
            return Err(ProtocolError::invalid_argument(
                "author_self_perspective_id",
                "author self must reference a Perspective",
            ));
        }
        Ok(())
    }

    /// Cool one owned memory `t` in this transaction.
    ///
    /// Delegated one-shot forget is [`Engine::forget_memory`] (generic
    /// [`crate::EngineAuthority`]). This path is `AuthzContext` only.
    ///
    /// # Errors
    ///
    /// Authorization or storage faults.
    pub async fn forget(&mut self, owner: Owner, memory_id: MemoryId) -> Result<(), ProtocolError> {
        let write_permit = self
            .engine
            .authorize_write(self.authz()?, &owner, Relation::Editor)
            .await?;
        self.ensure_session()
            .await?
            .forget_memory(write_permit.owner_write_permit(), memory_id)
            .await
            .map_err(|err| {
                super::errors::map_write_storage_error(err, "memory", "memory not found")
            })
    }

    /// Commit the transaction. Further methods fail.
    ///
    /// # Errors
    ///
    /// Storage commit faults.
    pub async fn commit(mut self) -> Result<(), ProtocolError> {
        if self.committed {
            return Err(ProtocolError::internal("unit of work already committed"));
        }
        if self.poisoned {
            self.session.take();
            return Err(ProtocolError::internal(
                "unit of work failed after a host-state error; the transaction was not committed",
            ));
        }
        self.committed = true;
        let Some(session) = self.session.take() else {
            return Ok(());
        };
        session
            .commit()
            .await
            .map_err(|err| ProtocolError::internal(err.to_string()))
    }
}

impl HostStateUnitOfWork<'_> {
    /// Execute a command for this unit's fixed owner and captured participant.
    ///
    /// # Errors
    /// Returns before storage access for an owner, participant, or table
    /// mismatch. Participant faults poison this unit and prevent commit.
    pub async fn apply_host_state<C: HostStateCommand>(
        &mut self,
        command: C,
    ) -> Result<HostStateOutcome<C::Outcome>, ProtocolError> {
        self.inner.apply_host_state(command).await
    }

    /// Commit this host-state-only unit.
    ///
    /// # Errors
    /// Returns a storage fault or refuses a unit poisoned by participant
    /// failure/cancellation.
    pub async fn commit(self) -> Result<(), ProtocolError> {
        self.inner.commit().await
    }
}

impl Drop for UnitOfWork<'_> {
    fn drop(&mut self) {
        self.session.take();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::flavor::contract::{
        CounterRule, EraseRule, ExportRule, FlavorContract, ForgetRule, KeyShape, ProjectionDecl,
        Surface, TransferRule,
    };
    use crate::storage::StorageError;
    use crate::storage_ports::{
        HostStateCommand, HostStateParticipantDescriptor, HostStateParticipantId, StateSurfaceName,
        StoragePorts, WriteSession, WriteSessionFactory,
    };
    use crate::{Engine, Owner, UserId};

    const PARTICIPANT_ID: HostStateParticipantId = HostStateParticipantId::new("host_state_test");
    const STATE_TABLE: StateSurfaceName = StateSurfaceName::new("host_state_test.state");
    const STATE_TABLES: &[StateSurfaceName] = &[STATE_TABLE];
    const DESCRIPTOR: HostStateParticipantDescriptor =
        HostStateParticipantDescriptor::new(PARTICIPANT_ID, STATE_TABLES);
    const STATE_SURFACES: &[Surface] = &[Surface {
        table: "host_state_test.state",
        key: KeyShape::OwnerId,
        owner_column: Some("owner_id"),
        transfer: TransferRule::StaysOnKey,
        erase: EraseRule::ByOwner,
        export: ExportRule::Rows,
        forget: ForgetRule::Keep {
            why: "a host-state test surface is not a memory sidecar",
        },
        lexical_language_column: None,
        counter: CounterRule::Uncounted {
            why: "the test surface is outside owner-erase receipts",
        },
        completeness: None,
    }];
    static HOST_STATE_TEST_CONTRACT: FlavorContract = FlavorContract {
        flavor_id: "host_state_test",
        ordinal: 97,
        schemas: &[],
        state_surfaces: STATE_SURFACES,
        scopes: &[],
        kernel_surfaces: &[],
        tools: &[],
        resources: &[],
        projection: ProjectionDecl::None {
            why: "the host-state test flavor has no search surface",
        },
        bespoke_erase_legs: &[],
        bespoke_transfer_legs: &[],
    };

    struct EmptyTablesCommand {
        owner: Owner,
    }

    impl HostStateCommand for EmptyTablesCommand {
        const PARTICIPANT_ID: HostStateParticipantId = PARTICIPANT_ID;
        const TABLES: &'static [StateSurfaceName] = &[];
        type Outcome = ();

        fn owner(&self) -> Owner {
            self.owner
        }
    }

    struct BeginSpyFactory {
        begin_calls: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl WriteSessionFactory for BeginSpyFactory {
        fn host_state_descriptor(&self) -> Option<HostStateParticipantDescriptor> {
            Some(DESCRIPTOR)
        }

        async fn begin(
            &self,
            _scope: Option<&crate::OwnerScope>,
        ) -> Result<Box<dyn WriteSession>, StorageError> {
            self.begin_calls.fetch_add(1, Ordering::SeqCst);
            Err(StorageError::Unavailable(
                "the begin spy must not open a session".to_owned(),
            ))
        }
    }

    fn engine(begin_calls: Arc<AtomicUsize>) -> Engine {
        let factory = Arc::new(BeginSpyFactory { begin_calls });
        Engine::try_compose(
            StoragePorts::rejecting_with_write_session(factory),
            |registry| {
                registry.try_add_contract(&HOST_STATE_TEST_CONTRACT)?;
                Ok(())
            },
        )
        .expect("the host-state test registry must freeze")
    }

    #[test]
    fn foreign_engine_maintenance_authority_is_rejected_before_storage_begin() {
        let issuer_begins = Arc::new(AtomicUsize::new(0));
        let (issuer, system, _) = engine(issuer_begins.clone()).into_runtime_authorities();
        let authority = issuer
            .host_state_maintenance_authority(&system)
            .expect("the issuer's descriptor is valid")
            .expect("the test backend has a host-state participant");

        let foreign_begins = Arc::new(AtomicUsize::new(0));
        let foreign_engine = engine(foreign_begins.clone());
        let err = foreign_engine
            .host_state_unit_of_work(&authority, Owner::Personal(UserId::new(uuid::Uuid::nil())))
            .expect_err("another engine boot cannot accept the authority");

        assert!(err.to_string().contains("different engine"));
        assert_eq!(issuer_begins.load(Ordering::SeqCst), 0);
        assert_eq!(foreign_begins.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn empty_host_state_command_is_rejected_before_storage_begin() {
        let begin_calls = Arc::new(AtomicUsize::new(0));
        let (engine, system, _) = engine(begin_calls.clone()).into_runtime_authorities();
        let authority = engine
            .host_state_maintenance_authority(&system)
            .expect("the participant descriptor is valid")
            .expect("the test backend has a host-state participant");
        let owner = Owner::Personal(UserId::new(uuid::Uuid::nil()));
        let mut unit = engine
            .host_state_unit_of_work(&authority, owner)
            .expect("the capability belongs to the engine");

        let err = unit
            .apply_host_state(EmptyTablesCommand { owner })
            .await
            .expect_err("an empty command table declaration must be refused");

        assert!(err.to_string().contains("no state surfaces"));
        assert_eq!(begin_calls.load(Ordering::SeqCst), 0);
    }
}

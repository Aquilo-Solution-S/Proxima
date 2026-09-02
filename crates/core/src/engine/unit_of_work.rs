//! Backend-owned [`UnitOfWork`]: one transaction, several Engine writes.

use super::Engine;
use crate::access::Relation;
use crate::authz::AuthzContext;
use crate::edge::EdgeEndpoint;
use crate::error::ProtocolError;
use crate::storage::{AuthorDerivedRequest, DerivedEmbedding};
use crate::storage_ports::{SidecarSessionRead, WriteSession};
use crate::verbs::fact_ingest::{CitationSpec, FactIngestOutcome, FactWriteCommand};
use crate::verbs::goal_write::{
    CreateGoalAtomicRequest, GoalDraft, GoalReplayRequest, GoalWriteOutcome,
};
use crate::verbs::query::SidecarAtom;
use crate::{
    AuthorDerivedAuthorizedOutcome, AuthorDerivedRequestInput, EntityKind, FactPayload, MemoryId,
    MemoryOperatorKind, Owner, SchemaId, SchemaVersion, SidecarPayload,
};

/// Owned embedding so a prepared batch can outlive the client borrow
/// used by [`DerivedEmbedding`].
enum PreparedEmbedding {
    None,
    Ready { model_id: String, vector: Vec<f32> },
    Deferred { model_id: String },
}

impl PreparedEmbedding {
    fn as_derived(&self) -> DerivedEmbedding<'_> {
        match self {
            Self::None => DerivedEmbedding::None,
            Self::Ready { model_id, vector } => DerivedEmbedding::Ready {
                model_id,
                vector: vector.clone(),
            },
            Self::Deferred { model_id } => DerivedEmbedding::Deferred { model_id },
        }
    }
}

struct PreparedDerived {
    write_permit: super::pipeline::WritePermit,
    owner: Owner,
    memory_id: MemoryId,
    kind: EntityKind,
    text: String,
    schema_id: SchemaId,
    schema_version: SchemaVersion,
    operator_kind: MemoryOperatorKind,
    model_id: String,
    sidecar_payload: SidecarPayload,
    supersedes: Option<MemoryId>,
    lexical_language: Option<String>,
    embedding: PreparedEmbedding,
    origins: Vec<EdgeEndpoint>,
    references: Vec<EdgeEndpoint>,
}

/// One typed Fact write: payload plus optional citation and origins.
///
/// Natural-key handle reuse is automatic when `handle` is unset and the
/// schema declared `natural_key_columns`. That lookup reads committed
/// heads only — two versions of the same key in one [`UnitOfWork`] must
/// pass `handle` explicitly.
#[derive(Clone)]
pub struct TypedFactIngest<'a, P: FactPayload> {
    source_id: &'a str,
    payload: &'a P,
    observed_at: Option<time::OffsetDateTime>,
    citation: Option<CitationSpec>,
    derived_from: Vec<EdgeEndpoint>,
    handle: Option<uuid::Uuid>,
    lexical_language: Option<String>,
    refs: Vec<uuid::Uuid>,
}

impl<P: FactPayload> std::fmt::Debug for TypedFactIngest<'_, P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TypedFactIngest")
            .field("source_id", &self.source_id)
            .field("schema_id", &P::SCHEMA_ID)
            .field("has_citation", &self.citation.is_some())
            .field("derived_from", &self.derived_from.len())
            .finish_non_exhaustive()
    }
}

impl<'a, P: FactPayload> TypedFactIngest<'a, P> {
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
    pub fn new(source_id: &'a str, payload: &'a P) -> Self {
        Self {
            source_id,
            payload,
            observed_at: None,
            citation: None,
            derived_from: Vec::new(),
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

    /// What this Fact was made from (`origin` rows in the same write).
    #[must_use]
    pub fn derived_from(mut self, origins: impl IntoIterator<Item = EdgeEndpoint>) -> Self {
        self.derived_from = origins.into_iter().collect();
        self
    }

    /// Reuse this series handle. Unset ⇒ NK lookup, then mint.
    #[must_use]
    pub const fn handle(mut self, handle: uuid::Uuid) -> Self {
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
    pub fn refs(mut self, refs: impl IntoIterator<Item = uuid::Uuid>) -> Self {
        self.refs = refs.into_iter().collect();
        self
    }
}

/// One transaction the Engine can attach several authorized writes to.
/// Drop without [`Self::commit`] rolls the transaction back.
///
/// The transaction is not opened by [`Engine::unit_of_work`]. Authorization,
/// natural-key lookup, and embedding run against the pool (or the embed
/// client) first; [`crate::storage_ports::WriteSessionFactory::begin`] happens on the first
/// write, advisory lock, or forget. A multi-derived batch
/// ([`Self::author_derived_all`]) embeds every text before that begin, so
/// a file of N chunks does not hold a pool slot across N provider RTTs.
pub struct UnitOfWork<'a> {
    engine: &'a Engine,
    authz: &'a AuthzContext,
    session: Option<Box<dyn WriteSession>>,
    committed: bool,
    /// Memory `t`s written in this transaction. Later writes may cite them
    /// before commit; `authorize_entry_read` only sees committed rows.
    written: Vec<MemoryId>,
    /// Session-visible `(t, kind)` pairs. A declaration must still agree
    /// with the kind of a row written earlier in this transaction.
    written_kinds: Vec<(MemoryId, EntityKind)>,
}

impl std::fmt::Debug for UnitOfWork<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UnitOfWork")
            .field("committed", &self.committed)
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
            authz,
            session: None,
            committed: false,
            written: Vec::new(),
            written_kinds: Vec::new(),
        })
    }

    /// One-shot Fact + typed sidecar: [`UnitOfWork`] of one, then commit.
    ///
    /// # Errors
    ///
    /// Authorization, schema, or storage faults from the ingest.
    pub async fn ingest_typed_fact<P>(
        &self,
        authz: &AuthzContext,
        source_id: &str,
        payload: &P,
    ) -> Result<FactIngestOutcome, ProtocolError>
    where
        P: FactPayload + Clone,
    {
        self.ingest_typed_fact_with(authz, TypedFactIngest::new(source_id, payload))
            .await
    }

    /// One-shot Fact + typed sidecar with citation / origins.
    ///
    /// # Errors
    ///
    /// Authorization, schema, or storage faults from the ingest.
    pub async fn ingest_typed_fact_with<P>(
        &self,
        authz: &AuthzContext,
        spec: TypedFactIngest<'_, P>,
    ) -> Result<FactIngestOutcome, ProtocolError>
    where
        P: FactPayload + Clone,
    {
        let mut uow = self.unit_of_work(authz).await?;
        let outcome = uow.ingest_typed(spec).await?;
        uow.commit().await?;
        Ok(outcome)
    }
}

impl UnitOfWork<'_> {
    async fn ensure_session(&mut self) -> Result<&mut Box<dyn WriteSession>, ProtocolError> {
        if self.committed {
            return Err(ProtocolError::internal("unit of work already committed"));
        }
        if self.session.is_none() {
            let session = self
                .engine
                .storage()
                .write_session
                .begin()
                .await
                .map_err(|err| ProtocolError::internal(err.to_string()))?;
            self.session = Some(session);
        }
        self.session
            .as_mut()
            .ok_or_else(|| ProtocolError::internal("unit of work session missing after begin"))
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

    /// Read `owner`'s own sidecar rows inside THIS transaction.
    ///
    /// The read-model precondition affordance: `advisory_xact_lock` →
    /// read → check → append, all on one snapshot. Opens the transaction
    /// if the unit has not written yet, because a read that is not in the
    /// transaction is exactly the thing this exists to stop being.
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
            .authorize_write(self.authz, &owner, Relation::Editor)
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
            .authorize_write(self.authz, &owner, Relation::Editor)
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

    /// Authorize and persist one typed Fact + sidecar in this transaction.
    ///
    /// # Errors
    ///
    /// Authorization, schema, or storage faults.
    pub async fn ingest_fact<P>(
        &mut self,
        source_id: &str,
        payload: &P,
    ) -> Result<FactIngestOutcome, ProtocolError>
    where
        P: FactPayload + Clone,
    {
        self.ingest_typed(TypedFactIngest::new(source_id, payload))
            .await
    }

    /// Authorize and persist one typed Fact with citation / origins.
    ///
    /// # Errors
    ///
    /// Authorization, schema, or storage faults.
    pub async fn ingest_typed<P>(
        &mut self,
        spec: TypedFactIngest<'_, P>,
    ) -> Result<FactIngestOutcome, ProtocolError>
    where
        P: FactPayload + Clone,
    {
        let observed_at = spec
            .observed_at
            .unwrap_or_else(time::OffsetDateTime::now_utc);
        let mut draft = FactWriteCommand::from_payload(spec.source_id, spec.payload, observed_at)
            .with_derived_from(spec.derived_from)
            .with_handle(spec.handle);
        if let Some(citation) = spec.citation {
            draft = draft.with_citation(citation);
        }
        if let Some(lexical_language) = spec.lexical_language {
            draft = draft.with_lexical_language(Some(lexical_language));
        }
        if !spec.refs.is_empty() {
            draft = draft.with_refs(spec.refs);
        }
        if draft.handle.is_none() {
            let engine = self.engine;
            let authz = self.authz;
            draft.handle = owned_fact_series_handle(engine, authz, spec.payload).await?;
        }
        let sidecars = [SidecarPayload::fact(spec.payload.clone())];
        let authorized = self
            .engine
            .authorize_fact_ingest_visible(
                self.authz,
                Relation::Editor,
                draft,
                &sidecars,
                &self.written,
                &self.written_kinds,
            )
            .await?;
        self.engine
            .validate_write_permit(authorized.owner_write_permit())?;
        let embed_client = self.engine.embed_client();
        let requested = embed_client.as_ref().map(|client| client.model_id());
        let embedding_model_id = self
            .engine
            .vector_model_for(authorized.draft().schema_id.as_str(), requested);
        let outcome = self
            .ensure_session()
            .await?
            .ingest_fact_with_typed_sidecar(&authorized, &sidecars, embedding_model_id)
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
        }
        Ok(outcome)
    }

    /// Authorize and persist one derived memory in this transaction.
    ///
    /// Embedding runs before the transaction starts when this is the first
    /// write. If the transaction is already open, the vector is deferred
    /// (job enqueued, no provider call) so an open pool slot is not held
    /// across HTTP. Prefer [`Self::author_derived_all`] when several
    /// derived rows must share one transaction *and* land with vectors.
    ///
    /// # Errors
    ///
    /// Authorization or storage faults.
    pub async fn author_derived(
        &mut self,
        req: AuthorDerivedRequestInput<'_>,
    ) -> Result<AuthorDerivedAuthorizedOutcome, ProtocolError> {
        let mut outcomes = self.author_derived_all(std::iter::once(req)).await?;
        outcomes.pop().ok_or_else(|| {
            ProtocolError::internal("author_derived_all returned no outcome for one request")
        })
    }

    /// Authorize and persist many derived memories in one transaction.
    ///
    /// Every text is embedded (or deferred for a refused input) **before**
    /// [`crate::storage_ports::WriteSessionFactory::begin`]. Use this for a self-referential
    /// group (code slices of one file) so the pool slot is held only for
    /// the writes.
    ///
    /// # Errors
    ///
    /// Authorization or storage faults from any member. No row is
    /// committed unless the caller [`Self::commit`]s.
    pub async fn author_derived_all(
        &mut self,
        reqs: impl IntoIterator<Item = AuthorDerivedRequestInput<'_>>,
    ) -> Result<Vec<AuthorDerivedAuthorizedOutcome>, ProtocolError> {
        let reqs: Vec<AuthorDerivedRequestInput<'_>> = reqs.into_iter().collect();
        if reqs.is_empty() {
            return Ok(Vec::new());
        }
        let mut prepared = Vec::with_capacity(reqs.len());
        for req in reqs {
            prepared.push(self.prepare_derived(req).await?);
        }
        let mut outcomes = Vec::with_capacity(prepared.len());
        for item in prepared {
            outcomes.push(self.write_prepared_derived(item).await?);
        }
        Ok(outcomes)
    }

    async fn prepare_derived(
        &mut self,
        req: AuthorDerivedRequestInput<'_>,
    ) -> Result<PreparedDerived, ProtocolError> {
        let write_permit = self
            .engine
            .authorize_write(self.authz, &req.owner, Relation::Editor)
            .await?;
        let owner = *write_permit.owner();
        if let Some(prior) = req.supersedes {
            let prior_home = self
                .engine
                .storage()
                .memory_authoring
                .owner_access_read
                .home_owner(crate::EntityId::Memory(prior))
                .await
                .map_err(|err| ProtocolError::internal(err.to_string()))?;
            if prior_home.as_ref() != Some(&owner) {
                return Err(ProtocolError::forbidden(
                    "supersedes target is not an owned entity of the same owner",
                ));
            }
            let prior_kind = self.engine.load_required_memory_kind(&owner, prior).await?;
            if prior_kind != req.kind {
                return Err(ProtocolError::invalid_argument(
                    "supersedes",
                    "must supersede a memory of the same kind",
                ));
            }
        }
        let source = EdgeEndpoint::memory(req.kind, req.memory_id);
        let origins = self
            .engine
            .authorized_index_targets_visible(
                self.authz,
                source,
                req.derived_from,
                "derived_from",
                &self.written,
            )
            .await?;
        let declared = req.sidecar_payload.references();
        let mut references = self
            .engine
            .authorized_payload_references_visible(self.authz, source, &declared, &self.written)
            .await?;
        if !req.extra_refs.is_empty() {
            let extras: Vec<EdgeEndpoint> = req
                .extra_refs
                .iter()
                .copied()
                .map(|id| EdgeEndpoint::memory(EntityKind::Fact, id))
                .collect();
            let extra = self
                .engine
                .authorized_index_targets_visible(
                    self.authz,
                    source,
                    &extras,
                    "refs",
                    &self.written,
                )
                .await?;
            references.extend(extra);
        }
        super::memory_authoring::validate_operator_memory_invocation_request(&req)
            .map_err(super::memory_authoring::map_derived_storage_error)?;
        let embedding = self
            .prepare_embedding(req.memory_id, req.schema_id.as_str(), &req.text)
            .await
            .map_err(super::memory_authoring::map_derived_storage_error)?;
        Ok(PreparedDerived {
            write_permit,
            owner,
            memory_id: req.memory_id,
            kind: req.kind,
            text: req.text,
            schema_id: req.schema_id,
            schema_version: req.schema_version,
            operator_kind: req.operator_kind,
            model_id: req.model_id.to_owned(),
            sidecar_payload: req.sidecar_payload,
            supersedes: req.supersedes,
            lexical_language: req.lexical_language.map(ToOwned::to_owned),
            embedding,
            origins,
            references,
        })
    }

    async fn prepare_embedding(
        &mut self,
        memory_id: MemoryId,
        schema_id: &str,
        text: &str,
    ) -> Result<PreparedEmbedding, crate::storage::StorageError> {
        let client = self.engine.embed_client();
        let Some(client) = client.as_deref() else {
            return Ok(PreparedEmbedding::None);
        };
        // A deferral is a promise the drain can keep. It cannot for a schema
        // whose recipe resolves to no unit: the job would be claimed, find no
        // text and be dropped, once per memory.
        if !self.engine.registry().schema_is_embeddable(schema_id) {
            return Ok(PreparedEmbedding::None);
        }
        // Transaction already open: do not hold the pool slot across HTTP.
        if self.session.is_some() {
            return Ok(PreparedEmbedding::Deferred {
                model_id: client.model_id().to_owned(),
            });
        }
        Ok(
            match super::memory_authoring::resolve_derived_embedding(client, memory_id, text)
                .await?
            {
                DerivedEmbedding::None => PreparedEmbedding::None,
                DerivedEmbedding::Ready { model_id, vector } => PreparedEmbedding::Ready {
                    model_id: model_id.to_owned(),
                    vector,
                },
                DerivedEmbedding::Deferred { model_id } => PreparedEmbedding::Deferred {
                    model_id: model_id.to_owned(),
                },
            },
        )
    }

    async fn write_prepared_derived(
        &mut self,
        item: PreparedDerived,
    ) -> Result<AuthorDerivedAuthorizedOutcome, ProtocolError> {
        let embedding = item.embedding.as_derived();
        let storage_req = AuthorDerivedRequest {
            memory_id: item.memory_id,
            owner: item.owner,
            kind: item.kind,
            text: item.text,
            schema_id: item.schema_id,
            schema_version: item.schema_version,
            operator_kind: item.operator_kind,
            model_id: &item.model_id,
            sidecar_payload: item.sidecar_payload,
            supersedes: item.supersedes,
            lexical_language: item.lexical_language.as_deref(),
            embedding,
            origins: &item.origins,
            references: &item.references,
        };
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
        }
        Ok(AuthorDerivedAuthorizedOutcome {
            memory_id: outcome.memory_id,
            idempotent_replay: outcome.idempotent_replay,
            edge_count: outcome.edge_count,
            embedding_deferred: outcome.embedding_deferred,
        })
    }

    /// Authorize and persist one Goal create in this transaction.
    ///
    /// `write_act_t` attaches this episode's write-act (`Goal.write_act_t`).
    /// Replay of a bound Goal is rejected by the episode protocol, not here.
    ///
    /// # Errors
    ///
    /// Authorization or storage faults.
    pub async fn create_goal(
        &mut self,
        req: super::GoalCreatePayloadWriteRequest,
        write_act_t: Option<MemoryId>,
    ) -> Result<GoalWriteOutcome, ProtocolError> {
        let permit = self
            .engine
            .authorize_write(self.authz, &req.owner, Relation::Editor)
            .await?;
        let payload = self.engine.normalize_payload_write(req.payload.clone())?;
        let draft = GoalDraft::active_from_payload_write(
            *permit.owner(),
            payload,
            req.topology.clone(),
            req.wake.clone(),
            req.authorship.clone(),
            req.request_id.clone(),
        );
        let embedding_client = self.engine.embed_client();
        let context = self
            .engine
            .goal_atomic_context(embedding_client.as_ref(), req.author_self_perspective_id);
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
                self.authz,
                permit.owner(),
                &req.topology,
                &self.written,
            )
            .await?;
        self.engine
            .author_self_perspective_authorized(self.authz, req.author_self_perspective_id)
            .await?;
        self.engine
            .validate_wake_config_for_write(self.authz, req.wake.as_ref())
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
            .authorize_write(self.authz, &owner, Relation::Editor)
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

impl Drop for UnitOfWork<'_> {
    fn drop(&mut self) {
        self.session.take();
    }
}

async fn owned_fact_series_handle<P: FactPayload>(
    engine: &Engine,
    authz: &AuthzContext,
    payload: &P,
) -> Result<Option<uuid::Uuid>, ProtocolError> {
    let Some(table) = P::sidecar_table() else {
        return Ok(None);
    };
    let columns = P::natural_key_columns();
    if columns.is_empty() {
        return Ok(None);
    }
    let owner = engine.single_write_owner_for(authz, Relation::Editor)?;
    let atoms = SidecarAtom::bind_columns(payload, columns)
        .map_err(|err| ProtocolError::invalid_argument("natural_key", err))?;
    let binds = atoms
        .iter()
        .map(|(column, value)| (column.as_str(), value.clone()))
        .collect::<Vec<_>>();
    engine
        .owned_series_handle(authz, owner, &P::schema_id(), table, &binds)
        .await
}

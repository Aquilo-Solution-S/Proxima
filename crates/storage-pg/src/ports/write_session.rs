use std::sync::Arc;

use proxima_core::storage_ports::{
    HostStateParticipantDescriptor, HostStateReply, HostStateRequest, HostStateWritePermit,
    OwnerWritePermit, SidecarSessionRead, WriteSession, WriteSessionFactory,
};
use proxima_core::verbs::fact_ingest::{AuthorizedFactWrite, FactIngestOutcome};
use proxima_core::verbs::goal_write::{
    CreateGoalAtomicRequest, GoalReplayOutcome, GoalReplayRequest, GoalWriteOutcome,
};
use proxima_core::verbs::query::SidecarAtom;
use proxima_core::{
    AuthorDerivedOutcome, AuthorDerivedRequest, ColdObjectStore, GroupId, MembershipRow, MemoryId,
    Role, StorageError, UserId,
};
use sqlx::{Postgres, Transaction};

use crate::access::owner_columns::PgAccessCeiling;
use crate::error::internal;
use crate::pg_enums::PgMemoryKind;
use crate::sidecars::PgSidecarRegistryFrozen;
use crate::{PgStorage, verbs};

/// The live transaction of one admitted host-state callback.
///
/// Only the write session constructs this value, after checking the registered
/// participant and its tables. [`Self::authority_memberships`] reads the
/// authenticated subject stamped on that callback's permit; it takes no
/// caller-selected subject or owner scope. Host-owned SQL uses
/// [`Self::transaction`] and continues to stamp the permit's owner.
///
/// ```
/// use proxima_storage_pg::PgHostStateSession;
/// fn inspect(session: &PgHostStateSession<'_, '_>) { let _ = session; }
/// ```
///
/// ```compile_fail
/// # use proxima_core::storage_ports::HostStateWritePermit;
/// # use proxima_storage_pg::PgHostStateSession;
/// # use sqlx::{Postgres, Transaction};
/// # fn forge<'session, 'connection>(tx: &'session mut Transaction<'connection, Postgres>, permit: &'session HostStateWritePermit) {
/// PgHostStateSession { tx, permit };
/// # }
/// ```
#[derive(Debug)]
pub struct PgHostStateSession<'session, 'connection> {
    tx: &'session mut Transaction<'connection, Postgres>,
    permit: &'session HostStateWritePermit,
}

impl<'connection> PgHostStateSession<'_, 'connection> {
    /// The unit's transaction for SQL against the callback's declared host tables.
    pub fn transaction(&mut self) -> &mut Transaction<'connection, Postgres> {
        self.tx
    }

    /// Read the authenticated subject's current memberships on this unit.
    ///
    /// The database holds the subject's membership projection fence until the
    /// unit commits or rolls back. This grants no membership writes or memory
    /// owner reach and requires a `READ COMMITTED` owner transaction.
    ///
    /// # Errors
    ///
    /// Refuses a permit without an authenticated owner scope, an expired scope,
    /// or a transaction that does not match the scope. Database and invalid-role
    /// errors are returned as storage faults.
    pub async fn authority_memberships(&mut self) -> Result<SubjectMemberships, StorageError> {
        let scope = self.permit.authority_read_scope().ok_or_else(|| {
            StorageError::ConstraintViolation(
                "host-state authority reads require an authenticated owner scope".into(),
            )
        })?;
        if scope.is_expired() {
            return Err(StorageError::ConstraintViolation(
                "host-state authority read scope has expired".into(),
            ));
        }
        let subject = scope.subject();
        let rows = sqlx::query_as::<_, (uuid::Uuid, PgAccessCeiling, PgAccessCeiling, bool)>(
            "SELECT group_id, read_ceiling, write_ceiling, manage
               FROM proxima_core.read_subject_authority($1)",
        )
        .bind(subject.into_inner())
        .fetch_all(&mut **self.tx)
        .await
        .map_err(crate::error::map_err)?;
        if scope.is_expired() {
            return Err(StorageError::ConstraintViolation(
                "host-state authority read scope has expired".into(),
            ));
        }
        let memberships = rows
            .into_iter()
            .map(|(group, read, write, manage)| {
                let role = Role::new(read.into(), write.into(), manage).map_err(|error| {
                    StorageError::ConstraintViolation(format!(
                        "group role projection contains an invalid role: {error}"
                    ))
                })?;
                Ok(MembershipRow {
                    group: GroupId::new(group),
                    role,
                })
            })
            .collect::<Result<Vec<_>, StorageError>>()?;
        Ok(SubjectMemberships {
            subject,
            memberships,
        })
    }
}

/// A read-only snapshot of one authenticated subject's memberships.
///
/// Returned only by [`PgHostStateSession::authority_memberships`]. The read
/// fence belongs to that unit of work and lasts until commit or rollback.
#[derive(Debug)]
pub struct SubjectMemberships {
    subject: UserId,
    memberships: Vec<MembershipRow>,
}

impl SubjectMemberships {
    /// The authenticated subject whose memberships were resolved.
    #[must_use]
    pub const fn subject(&self) -> UserId {
        self.subject
    }

    /// Every current group membership of [`Self::subject`].
    #[must_use]
    pub fn memberships(&self) -> &[MembershipRow] {
        &self.memberships
    }
}

struct PgWriteSession {
    store_number: u16,
    tx: Transaction<'static, Postgres>,
    sidecars: PgSidecarRegistryFrozen,
    surfaces: proxima_core::owner_inverse::OwnerSurfaces,
    /// The flavor-declared lifecycle scopes, so this session can resolve a
    /// payload's `ScopeRef` into the fence key and liveness probe without
    /// reaching back through the storage handle mid-transaction.
    scopes: crate::access::scope_surfaces::ScopeSurfaces,
    cold: Arc<dyn ColdObjectStore>,
    host_state: Option<crate::RegisteredHostStateParticipant>,
    /// Set only on a session from `begin_series_erase`.
    series_erase: Option<SeriesEraseState>,
}

/// What a series-erase session needs beyond an ordinary one: the boot's
/// erase context, the owner its fences were taken for, and — once the erase
/// ran — the cold debt `commit` settles.
struct SeriesEraseState {
    context: crate::PgHostStateEraseContext,
    owner: proxima_core::OwnerId,
    pool: sqlx::PgPool,
    platform: crate::PgPlatformScope,
    ran: bool,
    cold_purge: verbs::forget::ColdPurgePlan,
}

#[async_trait::async_trait]
impl WriteSessionFactory for PgStorage {
    fn host_state_descriptor(&self) -> Option<HostStateParticipantDescriptor> {
        self.host_state
            .as_ref()
            .map(|registered| registered.descriptor)
    }

    async fn begin(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
    ) -> Result<Box<dyn WriteSession>, StorageError> {
        let tx = crate::owner_scope::begin_owner_transaction(
            &self.pool,
            crate::owner_scope::require_owner_scope(owner_scope)?,
        )
        .await?;
        self.session_from_transaction(tx).await
    }

    async fn begin_host_state(
        &self,
        permit: &HostStateWritePermit,
    ) -> Result<Box<dyn WriteSession>, StorageError> {
        let descriptor = self.host_state_descriptor().ok_or_else(|| {
            proxima_core::StorageError::ConstraintViolation(
                "host-state participant is not registered".into(),
            )
        })?;
        if !matches!(
            permit.origin(),
            proxima_core::storage_ports::HostStateWriteOrigin::Maintenance
        ) || permit.participant_id() != descriptor.participant_id()
            || permit.tables() != descriptor.tables()
        {
            return Err(proxima_core::StorageError::ConstraintViolation(
                "host-state maintenance scope mismatch".into(),
            ));
        }
        let tx = self.platform_transaction().await?;
        self.session_from_transaction(tx).await
    }

    async fn begin_series_erase(
        &self,
        permit: &OwnerWritePermit,
    ) -> Result<Box<dyn WriteSession>, StorageError> {
        if permit.access_kind() != proxima_core::AccessKind::Goal {
            return Err(StorageError::ConstraintViolation(
                "a series erase requires an Admin-level owner permit".into(),
            ));
        }
        let context = self.host_state_erase_context()?;
        // Platform scope: the erase reaches captured publication records by
        // `t` across original owners, which owner RLS would filter out
        // silently. The owner still comes from the sealed permit.
        let mut tx = self.owner_maintenance_transaction(permit).await?;
        // Bounded waits from the first lock on, as the owner erase does: a
        // lock this erase cannot get is a 55P03 in five seconds, which the
        // caller retries, not a five-minute stall behind a writer.
        sqlx::query("SET LOCAL lock_timeout = '5s'")
            .execute(&mut *tx)
            .await
            .map_err(crate::error::map_err)?;
        // The exclusive lifecycle fence is the transaction's first lock and
        // the owner fence its second: an ordinary unit holds the former
        // shared from entry, and a transfer takes the latter exclusively
        // before it moves a series, so the footprint below is one ownership
        // snapshot.
        crate::access::owner_columns::lock_host_lifecycle_fence_exclusive_tx(&mut tx).await?;
        crate::access::owner_columns::lock_owner_fence_shared_tx(&mut tx, permit.owner()).await?;
        Ok(Box::new(PgWriteSession {
            store_number: self.store_number,
            tx,
            sidecars: self.sidecars.clone(),
            surfaces: self.surfaces.clone(),
            scopes: self.scopes.clone(),
            cold: Arc::clone(&self.cold),
            host_state: self.host_state.clone(),
            series_erase: Some(SeriesEraseState {
                context,
                owner: *permit.owner(),
                pool: self.pool.clone(),
                platform: self.platform_scope.clone(),
                ran: false,
                cold_purge: verbs::forget::ColdPurgePlan::default(),
            }),
        }))
    }
}

impl PgStorage {
    async fn session_from_transaction(
        &self,
        mut tx: Transaction<'static, Postgres>,
    ) -> Result<Box<dyn WriteSession>, StorageError> {
        if !self.surfaces.host_lifecycle_surfaces().is_empty() {
            // This global shared xact fence is the first lock in a host-capable
            // UoW, before a cognitive method can take owner/handle locks.
            crate::access::owner_columns::lock_host_lifecycle_fence_shared_tx(&mut tx).await?;
        }
        Ok(Box::new(PgWriteSession {
            store_number: self.store_number,
            tx,
            sidecars: self.sidecars.clone(),
            surfaces: self.surfaces.clone(),
            scopes: self.scopes.clone(),
            cold: Arc::clone(&self.cold),
            host_state: self.host_state.clone(),
            series_erase: None,
        }))
    }
}

#[async_trait::async_trait]
impl WriteSession for PgWriteSession {
    async fn advisory_xact_lock(&mut self, key: i64) -> Result<(), StorageError> {
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(key)
            .execute(&mut *self.tx)
            .await
            .map_err(crate::error::map_err)?;
        Ok(())
    }

    async fn advisory_xact_lock_shared(&mut self, key: i64) -> Result<(), StorageError> {
        sqlx::query("SELECT pg_advisory_xact_lock_shared($1)")
            .bind(key)
            .execute(&mut *self.tx)
            .await
            .map_err(crate::error::map_err)?;
        Ok(())
    }

    async fn read_own_sidecar(
        &mut self,
        permit: &OwnerWritePermit,
        read: &SidecarSessionRead<'_>,
    ) -> Result<Vec<serde_json::Value>, StorageError> {
        crate::sidecars::read_own_sidecar_in_tx(
            &mut self.tx,
            &self.sidecars,
            &self.surfaces,
            permit,
            read,
        )
        .await
    }

    async fn owned_series_head_memory_id(
        &mut self,
        permit: &OwnerWritePermit,
        schema_id: &proxima_core::SchemaId,
        sidecar_table: &str,
        columns: &[(&str, SidecarAtom)],
    ) -> Result<Option<MemoryId>, StorageError> {
        // The declared memory-key column IS the registration test: only a
        // registered memory sidecar has one, and the lookup's join needs it.
        let Some(key_column) = self.sidecars.memory_key_column(sidecar_table) else {
            return Err(StorageError::ConstraintViolation(format!(
                "session series-head lookup names {sidecar_table}, which is not a registered \
                 memory sidecar table; register the payload with `pg_sidecar!` so the surface \
                 is declared"
            )));
        };
        let head = verbs::query::owned_head_memory_id(
            &mut *self.tx,
            *permit.owner(),
            schema_id,
            sidecar_table,
            key_column,
            columns,
        )
        .await?;
        Ok(head.map(MemoryId::new))
    }

    async fn ingest_fact_with_typed_sidecar(
        &mut self,
        authorized: &AuthorizedFactWrite,
        embedding_spaces: &[proxima_core::EmbeddingSpace],
    ) -> Result<FactIngestOutcome, StorageError> {
        let sidecar_payloads = authorized.sidecar_payloads();
        let fact_sidecars = self.sidecars.writing(authorized.draft());
        let payloads = sidecar_payloads.to_vec();
        let content_payloads = payloads.clone();
        let tables = self.sidecars.tables_for_payloads(sidecar_payloads)?;
        let natural_key = verbs::fact_ingest::fact_natural_key(
            authorized.draft(),
            authorized.fact_sidecar_table(),
            authorized.fact_natural_key_columns(),
            authorized.fact_natural_key_values(),
            authorized.reobserves_displaced(),
            sidecar_payloads,
            &self.sidecars,
        )?;
        // The declared scopes come off the PAYLOADS, not off the caller: a
        // host writing a scoped payload straight through `Engine` reaches
        // this same line, and the fence it takes is the one the flavor's
        // erase holds.
        let scopes = self.scopes.targets_for_payloads(sidecar_payloads)?;
        let outcome = verbs::fact_ingest::ingest_fact_with_sidecar_in_tx(
            &mut self.tx,
            authorized,
            embedding_spaces,
            verbs::fact_ingest::FactAdmissionInput {
                store_number: self.store_number,
                natural_key: natural_key.as_ref(),
                sidecar_tables: &tables,
                scopes: &scopes,
                content: verbs::fact_ingest::ContentResolution {
                    content_id: None,
                    payloads: Some(&content_payloads),
                },
                publication: authorized.publication(),
            },
            move |tx, outcome| {
                Box::pin(async move {
                    for payload in &payloads {
                        fact_sidecars
                            .insert_memory_sidecar(tx, outcome.memory_id, payload)
                            .await?;
                    }
                    Ok(())
                })
            },
        )
        .await?;
        if !outcome.idempotent_replay {
            let kind = PgMemoryKind::from(authorized.draft().kind);
            verbs::sketch::upsert_sketch(
                &mut self.tx,
                authorized.owner_write_permit().owner().into_inner(),
                outcome.memory_id.into_inner(),
                kind.into(),
                &verbs::sketch::sketch_line(
                    kind.into(),
                    authorized.draft().rendered_text.as_deref(),
                    sidecar_payloads,
                ),
            )
            .await?;
        }
        Ok(outcome)
    }

    async fn author_derived(
        &mut self,
        req: &AuthorDerivedRequest<'_>,
        permit: &OwnerWritePermit,
    ) -> Result<AuthorDerivedOutcome, StorageError> {
        let draft = verbs::derive_append::DerivedDraft::from_request(req, self.store_number);
        verbs::derive_append::validate_derived_origins_in_tx(
            &mut self.tx,
            &draft,
            req.origins,
            req.references,
        )
        .await?;
        verbs::derive_append::validate_derived_reference_kinds_in_tx(
            &mut self.tx,
            &draft,
            req.origins,
            req.references,
        )
        .await?;
        let sidecars = self.sidecars.writing_derived(&draft);
        let sidecar_payload = req.sidecar_payload.clone();
        let content_payload = sidecar_payload.clone();
        let tables = self
            .sidecars
            .tables_for_payloads(std::slice::from_ref(&sidecar_payload))?;
        let scopes = self
            .scopes
            .targets_for_payloads(std::slice::from_ref(&sidecar_payload))?;
        let outcome = verbs::derive_append::append_derived_with_content_payloads_in_tx(
            &mut self.tx,
            permit,
            &draft,
            verbs::derive_append::DerivedAdmissionInput {
                origins: req.origins,
                references: req.references,
                sidecar_tables: &tables,
                scopes: &scopes,
                content: verbs::derive_append::ContentResolution {
                    content_id: None,
                    payloads: Some(std::slice::from_ref(&content_payload)),
                },
            },
            move |tx, outcome| {
                Box::pin(async move {
                    sidecars
                        .insert_memory_sidecar(tx, outcome.memory_id, &sidecar_payload)
                        .await
                })
            },
        )
        .await?;
        if !outcome.idempotent_replay {
            verbs::sketch::upsert_sketch(
                &mut self.tx,
                permit.owner().into_inner(),
                outcome.memory_id.into_inner(),
                req.kind.into(),
                &verbs::sketch::sketch_line(
                    req.kind.into(),
                    Some(req.text.as_str()),
                    std::slice::from_ref(&req.sidecar_payload),
                ),
            )
            .await?;
        }
        let edge_count = verbs::derive_append::assert_derived_index_rows(
            &mut self.tx,
            &draft,
            &outcome,
            req.origins,
            req.references,
        )
        .await?;
        let edge_count = edge_count.saturating_add(
            verbs::derive_append::record_external_edges(
                &mut self.tx,
                &outcome,
                req.external_origins,
                req.external_references,
            )
            .await?,
        );
        Ok(AuthorDerivedOutcome {
            memory_id: outcome.memory_id,
            idempotent_replay: outcome.idempotent_replay,
            forgotten: outcome.forgotten,
            edge_count,
            embedding_deferred: !req.queued_spaces.is_empty() && !outcome.idempotent_replay,
        })
    }

    async fn resolve_goal_replay(
        &mut self,
        req: GoalReplayRequest<'_, '_>,
        permit: &OwnerWritePermit,
    ) -> Result<Option<GoalReplayOutcome>, StorageError> {
        super::validate_permit_owner(permit, &req.owner())?;
        verbs::goal_write::resolve_goal_command_replay_on(self.tx.as_mut(), req).await
    }

    async fn create_goal(
        &mut self,
        req: &CreateGoalAtomicRequest<'_>,
        permit: &OwnerWritePermit,
    ) -> Result<GoalWriteOutcome, StorageError> {
        verbs::goal_write::create_goal_in_tx(
            &mut self.tx,
            &self.sidecars,
            req,
            permit,
            self.store_number,
        )
        .await
    }

    async fn apply_host_state(
        &mut self,
        permit: &HostStateWritePermit,
        request: HostStateRequest,
    ) -> Result<HostStateReply, StorageError> {
        if !permit.matches_request(&request) {
            return Err(StorageError::ConstraintViolation(
                "host-state request exceeds the participant or table scope stamped by the engine"
                    .into(),
            ));
        }
        let Some(registered) = self.host_state.clone() else {
            return Err(StorageError::ConstraintViolation(
                "no host-state participant is registered".into(),
            ));
        };
        let descriptor = registered.descriptor;
        if descriptor.participant_id() != request.participant_id() {
            return Err(StorageError::ConstraintViolation(format!(
                "host-state participant {} is not registered",
                request.participant_id().as_str()
            )));
        }
        for table in request.tables() {
            if !descriptor.tables().contains(table) {
                return Err(StorageError::ConstraintViolation(format!(
                    "host-state command names {}, which this participant does not declare",
                    table.as_str()
                )));
            }
        }
        crate::access::owner_columns::lock_owner_fence_shared_tx(&mut self.tx, permit.owner())
            .await?;
        registered
            .participant
            .apply(
                &mut PgHostStateSession {
                    tx: &mut self.tx,
                    permit,
                },
                permit,
                request,
            )
            .await
    }

    async fn forget_memory(
        &mut self,
        permit: &OwnerWritePermit,
        memory_id: MemoryId,
    ) -> Result<(), StorageError> {
        let owner = permit.owner();
        let owner_id = owner.into_inner();
        let t = memory_id.into_inner();
        // Ownership and kind precondition: a `t` the caller does not own, or
        // of a kind outside the permit, is NotFound rather than a forget on
        // someone else's row. It runs in owner scope, before the forget
        // enters the lifecycle scope, so it reads live rows only: forgetting
        // a forgotten memory is NotFound here.
        sqlx::query_scalar::<_, uuid::Uuid>(
            "SELECT handle FROM proxima_core.memory /* FORGET-POLICY: live-only (owner-scope precondition; a forgotten t is NotFound) */
              WHERE t = $1 AND owner_id = $2 AND kind::text = ANY($3) AND forgotten_at IS NULL",
        )
        .bind(t)
        .bind(owner_id)
        .bind(PgMemoryKind::labels(&verbs::forget::kinds_within(
            permit.access_kind(),
        )))
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(internal)?
        .ok_or(StorageError::NotFound)?;
        verbs::forget::forget_memory(&mut self.tx, &self.sidecars, &self.surfaces, t, owner_id)
            .await
    }

    async fn erase_series(
        &mut self,
        permit: &OwnerWritePermit,
        request: &proxima_core::verbs::own_erase::SeriesEraseRequest<'_>,
    ) -> Result<proxima_core::verbs::own_erase::SeriesEraseOutcome, StorageError> {
        let Some(state) = self.series_erase.as_mut() else {
            return Err(StorageError::ConstraintViolation(
                "a series erase runs only on a session from begin_series_erase".into(),
            ));
        };
        if state.ran || *permit.owner() != state.owner {
            return Err(StorageError::ConstraintViolation(
                "a series-erase session erases once, for the owner it was opened for".into(),
            ));
        }
        state.ran = true;
        let (outcome, cold_purge) = verbs::series_erase::erase_series(
            &mut self.tx,
            &self.sidecars,
            &state.context,
            permit.owner(),
            request,
        )
        .await?;
        state.cold_purge = cold_purge;
        Ok(outcome)
    }

    async fn commit(self: Box<Self>) -> Result<(), StorageError> {
        let session = *self;
        crate::write_wake::commit(session.tx).await?;
        // After commit and never before: a rollback after an object delete
        // would restore the upload row over destroyed bytes. A failed
        // purge leaves its `cold_purge_pending` row for the retry lane.
        if let Some(state) = session.series_erase {
            let outcome = verbs::forget::purge_cold_objects_after_commit_with_platform(
                &state.pool,
                &state.platform,
                session.cold.as_ref(),
                &state.cold_purge,
            )
            .await;
            if outcome.failed > 0 || outcome.pending {
                tracing::warn!(
                    attempted = outcome.attempted,
                    failed = outcome.failed,
                    "series erase committed; blob objects remain queued for \
                     maintain-storage --retry-cold-object-purges"
                );
            }
        }
        Ok(())
    }
}

//! Postgres `Storage` impl.
//!
//! See docs/07-storage.md and the `Storage` trait in
//! `proxima_core`.
//!
//! The verb logic lives under [`verbs`]; this module wires the
//! `PgStorage` struct, connection lifecycle, migration runner, and
//! outbox plumbing, then delegates each `Storage` trait method to its
//! per-verb implementation.

use std::sync::Arc;
use std::time::Duration;

use proxima_core::personality::{
    AbstractionRow, ActiveGoalSummary, ChangeEventForWake, InstantiatePersonalityRequest,
    InstantiatePersonalityResponse, ListReadScopeRequest, ListReadScopeResponse,
    ListWakeInvocationsRequest, MemorySnapshot, PersonalityInstanceId, PersonalityInstanceRow,
    PersonalityRef, PersonalityRuntimeRow, PersonalityWriteOutcome, PersonalityWriteRequest,
    RootPersonalityPerspectiveRow, SetReadScopeRequest, SetReadScopeResponse,
    SetWakeEntriesRequest, SetWakeEntriesResponse, SidecarSpec, TombstonePersonalityRequest,
    TombstonePersonalityResponse, WakeDispatchEntryRow, WakeInvocationFinalize,
    WakeInvocationLogDraft, WakeInvocationRow, WakeInvocationStart, WakeInvocationStatus,
};
use proxima_core::storage::WakeLockGuard;
use proxima_core::verbs::close_batch::CloseBatchOutcome;
use proxima_core::verbs::event_history::{EventHistoryRequest, EventHistoryResponse};
use proxima_core::verbs::event_ingest::{EventDraft, EventIngestOutcome};
use proxima_core::verbs::goal_write::{GoalDraft, GoalWriteOutcome};
use proxima_core::verbs::persist_wake_trace::{WakeTracePersistInput, WakeTracePersistOutcome};
use proxima_core::verbs::query::{
    MemoryLineageRequest, MemoryLineageResponse, MemorySearchRequest, MemorySearchResult,
    QueryRequest, QueryResponse,
};
use proxima_core::verbs::subscribe::ChangeEventStream;
use proxima_core::{
    BindInferenceTierRequest, BindInferenceTierResponse, BlockedWakeCandidate, ChangeEvent, GoalId,
    InferenceTargetRow, InferenceTierBindingRow, MasterTokenPersonality, MemoryDependency,
    MemoryId, ModelTier, Owner, RegisterInferenceTargetRequest, RegisterInferenceTargetResponse,
    RemoveInferenceTargetRequest, RemoveInferenceTargetResponse, SourceBatchId, Storage,
    StorageError, StorageHandle,
};
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use tokio::sync::broadcast;

mod authorship;
mod error;
pub mod outbox;
mod personality_locks;
mod pg_ident;
pub mod query {
    pub use crate::verbs::query::MAX_SNAPSHOT_EDGES;
}
pub mod settings;
pub mod verbs;

use outbox::BROADCAST_CAPACITY;

/// Default DB URL when `DATABASE_URL` is unset. Matches the
/// dev DB created locally via `createdb proxima_dev`.
pub const DEFAULT_DATABASE_URL: &str = "postgres://postgres@localhost/proxima_dev";

#[derive(Debug, Clone)]
pub struct PgStorage {
    pool: PgPool,
    tx: broadcast::Sender<ChangeEvent>,
}

impl PgStorage {
    /// Connect using `url`, build a tuned pool, and verify
    /// connectivity by acquiring one connection.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::Unavailable` on connection or
    /// query failure.
    pub async fn connect(url: &str) -> Result<Self, StorageError> {
        let opts: PgConnectOptions = url.parse().map_err(|e: sqlx::Error| {
            StorageError::Unavailable(format!("invalid DATABASE_URL: {e}"))
        })?;
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .acquire_timeout(Duration::from_secs(5))
            .connect_with(opts)
            .await
            .map_err(|e| StorageError::Unavailable(e.to_string()))?;

        // Validate connectivity with a trivial query.
        sqlx::query!("SELECT 1 AS one")
            .fetch_one(&pool)
            .await
            .map_err(|e| StorageError::Unavailable(e.to_string()))?;

        let tx = broadcast::channel(BROADCAST_CAPACITY).0;

        Ok(Self { pool, tx })
    }

    /// Read `DATABASE_URL` from env, fallback to
    /// `DEFAULT_DATABASE_URL`. Convenience for the bin / dev.
    #[must_use]
    pub fn url_from_env() -> String {
        std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_string())
    }

    #[must_use]
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    #[must_use]
    pub fn into_handle(self) -> StorageHandle {
        Arc::new(self)
    }

    /// Return a fresh broadcast receiver for `ChangeEvents`.
    /// Multiple calls produce independent receivers that each
    /// see all future events.
    #[must_use]
    pub fn changes(&self) -> broadcast::Receiver<ChangeEvent> {
        self.tx.subscribe()
    }

    /// Spawn the outbox publisher task and await its first
    /// successful LISTEN bind + backfill drain.
    ///
    /// Opens a `PgListener` on the same pool, LISTENs on
    /// `outbox::NOTIFY_CHANNEL`, drains anything currently in
    /// `change_event` to the broadcast channel, and only then
    /// returns. Subsequent reconnects (on listener error) carry the
    /// `last_seen_seq` watermark forward and do not re-signal.
    ///
    /// Awaiting readiness closes the boot race where a write
    /// committing before `LISTEN` bound would have its `pg_notify`
    /// silently dropped (`PostgreSQL` discards notifications for
    /// sessions not `LISTEN`ing at `COMMIT` time).
    ///
    /// # Errors
    ///
    /// `StorageError::Internal` if the publisher exits before
    /// signaling ready, or if the initial LISTEN / backfill fails.
    pub async fn start_outbox(&self) -> Result<(), StorageError> {
        let pool = self.pool.clone();
        let tx = self.tx.clone();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            outbox::outbox_publisher(pool, tx, Some(ready_tx)).await;
        });
        match ready_rx.await {
            Ok(result) => result,
            Err(_) => Err(StorageError::Internal(
                "outbox publisher exited before signaling ready".into(),
            )),
        }
    }

    /// Apply all pending migrations under
    /// `crates/storage-pg/migrations/`. Idempotent — sqlx tracks
    /// applied migrations in `_sqlx_migrations`. Call once
    /// at process start before any verb dispatch.
    ///
    /// `ignore_missing = true` matches the per-flavor migrator
    /// (`flavors/*/migrations.rs`): core and every flavor share the
    /// default `_sqlx_migrations` table, so on a second run the core
    /// migrator sees flavor-authored versions it doesn't know about.
    /// Without this relaxation the second run fails with
    /// `VersionMissing(<flavor version>)`. The core version-set is
    /// still validated; we only relax the cross-author check.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::Internal` on any sqlx
    /// migration failure (broken file, conflict with the
    /// recorded checksum, etc.).
    pub async fn run_migrations(&self) -> Result<(), StorageError> {
        let mut m = sqlx::migrate!("./migrations");
        m.set_ignore_missing(true);
        m.run(&self.pool)
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl Storage for PgStorage {
    async fn ingest_event_atomic(
        &self,
        draft: &EventDraft,
    ) -> Result<EventIngestOutcome, StorageError> {
        verbs::event_ingest::ingest_event_atomic(&self.pool, draft).await
    }

    async fn persist_wake_trace_atomic(
        &self,
        registry: &proxima_core::FlavorRegistryFrozen,
        input: &WakeTracePersistInput,
    ) -> Result<WakeTracePersistOutcome, StorageError> {
        verbs::persist_wake_trace::persist_wake_trace_atomic(&self.pool, registry, input).await
    }

    async fn persist_intervention_requested_atomic(
        &self,
        registry: &proxima_core::FlavorRegistryFrozen,
        input: &proxima_core::InterventionRequestPersistInput,
    ) -> Result<proxima_core::InterventionRequestPersistOutcome, StorageError> {
        verbs::persist_intervention_request::persist_intervention_requested_atomic(
            &self.pool, registry, input,
        )
        .await
    }

    async fn persist_core_workspace_run_atomic(
        &self,
        registry: &proxima_core::FlavorRegistryFrozen,
        input: &proxima_core::CoreWorkspaceRunPersistInput,
    ) -> Result<proxima_core::CoreWorkspaceRunPersistOutcome, StorageError> {
        verbs::persist_core_workspace_run::persist_core_workspace_run_atomic(
            &self.pool, registry, input,
        )
        .await
    }

    async fn load_intervention_continue_candidate(
        &self,
        owner: &Owner,
        decision_memory_id: MemoryId,
    ) -> Result<Option<proxima_core::InterventionContinueCandidate>, StorageError> {
        verbs::consolidate::load_intervention_continue_candidate(
            &self.pool,
            owner,
            decision_memory_id,
        )
        .await
    }

    async fn write_goal_atomic(&self, draft: &GoalDraft) -> Result<GoalWriteOutcome, StorageError> {
        verbs::goal_write::write_goal_atomic(&self.pool, draft).await
    }

    async fn supersede_goal_atomic(
        &self,
        prior: GoalId,
        draft: &GoalDraft,
    ) -> Result<GoalWriteOutcome, StorageError> {
        verbs::goal_write::supersede_goal_atomic(&self.pool, prior, draft).await
    }

    async fn subscribe_changes(
        &self,
        owner: &Owner,
        since: Option<uuid::Uuid>,
    ) -> Result<ChangeEventStream, StorageError> {
        verbs::subscribe::subscribe_changes(&self.pool, &self.tx, owner, since).await
    }

    async fn event_history(
        &self,
        req: &EventHistoryRequest,
    ) -> Result<EventHistoryResponse, StorageError> {
        verbs::event_history::event_history(&self.pool, req).await
    }

    async fn query_memories(
        &self,
        req: &QueryRequest,
        schemas: &[proxima_core::verbs::schema::SchemaInfo],
    ) -> Result<QueryResponse, StorageError> {
        verbs::query::query_memories(&self.pool, req, schemas).await
    }

    async fn search_memories(
        &self,
        req: &MemorySearchRequest,
        projections: &[proxima_core::verbs::schema::MemorySearchProjection],
    ) -> Result<Vec<MemorySearchResult>, StorageError> {
        verbs::query::search_memories(&self.pool, req, projections).await
    }

    async fn walk_memory_lineage(
        &self,
        req: &MemoryLineageRequest,
    ) -> Result<MemoryLineageResponse, StorageError> {
        verbs::query::walk_memory_lineage(&self.pool, req).await
    }

    async fn list_active_goals(
        &self,
        owner: &Owner,
        self_perspective_memory_id: MemoryId,
        limit: usize,
    ) -> Result<Vec<ActiveGoalSummary>, StorageError> {
        verbs::active_goals::list_active_goals(&self.pool, owner, self_perspective_memory_id, limit)
            .await
    }

    async fn close_batch(
        &self,
        owner: &Owner,
        source_batch_id: SourceBatchId,
    ) -> Result<CloseBatchOutcome, StorageError> {
        verbs::close_batch::close_batch(&self.pool, owner, source_batch_id).await
    }

    async fn register_inference_target(
        &self,
        req: &RegisterInferenceTargetRequest,
    ) -> Result<RegisterInferenceTargetResponse, StorageError> {
        settings::register_inference_target(&self.pool, req)
            .await
            .map_err(settings_error_to_storage)
    }

    async fn list_inference_targets(
        &self,
        owner: &Owner,
    ) -> Result<Vec<InferenceTargetRow>, StorageError> {
        settings::list_inference_targets(&self.pool, owner)
            .await
            .map_err(settings_error_to_storage)
    }

    async fn remove_inference_target(
        &self,
        req: &RemoveInferenceTargetRequest,
    ) -> Result<RemoveInferenceTargetResponse, StorageError> {
        settings::remove_inference_target(&self.pool, req)
            .await
            .map_err(settings_error_to_storage)
    }

    async fn bind_inference_tier(
        &self,
        req: &BindInferenceTierRequest,
    ) -> Result<BindInferenceTierResponse, StorageError> {
        settings::bind_inference_tier(&self.pool, req)
            .await
            .map_err(settings_error_to_storage)
    }

    async fn unbind_inference_tier(
        &self,
        owner: &Owner,
        tier: ModelTier,
    ) -> Result<(), StorageError> {
        settings::unbind_inference_tier(&self.pool, owner, tier)
            .await
            .map_err(settings_error_to_storage)
    }

    async fn list_inference_tier_bindings(
        &self,
        owner: &Owner,
    ) -> Result<Vec<InferenceTierBindingRow>, StorageError> {
        settings::list_inference_tier_bindings(&self.pool, owner)
            .await
            .map_err(settings_error_to_storage)
    }

    async fn list_embedding_models(
        &self,
    ) -> Result<Vec<proxima_core::EmbeddingModelConfig>, StorageError> {
        settings::list_embedding_models(&self.pool)
            .await
            .map(|rows| rows.into_iter().map(embedding_model_to_core).collect())
            .map_err(settings_error_to_storage)
    }

    async fn get_embedding_active(
        &self,
    ) -> Result<Option<proxima_core::EmbeddingModelRef>, StorageError> {
        settings::get_embedding_active(&self.pool)
            .await
            .map(|active| {
                active
                    .map(|(vendor, model_id)| proxima_core::EmbeddingModelRef { vendor, model_id })
            })
            .map_err(settings_error_to_storage)
    }

    async fn register_embedding_model(
        &self,
        model: proxima_core::EmbeddingModelConfig,
    ) -> Result<(), StorageError> {
        settings::register_embedding_model(&self.pool, embedding_model_from_core(model))
            .await
            .map_err(settings_error_to_storage)
    }

    async fn delete_embedding_model(
        &self,
        vendor: &str,
        model_id: &str,
    ) -> Result<bool, StorageError> {
        settings::delete_embedding_model(&self.pool, vendor, model_id)
            .await
            .map_err(settings_error_to_storage)
    }

    async fn set_embedding_active(&self, vendor: &str, model_id: &str) -> Result<(), StorageError> {
        settings::set_embedding_active(&self.pool, vendor, model_id)
            .await
            .map_err(settings_error_to_storage)
    }

    async fn clear_embedding_active(&self) -> Result<bool, StorageError> {
        settings::clear_embedding_active(&self.pool)
            .await
            .map_err(settings_error_to_storage)
    }

    async fn list_personality_instances(
        &self,
        owner: &Owner,
        include_tombstoned: bool,
    ) -> Result<Vec<PersonalityInstanceRow>, StorageError> {
        verbs::consolidate::list_personality_instances(&self.pool, owner, include_tombstoned).await
    }

    async fn tombstone_personality(
        &self,
        req: &TombstonePersonalityRequest,
    ) -> Result<TombstonePersonalityResponse, StorageError> {
        verbs::consolidate::tombstone_personality(&self.pool, req).await
    }

    async fn instantiate_personality(
        &self,
        req: &InstantiatePersonalityRequest,
    ) -> Result<InstantiatePersonalityResponse, StorageError> {
        verbs::consolidate::instantiate_personality(&self.pool, req).await
    }

    async fn ensure_master_token_personality(
        &self,
        owner: &Owner,
        master_token_id: uuid::Uuid,
    ) -> Result<MasterTokenPersonality, StorageError> {
        verbs::master_token_personality::ensure_master_token_personality(
            &self.pool,
            owner,
            master_token_id,
        )
        .await
    }

    async fn set_wake_entries(
        &self,
        req: &SetWakeEntriesRequest,
    ) -> Result<SetWakeEntriesResponse, StorageError> {
        verbs::consolidate::set_wake_entries(&self.pool, req).await
    }

    async fn set_wake_entries_within(
        &self,
        owner: &Owner,
        personality_instance_id: PersonalityInstanceId,
        mutate: proxima_core::WakeEntriesMutator,
    ) -> Result<SetWakeEntriesResponse, StorageError> {
        verbs::consolidate::set_wake_entries_within(
            &self.pool,
            owner,
            personality_instance_id,
            mutate,
        )
        .await
    }

    async fn list_read_scope(
        &self,
        req: &ListReadScopeRequest,
    ) -> Result<ListReadScopeResponse, StorageError> {
        verbs::consolidate::list_read_scope(&self.pool, req).await
    }

    async fn set_read_scope(
        &self,
        req: &SetReadScopeRequest,
    ) -> Result<SetReadScopeResponse, StorageError> {
        verbs::consolidate::set_read_scope(&self.pool, req).await
    }

    async fn list_active_wake_entries(&self) -> Result<Vec<WakeDispatchEntryRow>, StorageError> {
        verbs::consolidate::list_active_wake_entries(&self.pool).await
    }

    async fn list_change_events_after(
        &self,
        owner: &Owner,
        after: uuid::Uuid,
        limit: usize,
    ) -> Result<Vec<ChangeEventForWake>, StorageError> {
        verbs::consolidate::list_change_events_after(&self.pool, owner, after, limit).await
    }

    async fn list_change_events_for_replay(
        &self,
        owner: &Owner,
        after: uuid::Uuid,
        until: Option<uuid::Uuid>,
        limit: usize,
    ) -> Result<Vec<ChangeEventForWake>, StorageError> {
        verbs::consolidate::list_change_events_for_replay(&self.pool, owner, after, until, limit)
            .await
    }

    async fn advance_wake_cursor(
        &self,
        owner: &Owner,
        instance: PersonalityInstanceId,
        last_considered_seq: uuid::Uuid,
    ) -> Result<(), StorageError> {
        verbs::consolidate::advance_wake_cursor(&self.pool, owner, instance, last_considered_seq)
            .await
    }

    async fn try_begin_wake_invocation(
        &self,
        owner: &Owner,
        instance: PersonalityInstanceId,
        wake_entry_id: uuid::Uuid,
        change_event_seq: uuid::Uuid,
    ) -> Result<bool, StorageError> {
        verbs::consolidate::try_begin_wake_invocation(
            &self.pool,
            owner,
            instance,
            wake_entry_id,
            change_event_seq,
        )
        .await
    }

    async fn start_wake_invocation(
        &self,
        start: &WakeInvocationStart,
    ) -> Result<bool, StorageError> {
        verbs::consolidate::start_wake_invocation(&self.pool, start).await
    }

    async fn finish_wake_invocation(
        &self,
        owner: &Owner,
        instance: PersonalityInstanceId,
        wake_entry_id: uuid::Uuid,
        change_event_seq: uuid::Uuid,
        status: WakeInvocationStatus,
        turn_count: u16,
        cost_usd: f64,
    ) -> Result<(), StorageError> {
        verbs::consolidate::finish_wake_invocation(
            &self.pool,
            owner,
            instance,
            wake_entry_id,
            change_event_seq,
            status,
            turn_count,
            cost_usd,
        )
        .await
    }

    async fn finalize_wake_invocation(
        &self,
        finalize: &WakeInvocationFinalize,
    ) -> Result<(), StorageError> {
        verbs::consolidate::finalize_wake_invocation(&self.pool, finalize).await
    }

    async fn append_wake_invocation_log(
        &self,
        log: &WakeInvocationLogDraft,
    ) -> Result<(), StorageError> {
        verbs::consolidate::append_wake_invocation_log(&self.pool, log).await
    }

    async fn list_wake_invocations(
        &self,
        req: &ListWakeInvocationsRequest,
    ) -> Result<Vec<WakeInvocationRow>, StorageError> {
        verbs::consolidate::list_wake_invocations(&self.pool, req).await
    }

    async fn load_memory_batch_facts(
        &self,
        owner: &Owner,
        memory_id: proxima_core::MemoryId,
        sidecars: &[SidecarSpec],
    ) -> Result<Vec<proxima_core::FactRow>, StorageError> {
        verbs::consolidate::load_memory_batch_facts(&self.pool, owner, memory_id, sidecars).await
    }

    async fn load_abstraction_heads(
        &self,
        owner: &Owner,
        sidecars: &[SidecarSpec],
        limit: usize,
    ) -> Result<Vec<AbstractionRow>, StorageError> {
        verbs::consolidate::load_abstraction_heads(&self.pool, owner, sidecars, limit).await
    }

    async fn load_perspective_heads(
        &self,
        owner: &Owner,
        instance: PersonalityInstanceId,
        root_perspective_memory_id: proxima_core::MemoryId,
        sidecars: &[SidecarSpec],
        limit: usize,
    ) -> Result<Vec<MemorySnapshot>, StorageError> {
        verbs::consolidate::load_perspective_heads(
            &self.pool,
            owner,
            instance,
            root_perspective_memory_id,
            sidecars,
            limit,
        )
        .await
    }

    async fn lookup_prior_personality_head(
        &self,
        owner: &Owner,
        instance: &PersonalityRef,
        schema_id: &proxima_core::SchemaId,
    ) -> Result<Option<proxima_core::MemoryId>, StorageError> {
        verbs::consolidate::lookup_prior_personality_head(&self.pool, owner, instance, schema_id)
            .await
    }

    async fn append_personality_memories(
        &self,
        req: &PersonalityWriteRequest<'_>,
    ) -> Result<PersonalityWriteOutcome, StorageError> {
        verbs::consolidate::append_personality_memories(&self.pool, req).await
    }

    async fn load_memory_by_id(
        &self,
        owner: &Owner,
        memory_id: proxima_core::MemoryId,
        reader_personality_instance_id: Option<PersonalityInstanceId>,
        sidecars: &[SidecarSpec],
    ) -> Result<Option<MemorySnapshot>, StorageError> {
        verbs::consolidate::load_memory_by_id(
            &self.pool,
            owner,
            memory_id,
            reader_personality_instance_id,
            sidecars,
        )
        .await
    }

    async fn fetch_personality_runtime(
        &self,
        owner: &Owner,
        instance_id: PersonalityInstanceId,
    ) -> Result<Option<PersonalityRuntimeRow>, StorageError> {
        verbs::wake_context::fetch_personality_runtime(&self.pool, owner, instance_id).await
    }

    async fn fetch_root_personality_perspective(
        &self,
        owner: &Owner,
        memory_id: proxima_core::MemoryId,
    ) -> Result<Option<RootPersonalityPerspectiveRow>, StorageError> {
        verbs::wake_context::fetch_root_personality_perspective(&self.pool, owner, memory_id).await
    }

    async fn fetch_change_event_for_wake(
        &self,
        owner: &Owner,
        seq: uuid::Uuid,
    ) -> Result<Option<ChangeEventForWake>, StorageError> {
        verbs::wake_context::fetch_change_event_for_wake(&self.pool, owner, seq).await
    }

    async fn list_memory_dependencies(
        &self,
        owner: &Owner,
        source_memory_id: MemoryId,
    ) -> Result<Vec<MemoryDependency>, StorageError> {
        verbs::consolidate::list_memory_dependencies(&self.pool, owner, source_memory_id).await
    }

    async fn has_successful_core_workspace_run_derived_from(
        &self,
        owner: &Owner,
        source_memory_id: MemoryId,
    ) -> Result<bool, StorageError> {
        verbs::consolidate::has_successful_core_workspace_run_derived_from(
            &self.pool,
            owner,
            source_memory_id,
        )
        .await
    }

    async fn has_satisfied_code_test_request(
        &self,
        owner: &Owner,
        test_request_memory_id: MemoryId,
    ) -> Result<bool, StorageError> {
        verbs::consolidate::has_satisfied_code_test_request(
            &self.pool,
            owner,
            test_request_memory_id,
        )
        .await
    }

    async fn upsert_blocked_wake_candidate(
        &self,
        candidate: &BlockedWakeCandidate,
    ) -> Result<(), StorageError> {
        verbs::consolidate::upsert_blocked_wake_candidate(&self.pool, candidate).await
    }

    async fn list_blocked_wake_candidates(
        &self,
        owner: &Owner,
        personality_instance_id: PersonalityInstanceId,
        limit: usize,
    ) -> Result<Vec<BlockedWakeCandidate>, StorageError> {
        verbs::consolidate::list_blocked_wake_candidates(
            &self.pool,
            owner,
            personality_instance_id,
            limit,
        )
        .await
    }

    async fn delete_blocked_wake_candidate(
        &self,
        owner: &Owner,
        personality_instance_id: PersonalityInstanceId,
        wake_entry_id: uuid::Uuid,
        change_event_seq: uuid::Uuid,
    ) -> Result<(), StorageError> {
        verbs::consolidate::delete_blocked_wake_candidate(
            &self.pool,
            owner,
            personality_instance_id,
            wake_entry_id,
            change_event_seq,
        )
        .await
    }

    async fn acquire_wake_lock(
        &self,
        owner: &Owner,
        instance: &PersonalityRef,
    ) -> Result<WakeLockGuard, StorageError> {
        personality_locks::acquire_wake_lock(&self.pool, owner, instance).await
    }
}

fn settings_error_to_storage(err: settings::SettingsError) -> StorageError {
    match err {
        settings::SettingsError::Conflict(msg) | settings::SettingsError::InUse(msg) => {
            StorageError::ConstraintViolation(msg)
        }
        settings::SettingsError::Invariant(msg) => StorageError::ConstraintViolation(msg),
        settings::SettingsError::Database(err) => crate::error::map_err(err),
        settings::SettingsError::Json(err) => StorageError::Internal(err.to_string()),
        settings::SettingsError::DuplicateEmbeddingModel { vendor, model_id } => {
            StorageError::ConstraintViolation(format!(
                "duplicate embedding model {vendor:?}/{model_id:?}"
            ))
        }
        settings::SettingsError::UnknownEmbeddingModel { vendor, model_id } => {
            StorageError::ConstraintViolation(format!(
                "unknown embedding model {vendor:?}/{model_id:?}"
            ))
        }
    }
}

fn embedding_model_to_core(model: settings::EmbeddingModel) -> proxima_core::EmbeddingModelConfig {
    proxima_core::EmbeddingModelConfig {
        vendor: model.vendor,
        model_id: model.model_id,
        base_url: model.base_url,
        caps: model.caps,
        secret_ref: model.secret_ref,
    }
}

fn embedding_model_from_core(
    model: proxima_core::EmbeddingModelConfig,
) -> settings::EmbeddingModel {
    settings::EmbeddingModel {
        vendor: model.vendor,
        model_id: model.model_id,
        base_url: model.base_url,
        caps: model.caps,
        secret_ref: model.secret_ref,
    }
}

/// Settings registration — see [`mod@settings`] for shape.
impl PgStorage {
    /// # Errors
    /// `SettingsError::Database` for connectivity failures.
    pub async fn list_embedding_models(
        &self,
    ) -> Result<Vec<settings::EmbeddingModel>, settings::SettingsError> {
        settings::list_embedding_models(&self.pool).await
    }

    /// # Errors
    /// `SettingsError::Database` for connectivity failures.
    pub async fn get_embedding_active(
        &self,
    ) -> Result<Option<(String, String)>, settings::SettingsError> {
        settings::get_embedding_active(&self.pool).await
    }

    /// # Errors
    /// `SettingsError::Database` for connectivity failures.
    /// `SettingsError::DuplicateEmbeddingModel` if (vendor, `model_id`) already exists.
    pub async fn register_embedding_model(
        &self,
        m: settings::EmbeddingModel,
    ) -> Result<(), settings::SettingsError> {
        settings::register_embedding_model(&self.pool, m).await
    }

    /// # Errors
    /// `SettingsError::Database` for connectivity failures.
    pub async fn delete_embedding_model(
        &self,
        vendor: &str,
        model_id: &str,
    ) -> Result<bool, settings::SettingsError> {
        settings::delete_embedding_model(&self.pool, vendor, model_id).await
    }

    /// # Errors
    /// `SettingsError::Database` for connectivity failures.
    /// `SettingsError::UnknownEmbeddingModel` if (vendor, `model_id`) is not registered.
    pub async fn set_embedding_active(
        &self,
        vendor: &str,
        model_id: &str,
    ) -> Result<(), settings::SettingsError> {
        settings::set_embedding_active(&self.pool, vendor, model_id).await
    }

    /// # Errors
    /// `SettingsError::Database` for connectivity failures.
    pub async fn clear_embedding_active(&self) -> Result<bool, settings::SettingsError> {
        settings::clear_embedding_active(&self.pool).await
    }
}

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
    InstantiatePersonalityResponse, ListReadScopeRequest, ListReadScopeResponse, MemorySnapshot,
    PersonalityInstanceId, PersonalityInstanceRow, PersonalityRef, PersonalityWriteOutcome,
    PersonalityWriteRequest, SetReadScopeRequest, SetReadScopeResponse, SetWakeEntriesRequest,
    SetWakeEntriesResponse, SidecarSpec, TombstonePersonalityRequest, TombstonePersonalityResponse,
    WakeDispatchEntryRow,
};
use proxima_core::verbs::close_batch::CloseBatchOutcome;
use proxima_core::verbs::event_history::{EventHistoryRequest, EventHistoryResponse};
use proxima_core::verbs::event_ingest::{EventDraft, EventIngestOutcome};
use proxima_core::verbs::fact_cleanup::CleanupDueFactsOutcome;
use proxima_core::verbs::goal_write::{GoalDraft, GoalWriteOutcome};
use proxima_core::verbs::persist_mcp_call::{McpCallLogInput, McpCallLogOutcome};
use proxima_core::verbs::query::{
    MemoryLineageRequest, MemoryLineageResponse, MemorySearchRequest, MemorySearchResult,
    QueryRequest, QueryResponse,
};
use proxima_core::verbs::subscribe::ChangeEventStream;
use proxima_core::{
    ChangeEvent, GoalId, MasterTokenPersonality, MemoryDependency, MemoryId, Owner, Principal,
    SourceBatchId, Storage, StorageError, StorageHandle,
};
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use tokio::sync::broadcast;

mod authorship;
mod error;
pub mod outbox;
mod pg_ident;
pub mod query {
    pub use crate::verbs::query::MAX_SNAPSHOT_EDGES;
}
pub mod verbs;

use outbox::BROADCAST_CAPACITY;

/// Default DB URL when `DATABASE_URL` is unset. Matches the
/// dev DB created locally via `createdb proxima_dev`.
pub const DEFAULT_DATABASE_URL: &str = "postgres://postgres@localhost/proxima_dev";

/// Embedded core migration set under `crates/storage-pg/migrations/`.
///
/// `ignore_missing = true` is load-bearing when the same database also
/// records flavor migrations in `SQLx`'s default `_sqlx_migrations` table.
#[must_use]
pub fn core_migrator() -> sqlx::migrate::Migrator {
    let mut migrator = sqlx::migrate!("./migrations");
    migrator.set_ignore_missing(true);
    migrator
}

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
        core_migrator()
            .run(&self.pool)
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

    async fn persist_mcp_call_atomic(
        &self,
        input: &McpCallLogInput,
    ) -> Result<McpCallLogOutcome, StorageError> {
        verbs::persist_mcp_call::persist_mcp_call_atomic(&self.pool, input).await
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
        principal: &Principal,
        since: Option<uuid::Uuid>,
    ) -> Result<ChangeEventStream, StorageError> {
        verbs::subscribe::subscribe_changes(&self.pool, &self.tx, principal, since).await
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
        principal: &Principal,
        self_perspective_memory_id: MemoryId,
        limit: usize,
    ) -> Result<Vec<ActiveGoalSummary>, StorageError> {
        verbs::active_goals::list_active_goals(
            &self.pool,
            principal,
            self_perspective_memory_id,
            limit,
        )
        .await
    }

    async fn close_batch(
        &self,
        principal: &Principal,
        source_batch_id: SourceBatchId,
    ) -> Result<CloseBatchOutcome, StorageError> {
        verbs::close_batch::close_batch(&self.pool, principal, source_batch_id).await
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

    async fn upsert_fact_retention(&self, owner: &Owner, seconds: i64) -> Result<(), StorageError> {
        verbs::fact_retention::upsert_fact_retention(&self.pool, owner, seconds).await
    }

    async fn get_fact_retention(&self, owner: &Owner) -> Result<Option<i64>, StorageError> {
        verbs::fact_retention::get_fact_retention(&self.pool, owner).await
    }

    async fn clear_fact_retention(&self, owner: &Owner) -> Result<bool, StorageError> {
        verbs::fact_retention::clear_fact_retention(&self.pool, owner).await
    }

    async fn cleanup_due_facts(
        &self,
        owner: &Owner,
        fact_sidecar_tables: &[String],
        citation_mapping_sidecar_tables: &[String],
    ) -> Result<CleanupDueFactsOutcome, StorageError> {
        verbs::fact_cleanup::cleanup_due_facts(
            &self.pool,
            owner,
            fact_sidecar_tables,
            citation_mapping_sidecar_tables,
        )
        .await
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

    async fn list_memory_dependencies(
        &self,
        owner: &Owner,
        source_memory_id: MemoryId,
    ) -> Result<Vec<MemoryDependency>, StorageError> {
        verbs::consolidate::list_memory_dependencies(&self.pool, owner, source_memory_id).await
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
}

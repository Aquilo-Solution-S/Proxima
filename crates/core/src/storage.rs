//! Abstract storage trait — backend-neutral surface for the
//! engine.
//!
//! See docs/07-storage.md and AGENTS.md invariants 2, 3, 5.

use std::sync::Arc;

use crate::GoalId;
use crate::Owner;
use crate::SourceBatchId;
use crate::personality::{
    AbstractionRow, ActiveGoalSummary, ChangeEventForWake, InstantiatePersonalityRequest,
    InstantiatePersonalityResponse, MemorySnapshot, PersonalityInstanceRow, PersonalityRef,
    PersonalityWriteOutcome, PersonalityWriteRequest, SetWakeConfigRequest, SetWakeConfigResponse,
    SidecarSpec, TombstonePersonalityRequest, TombstonePersonalityResponse, WakeConfigRow,
    WakeInvocationStatus,
};
use crate::verbs::close_batch::CloseBatchOutcome;
use crate::verbs::event_history::{EventHistoryRequest, EventHistoryResponse};
use crate::verbs::event_ingest::{EventDraft, EventIngestOutcome};
use crate::verbs::goal_write::{GoalDraft, GoalWriteOutcome};
use crate::verbs::subscribe::ChangeEventStream;

#[derive(Debug, Clone, thiserror::Error)]
pub enum StorageError {
    #[error("storage backend unavailable: {0}")]
    Unavailable(String),
    #[error("constraint violation: {0}")]
    ConstraintViolation(String),
    #[error("not found")]
    NotFound,
    #[error("internal storage error: {0}")]
    Internal(String),
}

#[async_trait::async_trait]
pub trait Storage: Send + Sync {
    /// Atomic Fact materialization per docs/14 §EventIngest.
    /// Single transaction inserting cited_object, event,
    /// memory(Fact), citation_mapping, change_event. Replay
    /// (event_id collision) returns the original outcome with
    /// `idempotent_replay = true`.
    ///
    /// # Errors
    ///
    /// Constraint violations map to ConstraintViolation; sqlx
    /// failures map to Internal.
    async fn ingest_event_atomic(
        &self,
        draft: &EventDraft,
    ) -> Result<EventIngestOutcome, StorageError>;

    /// Atomic Goal write per docs/14 §GoalWrite.
    /// Single transaction inserting goal, goal_parents,
    /// change_event. Replay (request_id collision with same body)
    /// returns the original outcome with `idempotent_replay = true`.
    ///
    /// # Errors
    ///
    /// Constraint violations map to ConstraintViolation (including
    /// idempotency_conflict: prefix for request_id reuse with
    /// different body); NotFound if referenced parent missing;
    /// sqlx failures map to Internal.
    async fn write_goal_atomic(&self, draft: &GoalDraft) -> Result<GoalWriteOutcome, StorageError>;

    /// Atomic Goal supersede per docs/14 §GoalWrite.
    /// Single transaction inserting new goal with supersedes=prior,
    /// goal_parents, change_event. Replay check same as write_goal.
    ///
    /// # Errors
    ///
    /// Same as write_goal_atomic, plus NotFound if prior goal
    /// doesn't exist, ConstraintViolation if prior owner doesn't
    /// match draft.owner.
    async fn supersede_goal_atomic(
        &self,
        prior: GoalId,
        draft: &GoalDraft,
    ) -> Result<GoalWriteOutcome, StorageError>;

    /// Returns an owner-filtered stream of ChangeEvents. If
    /// `since` is `Some(seq)`, the stream begins by replaying
    /// rows whose `seq > since` and then attaches to live events.
    /// If `None`, the live stream begins immediately (no
    /// backfill).
    ///
    /// Per docs/14 §Cursor: at-least-once delivery; clients
    /// dedupe by `seq`. The server does NOT dedupe.
    async fn subscribe_changes(
        &self,
        owner: &crate::Owner,
        since: Option<uuid::Uuid>,
    ) -> Result<ChangeEventStream, StorageError>;

    /// Owner-scoped bounded read of `change_event` rows, newest-first.
    /// Server clamps `limit` to `MAX_EVENT_HISTORY_LIMIT`. When
    /// `before` is `Some(seq)`, returns rows with `seq < before`.
    /// `seq_high_water` is the latest seq in the owner's change_event
    /// log at read time (cursor for a follow-up Subscribe).
    async fn event_history(
        &self,
        req: &EventHistoryRequest,
    ) -> Result<EventHistoryResponse, StorageError>;

    /// Owner-scoped snapshot read of memories per docs/14 §"Query".
    /// Returns MemoryRow substrate shape with payload bytes projected
    /// from sidecar tables. `schemas` is the list of registered schemas
    /// with sidecar tables for dynamic JOIN construction.
    async fn query_memories(
        &self,
        req: &crate::verbs::query::QueryRequest,
        schemas: &[crate::verbs::schema::SchemaInfo],
    ) -> Result<crate::verbs::query::QueryResponse, StorageError>;

    /// Owner-scoped active Goal query for one personality Self-Perspective.
    /// Traverses `core/inspires` edges authored at proposal/attachment time,
    /// follows Goal supersession forward, and returns only current Active
    /// heads. No GoalConnection sidecar is modeled.
    async fn list_active_goals(
        &self,
        owner: &Owner,
        self_perspective_memory_id: crate::MemoryId,
        limit: usize,
    ) -> Result<Vec<ActiveGoalSummary>, StorageError>;

    /// Owner-scoped, idempotent batch close. See docs/01 §"The contract"
    /// and docs/04 §"Source-batch lifecycle". Flips
    /// `source_batches.closed_at` from NULL to `now()`. Re-close is a
    /// no-op returning the existing `closed_at` with `already_closed = true`.
    /// A batch belonging to a different owner returns `NotFound`.
    ///
    /// # Errors
    ///
    /// Returns `NotFound` when the batch doesn't exist or belongs to a
    /// different owner. sqlx failures map to `Internal`.
    async fn close_batch(
        &self,
        owner: &Owner,
        source_batch_id: SourceBatchId,
    ) -> Result<CloseBatchOutcome, StorageError>;

    /// List configured personality instances for an owner. When
    /// `include_tombstoned` is `false` (the default for UI listings),
    /// rows whose status is `tombstoned` are filtered out. Provisioning
    /// passes `true` so a previously tombstoned default isn't recreated.
    async fn list_personality_instances(
        &self,
        owner: &Owner,
        personality_type_id: Option<&str>,
        include_tombstoned: bool,
    ) -> Result<Vec<PersonalityInstanceRow>, StorageError>;

    /// Mark a personality instance tombstoned. Subsequent dispatcher
    /// ticks must skip it; `set_wake_config` against the same key must
    /// return `NotFound`. Idempotent on the natural key: repeats return
    /// `idempotent_replay = true` without rewriting `tombstoned_at`.
    async fn tombstone_personality(
        &self,
        req: &TombstonePersonalityRequest,
    ) -> Result<TombstonePersonalityResponse, StorageError>;

    /// Instantiate one personality instance with its self-Perspective,
    /// wake_config, and cursor rows.
    async fn instantiate_personality(
        &self,
        req: &InstantiatePersonalityRequest,
        self_draft: &crate::PersonalitySelfDraft,
        self_sidecar_table: &str,
        default_wake_filters: &[crate::WakeFilter],
    ) -> Result<InstantiatePersonalityResponse, StorageError>;

    /// Rewrite wake filters and mark the row active.
    async fn set_wake_config(
        &self,
        req: &SetWakeConfigRequest,
    ) -> Result<SetWakeConfigResponse, StorageError>;

    /// Active wake configs plus their cursor positions.
    async fn list_active_wake_configs(&self) -> Result<Vec<WakeConfigRow>, StorageError>;

    async fn mark_wake_config_needs_repair(
        &self,
        owner: &Owner,
        instance: &PersonalityRef,
    ) -> Result<(), StorageError>;

    async fn list_change_events_after(
        &self,
        owner: &Owner,
        after: uuid::Uuid,
        limit: usize,
    ) -> Result<Vec<ChangeEventForWake>, StorageError>;

    async fn advance_wake_cursor(
        &self,
        owner: &Owner,
        instance: &PersonalityRef,
        last_considered_seq: uuid::Uuid,
    ) -> Result<(), StorageError>;

    async fn try_begin_wake_invocation(
        &self,
        owner: &Owner,
        instance: &PersonalityRef,
        change_event_seq: uuid::Uuid,
    ) -> Result<bool, StorageError>;

    async fn finish_wake_invocation(
        &self,
        owner: &Owner,
        instance: &PersonalityRef,
        change_event_seq: uuid::Uuid,
        status: WakeInvocationStatus,
        turn_count: u16,
        cost_usd: f64,
    ) -> Result<(), StorageError>;

    async fn load_memory_batch_facts(
        &self,
        owner: &Owner,
        memory_id: crate::MemoryId,
        sidecars: &[SidecarSpec],
    ) -> Result<Vec<crate::FactRow>, StorageError>;

    async fn load_abstraction_heads(
        &self,
        owner: &Owner,
        sidecars: &[SidecarSpec],
        limit: usize,
    ) -> Result<Vec<AbstractionRow>, StorageError>;

    async fn lookup_prior_personality_head(
        &self,
        owner: &Owner,
        instance: &PersonalityRef,
        schema_id: &crate::SchemaId,
    ) -> Result<Option<crate::MemoryId>, StorageError>;

    async fn append_personality_memories(
        &self,
        req: &PersonalityWriteRequest<'_>,
    ) -> Result<PersonalityWriteOutcome, StorageError>;

    /// Owner-scoped fetch of a single memory by id, joined with whichever
    /// sidecar table holds its typed payload (matched by `schema_id`).
    /// Returns `None` when the memory does not exist or belongs to a
    /// different owner.
    async fn load_memory_by_id(
        &self,
        owner: &Owner,
        memory_id: crate::MemoryId,
        sidecars: &[SidecarSpec],
    ) -> Result<Option<MemorySnapshot>, StorageError>;

    /// Per-(owner, type_id, instance_id) advisory lock spanning a wake
    /// run. Acquires `pg_advisory_xact_lock` on a stable bigint hash;
    /// the returned guard releases the lock when dropped.
    async fn acquire_wake_lock(
        &self,
        owner: &Owner,
        instance: &PersonalityRef,
    ) -> Result<WakeLockGuard, StorageError>;
}

/// RAII guard for the advisory lock held during a wake run. Storage
/// backends that don't model real locking return a no-op guard.
pub struct WakeLockGuard {
    pub release: Option<Box<dyn FnOnce() + Send + Sync>>,
}

impl std::fmt::Debug for WakeLockGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WakeLockGuard")
            .field("released", &self.release.is_none())
            .finish()
    }
}

impl WakeLockGuard {
    #[must_use]
    pub fn noop() -> Self {
        Self { release: None }
    }
}

impl Drop for WakeLockGuard {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            release();
        }
    }
}

pub type StorageHandle = Arc<dyn Storage>;

/// Storage that rejects all writes — used by the in-memory
/// demo path and by tests that don't want PG.
#[derive(Debug, Default, Clone)]
pub struct NoopStorage;

#[async_trait::async_trait]
impl Storage for NoopStorage {
    async fn ingest_event_atomic(
        &self,
        _draft: &EventDraft,
    ) -> Result<EventIngestOutcome, StorageError> {
        Err(StorageError::Internal("NoopStorage rejects writes".into()))
    }

    async fn write_goal_atomic(
        &self,
        _draft: &GoalDraft,
    ) -> Result<GoalWriteOutcome, StorageError> {
        Err(StorageError::Internal("NoopStorage rejects writes".into()))
    }

    async fn supersede_goal_atomic(
        &self,
        _prior: GoalId,
        _draft: &GoalDraft,
    ) -> Result<GoalWriteOutcome, StorageError> {
        Err(StorageError::Internal("NoopStorage rejects writes".into()))
    }

    async fn subscribe_changes(
        &self,
        _owner: &Owner,
        _since: Option<uuid::Uuid>,
    ) -> Result<ChangeEventStream, StorageError> {
        Ok(Box::pin(futures_util::stream::empty()))
    }

    async fn event_history(
        &self,
        _req: &EventHistoryRequest,
    ) -> Result<EventHistoryResponse, StorageError> {
        Ok(EventHistoryResponse {
            events: Vec::new(),
            seq_high_water: None,
        })
    }

    async fn query_memories(
        &self,
        _req: &crate::verbs::query::QueryRequest,
        _schemas: &[crate::verbs::schema::SchemaInfo],
    ) -> Result<crate::verbs::query::QueryResponse, StorageError> {
        Ok(crate::verbs::query::QueryResponse {
            memories: Vec::new(),
            goals: Vec::new(),
            edges: Vec::new(),
            seq_high_water: None,
        })
    }

    async fn list_active_goals(
        &self,
        _owner: &Owner,
        _self_perspective_memory_id: crate::MemoryId,
        _limit: usize,
    ) -> Result<Vec<ActiveGoalSummary>, StorageError> {
        Ok(Vec::new())
    }

    async fn close_batch(
        &self,
        _owner: &Owner,
        _source_batch_id: SourceBatchId,
    ) -> Result<CloseBatchOutcome, StorageError> {
        Err(StorageError::Internal("NoopStorage rejects writes".into()))
    }

    async fn list_personality_instances(
        &self,
        _owner: &Owner,
        _personality_type_id: Option<&str>,
        _include_tombstoned: bool,
    ) -> Result<Vec<PersonalityInstanceRow>, StorageError> {
        Ok(Vec::new())
    }

    async fn tombstone_personality(
        &self,
        _req: &TombstonePersonalityRequest,
    ) -> Result<TombstonePersonalityResponse, StorageError> {
        Err(StorageError::Internal("NoopStorage rejects writes".into()))
    }

    async fn instantiate_personality(
        &self,
        _req: &InstantiatePersonalityRequest,
        _self_draft: &crate::PersonalitySelfDraft,
        _self_sidecar_table: &str,
        _default_wake_filters: &[crate::WakeFilter],
    ) -> Result<InstantiatePersonalityResponse, StorageError> {
        Err(StorageError::Internal("NoopStorage rejects writes".into()))
    }

    async fn set_wake_config(
        &self,
        _req: &SetWakeConfigRequest,
    ) -> Result<SetWakeConfigResponse, StorageError> {
        Err(StorageError::Internal("NoopStorage rejects writes".into()))
    }

    async fn list_active_wake_configs(&self) -> Result<Vec<WakeConfigRow>, StorageError> {
        Ok(Vec::new())
    }

    async fn mark_wake_config_needs_repair(
        &self,
        _owner: &Owner,
        _instance: &PersonalityRef,
    ) -> Result<(), StorageError> {
        Ok(())
    }

    async fn list_change_events_after(
        &self,
        _owner: &Owner,
        _after: uuid::Uuid,
        _limit: usize,
    ) -> Result<Vec<ChangeEventForWake>, StorageError> {
        Ok(Vec::new())
    }

    async fn advance_wake_cursor(
        &self,
        _owner: &Owner,
        _instance: &PersonalityRef,
        _last_considered_seq: uuid::Uuid,
    ) -> Result<(), StorageError> {
        Ok(())
    }

    async fn try_begin_wake_invocation(
        &self,
        _owner: &Owner,
        _instance: &PersonalityRef,
        _change_event_seq: uuid::Uuid,
    ) -> Result<bool, StorageError> {
        Ok(false)
    }

    async fn finish_wake_invocation(
        &self,
        _owner: &Owner,
        _instance: &PersonalityRef,
        _change_event_seq: uuid::Uuid,
        _status: WakeInvocationStatus,
        _turn_count: u16,
        _cost_usd: f64,
    ) -> Result<(), StorageError> {
        Ok(())
    }

    async fn load_memory_batch_facts(
        &self,
        _owner: &Owner,
        _memory_id: crate::MemoryId,
        _sidecars: &[SidecarSpec],
    ) -> Result<Vec<crate::FactRow>, StorageError> {
        Ok(Vec::new())
    }

    async fn load_abstraction_heads(
        &self,
        _owner: &Owner,
        _sidecars: &[SidecarSpec],
        _limit: usize,
    ) -> Result<Vec<AbstractionRow>, StorageError> {
        Ok(Vec::new())
    }

    async fn lookup_prior_personality_head(
        &self,
        _owner: &Owner,
        _instance: &PersonalityRef,
        _schema_id: &crate::SchemaId,
    ) -> Result<Option<crate::MemoryId>, StorageError> {
        Ok(None)
    }

    async fn append_personality_memories(
        &self,
        _req: &PersonalityWriteRequest<'_>,
    ) -> Result<PersonalityWriteOutcome, StorageError> {
        Err(StorageError::Internal("NoopStorage rejects writes".into()))
    }

    async fn load_memory_by_id(
        &self,
        _owner: &Owner,
        _memory_id: crate::MemoryId,
        _sidecars: &[SidecarSpec],
    ) -> Result<Option<MemorySnapshot>, StorageError> {
        Ok(None)
    }

    async fn acquire_wake_lock(
        &self,
        _owner: &Owner,
        _instance: &PersonalityRef,
    ) -> Result<WakeLockGuard, StorageError> {
        Ok(WakeLockGuard::noop())
    }
}

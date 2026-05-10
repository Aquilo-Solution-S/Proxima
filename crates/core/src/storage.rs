//! Abstract storage trait — backend-neutral surface for the
//! engine.
//!
//! See docs/07-storage.md and AGENTS.md invariants 2, 3, 5.

use std::sync::Arc;

use crate::GoalId;
use crate::Owner;
use crate::SourceBatchId;
use crate::inference::{
    BindInferenceTierRequest, BindInferenceTierResponse, InferenceTargetRow,
    InferenceTierBindingRow, RegisterInferenceTargetRequest, RegisterInferenceTargetResponse,
    RemoveInferenceTargetRequest, RemoveInferenceTargetResponse,
};
use crate::personality::WakeEntryDraft;
use crate::personality::{
    AbstractionRow, ActiveGoalSummary, ChangeEventForWake, InstantiatePersonalityRequest,
    InstantiatePersonalityResponse, ListWakeInvocationsRequest, MemorySnapshot,
    PersonalityInstanceId, PersonalityInstanceRow, PersonalityRef, PersonalityRuntimeRow,
    PersonalityWriteOutcome, PersonalityWriteRequest, RootPersonalityPerspectiveRow,
    SetWakeEntriesRequest, SetWakeEntriesResponse, SidecarSpec, TombstonePersonalityRequest,
    TombstonePersonalityResponse, WakeDispatchEntryRow, WakeInvocationFinalize,
    WakeInvocationLogDraft, WakeInvocationRow, WakeInvocationStart, WakeInvocationStatus,
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

/// Boxed closure for read-modify-write on WakeEntry rows.
pub type WakeEntriesMutator =
    Box<dyn FnOnce(&[WakeEntryDraft]) -> Result<Vec<WakeEntryDraft>, String> + Send + 'static>;

/// Identity row for a per-master-token shell-author personality.
///
/// Returned by [`Storage::ensure_master_token_personality`].
/// Carries both the personality instance id and the
/// `current_root_perspective_memory_id` so callers can populate
/// `McpToolCtx.caller_self_perspective` without a second round trip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MasterTokenPersonality {
    pub instance_id: crate::PersonalityInstanceId,
    pub self_perspective_memory_id: crate::MemoryId,
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

    /// Register or replace an InferenceTarget. Idempotent on
    /// `(owner, target_ref)` when the body matches; returns
    /// `target_ref_conflict` (mapped at the verb layer) if the body
    /// differs from the existing row.
    async fn register_inference_target(
        &self,
        req: &RegisterInferenceTargetRequest,
    ) -> Result<RegisterInferenceTargetResponse, StorageError>;

    async fn list_inference_targets(
        &self,
        owner: &Owner,
    ) -> Result<Vec<InferenceTargetRow>, StorageError>;

    /// Remove an InferenceTarget. Returns `idempotent_replay = true` if
    /// the row was already absent. Returns `target_in_use` (mapped at
    /// the verb layer) if a tier binding or wake entry still references
    /// `target_ref`.
    async fn remove_inference_target(
        &self,
        req: &RemoveInferenceTargetRequest,
    ) -> Result<RemoveInferenceTargetResponse, StorageError>;

    /// Upsert a tier binding.
    async fn bind_inference_tier(
        &self,
        req: &BindInferenceTierRequest,
    ) -> Result<BindInferenceTierResponse, StorageError>;

    /// Remove a tier binding. Idempotent.
    async fn unbind_inference_tier(
        &self,
        owner: &Owner,
        tier: crate::ModelTier,
    ) -> Result<(), StorageError>;

    async fn list_inference_tier_bindings(
        &self,
        owner: &Owner,
    ) -> Result<Vec<InferenceTierBindingRow>, StorageError>;

    /// List configured personality instances for an owner. When
    /// `include_tombstoned` is `false` (the default for UI listings),
    /// rows whose status is `tombstoned` are filtered out.
    /// Implementations populate each row's active `wake_entries`.
    async fn list_personality_instances(
        &self,
        owner: &Owner,
        include_tombstoned: bool,
    ) -> Result<Vec<PersonalityInstanceRow>, StorageError>;

    /// Mark a personality instance tombstoned. Subsequent dispatcher
    /// ticks must skip it. Idempotent on the natural key: repeats
    /// return `idempotent_replay = true` without rewriting
    /// `tombstoned_at`.
    async fn tombstone_personality(
        &self,
        req: &TombstonePersonalityRequest,
    ) -> Result<TombstonePersonalityResponse, StorageError>;

    /// Instantiate one inert personality instance with its Root
    /// Perspective and cursor rows. Writes the canonical
    /// `proxima_core.root_personality_perspective_v1` sidecar.
    async fn instantiate_personality(
        &self,
        req: &InstantiatePersonalityRequest,
    ) -> Result<InstantiatePersonalityResponse, StorageError>;

    /// Ensure a per-master-token shell-author personality exists for
    /// the `(owner, master_token_id)` pair. Idempotent: returns the
    /// existing identity on replay, or mints a fresh personality with
    /// `display_name = "shell-author"`,
    /// `purpose = "Per-master-token MCP client identity"`, an empty
    /// `WakeConfig`, and a row in
    /// `proxima_core.master_token_personality`.
    async fn ensure_master_token_personality(
        &self,
        owner: &Owner,
        master_token_id: uuid::Uuid,
    ) -> Result<MasterTokenPersonality, StorageError>;

    /// Replace active WakeEntry rows for one personality instance.
    async fn set_wake_entries(
        &self,
        req: &SetWakeEntriesRequest,
    ) -> Result<SetWakeEntriesResponse, StorageError>;

    /// Transactional read-modify-write over a personality's WakeConfig.
    /// Locks the personality row (SELECT FOR UPDATE), reads current active
    /// wake entries, applies the `mutate` closure, then replaces all entries
    /// atomically. Used by granular add/update/remove ops to serialise
    /// concurrent mutations on the same personality.
    async fn set_wake_entries_within(
        &self,
        owner: &Owner,
        personality_instance_id: PersonalityInstanceId,
        mutate: WakeEntriesMutator,
    ) -> Result<SetWakeEntriesResponse, StorageError>;

    /// Active WakeEntry rows plus their cursor positions.
    async fn list_active_wake_entries(&self) -> Result<Vec<WakeDispatchEntryRow>, StorageError>;

    async fn list_change_events_after(
        &self,
        owner: &Owner,
        after: uuid::Uuid,
        limit: usize,
    ) -> Result<Vec<ChangeEventForWake>, StorageError>;

    async fn advance_wake_cursor(
        &self,
        owner: &Owner,
        instance: PersonalityInstanceId,
        last_considered_seq: uuid::Uuid,
    ) -> Result<(), StorageError>;

    async fn try_begin_wake_invocation(
        &self,
        owner: &Owner,
        instance: PersonalityInstanceId,
        wake_entry_id: uuid::Uuid,
        change_event_seq: uuid::Uuid,
    ) -> Result<bool, StorageError>;

    async fn start_wake_invocation(
        &self,
        start: &WakeInvocationStart,
    ) -> Result<bool, StorageError>;

    #[allow(clippy::too_many_arguments)]
    async fn finish_wake_invocation(
        &self,
        owner: &Owner,
        instance: PersonalityInstanceId,
        wake_entry_id: uuid::Uuid,
        change_event_seq: uuid::Uuid,
        status: WakeInvocationStatus,
        turn_count: u16,
        cost_usd: f64,
    ) -> Result<(), StorageError>;

    async fn finalize_wake_invocation(
        &self,
        finalize: &WakeInvocationFinalize,
    ) -> Result<(), StorageError>;

    async fn append_wake_invocation_log(
        &self,
        _log: &WakeInvocationLogDraft,
    ) -> Result<(), StorageError> {
        Ok(())
    }

    async fn list_wake_invocations(
        &self,
        _req: &ListWakeInvocationsRequest,
    ) -> Result<Vec<WakeInvocationRow>, StorageError> {
        Ok(Vec::new())
    }

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

    /// Owner-scoped fetch of one personality runtime row by instance id.
    /// Returns the personality row plus its current root-perspective memory
    /// id and display name. Used by the wake-context assembler so the
    /// engine reads the freshest root perspective per wake.
    async fn fetch_personality_runtime(
        &self,
        owner: &Owner,
        instance_id: PersonalityInstanceId,
    ) -> Result<Option<PersonalityRuntimeRow>, StorageError>;

    /// Owner-scoped fetch of the root-perspective sidecar (display_name,
    /// purpose) for a given memory_id. Used to populate the
    /// `root_perspective` parameter passed to recipes.
    async fn fetch_root_personality_perspective(
        &self,
        owner: &Owner,
        memory_id: crate::MemoryId,
    ) -> Result<Option<RootPersonalityPerspectiveRow>, StorageError>;

    /// Owner-scoped fetch of one change event by `seq` plus the personality
    /// authorship and wake-chain depth columns the wake-context assembler
    /// needs. Returns `None` when no row matches.
    async fn fetch_change_event_for_wake(
        &self,
        owner: &Owner,
        seq: uuid::Uuid,
    ) -> Result<Option<ChangeEventForWake>, StorageError>;

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

    async fn register_inference_target(
        &self,
        _req: &RegisterInferenceTargetRequest,
    ) -> Result<RegisterInferenceTargetResponse, StorageError> {
        unimplemented!("noop storage")
    }

    async fn list_inference_targets(
        &self,
        _owner: &Owner,
    ) -> Result<Vec<InferenceTargetRow>, StorageError> {
        unimplemented!("noop storage")
    }

    async fn remove_inference_target(
        &self,
        _req: &RemoveInferenceTargetRequest,
    ) -> Result<RemoveInferenceTargetResponse, StorageError> {
        unimplemented!("noop storage")
    }

    async fn bind_inference_tier(
        &self,
        _req: &BindInferenceTierRequest,
    ) -> Result<BindInferenceTierResponse, StorageError> {
        unimplemented!("noop storage")
    }

    async fn unbind_inference_tier(
        &self,
        _owner: &Owner,
        _tier: crate::ModelTier,
    ) -> Result<(), StorageError> {
        unimplemented!("noop storage")
    }

    async fn list_inference_tier_bindings(
        &self,
        _owner: &Owner,
    ) -> Result<Vec<InferenceTierBindingRow>, StorageError> {
        unimplemented!("noop storage")
    }

    async fn list_personality_instances(
        &self,
        _owner: &Owner,
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
    ) -> Result<InstantiatePersonalityResponse, StorageError> {
        Err(StorageError::Internal("NoopStorage rejects writes".into()))
    }

    async fn ensure_master_token_personality(
        &self,
        _owner: &Owner,
        _master_token_id: uuid::Uuid,
    ) -> Result<MasterTokenPersonality, StorageError> {
        Err(StorageError::Internal(
            "mock: ensure_master_token_personality not stubbed".into(),
        ))
    }

    async fn set_wake_entries(
        &self,
        _req: &SetWakeEntriesRequest,
    ) -> Result<SetWakeEntriesResponse, StorageError> {
        Err(StorageError::Internal("NoopStorage rejects writes".into()))
    }

    async fn set_wake_entries_within(
        &self,
        _owner: &Owner,
        _personality_instance_id: PersonalityInstanceId,
        _mutate: WakeEntriesMutator,
    ) -> Result<SetWakeEntriesResponse, StorageError> {
        Err(StorageError::Internal("NoopStorage rejects writes".into()))
    }

    async fn list_active_wake_entries(&self) -> Result<Vec<WakeDispatchEntryRow>, StorageError> {
        Ok(Vec::new())
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
        _instance: PersonalityInstanceId,
        _last_considered_seq: uuid::Uuid,
    ) -> Result<(), StorageError> {
        Ok(())
    }

    async fn try_begin_wake_invocation(
        &self,
        _owner: &Owner,
        _instance: PersonalityInstanceId,
        _wake_entry_id: uuid::Uuid,
        _change_event_seq: uuid::Uuid,
    ) -> Result<bool, StorageError> {
        Ok(false)
    }

    async fn start_wake_invocation(
        &self,
        _start: &WakeInvocationStart,
    ) -> Result<bool, StorageError> {
        Ok(false)
    }

    async fn finish_wake_invocation(
        &self,
        _owner: &Owner,
        _instance: PersonalityInstanceId,
        _wake_entry_id: uuid::Uuid,
        _change_event_seq: uuid::Uuid,
        _status: WakeInvocationStatus,
        _turn_count: u16,
        _cost_usd: f64,
    ) -> Result<(), StorageError> {
        Ok(())
    }

    async fn finalize_wake_invocation(
        &self,
        _finalize: &WakeInvocationFinalize,
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

    async fn fetch_personality_runtime(
        &self,
        _owner: &Owner,
        _instance_id: PersonalityInstanceId,
    ) -> Result<Option<PersonalityRuntimeRow>, StorageError> {
        Ok(None)
    }

    async fn fetch_root_personality_perspective(
        &self,
        _owner: &Owner,
        _memory_id: crate::MemoryId,
    ) -> Result<Option<RootPersonalityPerspectiveRow>, StorageError> {
        Ok(None)
    }

    async fn fetch_change_event_for_wake(
        &self,
        _owner: &Owner,
        _seq: uuid::Uuid,
    ) -> Result<Option<ChangeEventForWake>, StorageError> {
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

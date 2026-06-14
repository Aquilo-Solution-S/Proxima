//! Abstract storage trait — backend-neutral surface for the
//! engine.
//!
//! See docs/07-storage.md and AGENTS.md invariants 2, 3, 5.

use std::sync::Arc;

use crate::GoalId;
use crate::SourceBatchId;
use crate::approval::ApprovalStore;
use crate::chat::ChatStore;
use crate::dependency::MemoryDependency;
use crate::personality::WakeEntryDraft;
use crate::personality::{
    AbstractionRow, ActiveGoalSummary, ChangeEventForWake, InstantiatePersonalityRequest,
    InstantiatePersonalityResponse, ListReadScopeRequest, ListReadScopeResponse, MemorySnapshot,
    PersonalityInstanceId, PersonalityInstanceRow, PersonalityRef, PersonalityWriteOutcome,
    PersonalityWriteRequest, SetReadScopeRequest, SetReadScopeResponse, SetWakeEntriesRequest,
    SetWakeEntriesResponse, SidecarSpec, TombstonePersonalityRequest, TombstonePersonalityResponse,
    WakeDispatchEntryRow,
};
use crate::verbs::close_batch::CloseBatchOutcome;
use crate::verbs::event_history::{EventHistoryRequest, EventHistoryResponse};
use crate::verbs::event_ingest::{EventDraft, EventIngestOutcome};
use crate::verbs::goal_write::{GoalDraft, GoalWriteOutcome};
use crate::verbs::persist_mcp_call::{McpCallLogInput, McpCallLogOutcome};
use crate::verbs::subscribe::ChangeEventStream;
use crate::{Owner, Principal};

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

/// Boxed closure for read-modify-write on `WakeEntry` rows.
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
pub trait Storage: ApprovalStore + ChatStore + Send + Sync {
    /// Atomic Fact materialization per docs/14 §`EventIngest`.
    /// Single transaction inserting `cited_object`, event,
    /// memory(Fact), `citation_mapping`, `change_event`. Replay
    /// (`event_id` collision) returns the original outcome with
    /// `idempotent_replay = true`.
    ///
    /// # Errors
    ///
    /// Constraint violations map to `ConstraintViolation`; sqlx
    /// failures map to Internal.
    async fn ingest_event_atomic(
        &self,
        draft: &EventDraft,
    ) -> Result<EventIngestOutcome, StorageError>;

    /// Atomic MCP-call activity materialization. One transaction writes
    /// the call Fact, inline I/O `CitedObject`, `CitationMapping`, typed
    /// sidecars, and entity change event. Whole-verb replay returns the
    /// original ids with `idempotent_replay = true`.
    async fn persist_mcp_call_atomic(
        &self,
        input: &McpCallLogInput,
    ) -> Result<McpCallLogOutcome, StorageError>;

    /// Atomic Goal write per docs/14 §`GoalWrite`.
    /// Single transaction inserting goal, `goal_parents`,
    /// `change_event`. Replay (`request_id` collision with same body)
    /// returns the original outcome with `idempotent_replay = true`.
    ///
    /// # Errors
    ///
    /// Constraint violations map to `ConstraintViolation` (including
    /// `idempotency_conflict`: prefix for `request_id` reuse with
    /// different body); `NotFound` if referenced parent missing;
    /// sqlx failures map to Internal.
    async fn write_goal_atomic(&self, draft: &GoalDraft) -> Result<GoalWriteOutcome, StorageError>;

    /// Atomic Goal supersede per docs/14 §`GoalWrite`.
    /// Single transaction inserting new goal with supersedes=prior,
    /// `goal_parents`, `change_event`. Replay check same as `write_goal`.
    ///
    /// # Errors
    ///
    /// Same as `write_goal_atomic`, plus `NotFound` if prior goal
    /// doesn't exist, `ConstraintViolation` if prior owner doesn't
    /// match draft.owner.
    async fn supersede_goal_atomic(
        &self,
        prior: GoalId,
        draft: &GoalDraft,
    ) -> Result<GoalWriteOutcome, StorageError>;

    /// Returns an owner-filtered stream of `ChangeEvents`. If
    /// `since` is `Some(seq)`, the stream begins by replaying
    /// rows whose `seq > since` and then attaches to live events.
    /// If `None`, the live stream begins immediately (no
    /// backfill).
    ///
    /// Per docs/14 §Cursor: at-least-once delivery; clients
    /// dedupe by `seq`. The server does NOT dedupe.
    async fn subscribe_changes(
        &self,
        principal: &Principal,
        since: Option<uuid::Uuid>,
    ) -> Result<ChangeEventStream, StorageError>;

    /// Owner-scoped bounded read of `change_event` rows, newest-first.
    /// Server clamps `limit` to `MAX_EVENT_HISTORY_LIMIT`. When
    /// `before` is `Some(seq)`, returns rows with `seq < before`.
    /// `seq_high_water` is the latest seq in the owner's `change_event`
    /// log at read time (cursor for a follow-up Subscribe).
    async fn event_history(
        &self,
        req: &EventHistoryRequest,
    ) -> Result<EventHistoryResponse, StorageError>;

    /// Owner-scoped snapshot read of memories per docs/14 §"Query".
    /// Returns `MemoryRow` substrate shape with payload bytes projected
    /// from sidecar tables. `schemas` is the list of registered schemas
    /// with sidecar tables for dynamic JOIN construction.
    async fn query_memories(
        &self,
        req: &crate::verbs::query::QueryRequest,
        schemas: &[crate::verbs::schema::SchemaInfo],
    ) -> Result<crate::verbs::query::QueryResponse, StorageError>;

    /// Owner-scoped lexical/semantic memory search. Similarity is
    /// query-time only; this method never writes edges.
    async fn search_memories(
        &self,
        req: &crate::verbs::query::MemorySearchRequest,
        projections: &[crate::verbs::schema::MemorySearchProjection],
    ) -> Result<Vec<crate::verbs::query::MemorySearchResult>, StorageError>;

    /// Owner-scoped bounded walk over memory-only Provenance and
    /// Supersession edges. Does not traverse Goals or write edges.
    async fn walk_memory_lineage(
        &self,
        req: &crate::verbs::query::MemoryLineageRequest,
    ) -> Result<crate::verbs::query::MemoryLineageResponse, StorageError>;

    /// Owner-scoped active Goal query for one personality Self-Perspective.
    /// Traverses `core/inspires` edges authored at proposal/attachment time,
    /// follows Goal supersession forward, and returns only current Active
    /// heads. No `GoalConnection` sidecar is modeled.
    async fn list_active_goals(
        &self,
        principal: &Principal,
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
        principal: &Principal,
        source_batch_id: SourceBatchId,
    ) -> Result<CloseBatchOutcome, StorageError>;

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

    /// Replace active `WakeEntry` rows for one personality instance.
    async fn set_wake_entries(
        &self,
        req: &SetWakeEntriesRequest,
    ) -> Result<SetWakeEntriesResponse, StorageError>;

    /// Transactional read-modify-write over a personality's `WakeConfig`.
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

    /// List explicit read-scope grants for one reader personality. Identity
    /// reads are implicit and are not returned.
    async fn list_read_scope(
        &self,
        req: &ListReadScopeRequest,
    ) -> Result<ListReadScopeResponse, StorageError>;

    /// Replace explicit read-scope grants for one reader personality. Identity
    /// reads remain implicit even when omitted.
    async fn set_read_scope(
        &self,
        req: &SetReadScopeRequest,
    ) -> Result<SetReadScopeResponse, StorageError>;

    /// Active `WakeEntry` rows plus their cursor positions.
    async fn list_active_wake_entries(&self) -> Result<Vec<WakeDispatchEntryRow>, StorageError>;

    async fn list_change_events_after(
        &self,
        owner: &Owner,
        after: uuid::Uuid,
        limit: usize,
    ) -> Result<Vec<ChangeEventForWake>, StorageError>;

    async fn list_change_events_for_replay(
        &self,
        owner: &Owner,
        after: uuid::Uuid,
        until: Option<uuid::Uuid>,
        limit: usize,
    ) -> Result<Vec<ChangeEventForWake>, StorageError> {
        let rows = self.list_change_events_after(owner, after, limit).await?;
        Ok(rows
            .into_iter()
            .filter(|row| until.is_none_or(|until| row.event.seq <= until))
            .collect())
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

    async fn load_perspective_heads(
        &self,
        owner: &Owner,
        instance: PersonalityInstanceId,
        root_perspective_memory_id: crate::MemoryId,
        sidecars: &[SidecarSpec],
        limit: usize,
    ) -> Result<Vec<MemorySnapshot>, StorageError>;

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
        reader_personality_instance_id: Option<PersonalityInstanceId>,
        sidecars: &[SidecarSpec],
    ) -> Result<Option<MemorySnapshot>, StorageError>;

    async fn list_memory_dependencies(
        &self,
        _owner: &Owner,
        _source_memory_id: crate::MemoryId,
    ) -> Result<Vec<MemoryDependency>, StorageError> {
        Ok(Vec::new())
    }

    async fn has_satisfied_code_test_request(
        &self,
        _owner: &Owner,
        _test_request_memory_id: crate::MemoryId,
    ) -> Result<bool, StorageError> {
        Ok(false)
    }
}

pub type StorageHandle = Arc<dyn Storage>;

/// Storage that rejects all writes — used by the in-memory
/// demo path and by tests that don't want PG.
#[derive(Debug, Default, Clone)]
pub struct NoopStorage;

/// `NoopStorage` rejects all writes; the `ApprovalStore` / `ChatStore`
/// default bodies (errors / empty reads) are exactly that behavior.
impl ApprovalStore for NoopStorage {}
impl ChatStore for NoopStorage {}

#[async_trait::async_trait]
impl Storage for NoopStorage {
    async fn ingest_event_atomic(
        &self,
        _draft: &EventDraft,
    ) -> Result<EventIngestOutcome, StorageError> {
        Err(StorageError::Internal("NoopStorage rejects writes".into()))
    }

    async fn persist_mcp_call_atomic(
        &self,
        _input: &McpCallLogInput,
    ) -> Result<McpCallLogOutcome, StorageError> {
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
        _principal: &Principal,
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

    async fn search_memories(
        &self,
        _req: &crate::verbs::query::MemorySearchRequest,
        _projections: &[crate::verbs::schema::MemorySearchProjection],
    ) -> Result<Vec<crate::verbs::query::MemorySearchResult>, StorageError> {
        Ok(Vec::new())
    }

    async fn walk_memory_lineage(
        &self,
        _req: &crate::verbs::query::MemoryLineageRequest,
    ) -> Result<crate::verbs::query::MemoryLineageResponse, StorageError> {
        Ok(crate::verbs::query::MemoryLineageResponse {
            nodes: Vec::new(),
            edges: Vec::new(),
            truncated: false,
        })
    }

    async fn list_active_goals(
        &self,
        _principal: &Principal,
        _self_perspective_memory_id: crate::MemoryId,
        _limit: usize,
    ) -> Result<Vec<ActiveGoalSummary>, StorageError> {
        Ok(Vec::new())
    }

    async fn close_batch(
        &self,
        _principal: &Principal,
        _source_batch_id: SourceBatchId,
    ) -> Result<CloseBatchOutcome, StorageError> {
        Err(StorageError::Internal("NoopStorage rejects writes".into()))
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

    async fn list_read_scope(
        &self,
        _req: &ListReadScopeRequest,
    ) -> Result<ListReadScopeResponse, StorageError> {
        Ok(ListReadScopeResponse {
            readable_personality_instance_ids: Vec::new(),
        })
    }

    async fn set_read_scope(
        &self,
        _req: &SetReadScopeRequest,
    ) -> Result<SetReadScopeResponse, StorageError> {
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

    async fn load_perspective_heads(
        &self,
        _owner: &Owner,
        _instance: PersonalityInstanceId,
        _root_perspective_memory_id: crate::MemoryId,
        _sidecars: &[SidecarSpec],
        _limit: usize,
    ) -> Result<Vec<MemorySnapshot>, StorageError> {
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
        _reader_personality_instance_id: Option<PersonalityInstanceId>,
        _sidecars: &[SidecarSpec],
    ) -> Result<Option<MemorySnapshot>, StorageError> {
        Ok(None)
    }
}

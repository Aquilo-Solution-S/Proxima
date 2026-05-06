//! Abstract storage trait — backend-neutral surface for the
//! engine.
//!
//! See docs/07-storage.md and AGENTS.md invariants 2, 3, 5.

use std::sync::Arc;

use crate::GoalId;
use crate::Owner;
use crate::SourceBatchId;
use crate::operators::{
    A2PInvocationKey, A2PLineageKey, AbstractionRow, ConsolidateA2POutcome, ConsolidateA2PRequest,
    ConsolidateBatchF2AOutcome, ConsolidateBatchF2ARequest, F2AInvocationKey, FactRow, SidecarSpec,
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

    /// Load all Facts in a closed source-batch with their typed
    /// sidecar payloads serialized to JSON. The engine builds
    /// `sidecars` from its `FlavorRegistryFrozen` (Fact schemas with a
    /// declared `sidecar_table`); storage emits one
    /// `row_to_json(s.*)` join per spec and unions the rows.
    ///
    /// Used by the F→A dispatcher (M5+) — the operator's `run()`
    /// receives these rows pre-loaded.
    ///
    /// # Errors
    ///
    /// Returns `NotFound` when the batch doesn't exist for `owner`;
    /// `Internal` on sqlx failure.
    async fn load_batch_facts(
        &self,
        owner: &Owner,
        batch_id: SourceBatchId,
        sidecars: &[SidecarSpec],
    ) -> Result<Vec<FactRow>, StorageError>;

    /// Atomic F→A consolidation persistence. Inserts N memory rows
    /// (Abstractions) + N typed sidecar rows + M provenance edges +
    /// N embedding rows + outbox change_events + the
    /// `source_batch_f2a` dedup row, all in a single transaction.
    /// Idempotent on `(batch_id, operator_id, prompt_version,
    /// model_id, personality_id)` — a re-call with the row already present returns
    /// `already_consolidated = true` without writing. Re-running with
    /// a different prompt, model, or personality id is a new
    /// invocation.
    ///
    /// # Errors
    ///
    /// Returns `ConstraintViolation` for schema/owner-mismatch
    /// violations; `Internal` on sqlx failure.
    async fn consolidate_batch_f2a(
        &self,
        req: &ConsolidateBatchF2ARequest<'_>,
    ) -> Result<ConsolidateBatchF2AOutcome, StorageError>;

    /// List `source_batches` for `owner` that are closed
    /// (`closed_at IS NOT NULL`) and have no `source_batch_f2a` row
    /// for this exact invocation key. The Engine's dispatcher uses
    /// this to "catch up" — running F→A against any batch that the
    /// source closed without going through the auth-gated
    /// `Engine::close_batch` surface (M4-era `LocalGitSource` is such
    /// a caller).
    ///
    /// # Errors
    ///
    /// `Internal` on sqlx failure.
    async fn list_unconsolidated_batches(
        &self,
        owner: &Owner,
        key: &F2AInvocationKey<'_>,
    ) -> Result<Vec<SourceBatchId>, StorageError>;

    /// Load current Abstraction heads for A→P operator input.
    async fn load_a2p_abstractions(
        &self,
        owner: &Owner,
        sidecars: &[SidecarSpec],
        limit: usize,
    ) -> Result<Vec<AbstractionRow>, StorageError>;

    /// Atomic A→P consolidation persistence. Writes zero or more
    /// Perspective rows and always records the invocation key.
    async fn consolidate_a2p(
        &self,
        req: &ConsolidateA2PRequest<'_>,
    ) -> Result<ConsolidateA2POutcome, StorageError>;

    /// Checks full A→P invocation idempotence.
    async fn has_a2p_invocation(
        &self,
        owner: &Owner,
        key: &A2PInvocationKey<'_>,
    ) -> Result<bool, StorageError>;

    /// Most recent Perspective head for this operator/model/personality
    /// lineage, ignoring context/input shifts.
    async fn lookup_prior_a2p_head(
        &self,
        owner: &Owner,
        key: &A2PLineageKey<'_>,
    ) -> Result<Option<crate::MemoryId>, StorageError>;
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

    async fn close_batch(
        &self,
        _owner: &Owner,
        _source_batch_id: SourceBatchId,
    ) -> Result<CloseBatchOutcome, StorageError> {
        Err(StorageError::Internal("NoopStorage rejects writes".into()))
    }

    async fn load_batch_facts(
        &self,
        _owner: &Owner,
        _batch_id: SourceBatchId,
        _sidecars: &[SidecarSpec],
    ) -> Result<Vec<FactRow>, StorageError> {
        Ok(Vec::new())
    }

    async fn consolidate_batch_f2a(
        &self,
        _req: &ConsolidateBatchF2ARequest<'_>,
    ) -> Result<ConsolidateBatchF2AOutcome, StorageError> {
        Err(StorageError::Internal("NoopStorage rejects writes".into()))
    }

    async fn list_unconsolidated_batches(
        &self,
        _owner: &Owner,
        _key: &F2AInvocationKey<'_>,
    ) -> Result<Vec<SourceBatchId>, StorageError> {
        Ok(Vec::new())
    }

    async fn load_a2p_abstractions(
        &self,
        _owner: &Owner,
        _sidecars: &[SidecarSpec],
        _limit: usize,
    ) -> Result<Vec<AbstractionRow>, StorageError> {
        Ok(Vec::new())
    }

    async fn consolidate_a2p(
        &self,
        _req: &ConsolidateA2PRequest<'_>,
    ) -> Result<ConsolidateA2POutcome, StorageError> {
        Err(StorageError::Internal("NoopStorage rejects writes".into()))
    }

    async fn has_a2p_invocation(
        &self,
        _owner: &Owner,
        _key: &A2PInvocationKey<'_>,
    ) -> Result<bool, StorageError> {
        Ok(false)
    }

    async fn lookup_prior_a2p_head(
        &self,
        _owner: &Owner,
        _key: &A2PLineageKey<'_>,
    ) -> Result<Option<crate::MemoryId>, StorageError> {
        Ok(None)
    }
}

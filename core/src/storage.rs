//! Abstract storage trait — backend-neutral surface for the
//! engine.
//!
//! See docs/07-storage.md and AGENTS.md invariants 2, 3, 5.

use std::sync::Arc;

use crate::GoalId;
use crate::Owner;
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
}

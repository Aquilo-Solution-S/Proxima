use std::collections::HashSet;

use super::{
    EvidenceTarget, GoalId, MemoryId, Postgres, StorageError, Transaction, WakeWrite,
    goal_wake_matches, map_err,
};

pub(super) struct CreateGoalReplayExpectation<'a> {
    pub(super) goal_id: GoalId,
    pub(super) target_self_perspective_id: MemoryId,
    pub(super) author_self_perspective_id: Option<MemoryId>,
    pub(super) wake_write: WakeWrite<'a>,
    pub(super) expected_prior: Option<GoalId>,
    pub(super) request_id: &'a str,
}

/// The create-only half of replay verification: the rows a create writes that
/// no other Goal verb does. Evidence equality is checked for every verb by
/// the shared lifecycle tail, so it is deliberately not restated here.
pub(super) async fn ensure_create_goal_replay_side_effects_match(
    tx: &mut Transaction<'_, Postgres>,
    expected: CreateGoalReplayExpectation<'_>,
) -> Result<(), StorageError> {
    if !goal_self_assignment_matches(tx, expected.goal_id, expected.target_self_perspective_id)
        .await?
    {
        return Err(idempotency_conflict(expected.request_id));
    }
    let _ = expected.author_self_perspective_id;
    if !goal_wake_matches(
        tx,
        expected.goal_id,
        expected.wake_write,
        expected.expected_prior,
    )
    .await?
    {
        return Err(idempotency_conflict(expected.request_id));
    }
    Ok(())
}

async fn goal_self_assignment_matches(
    tx: &mut Transaction<'_, Postgres>,
    goal_id: GoalId,
    target_self_perspective_id: MemoryId,
) -> Result<bool, StorageError> {
    let stored: Option<Option<uuid::Uuid>> =
        sqlx::query_scalar("SELECT assignment_t FROM proxima_core.goal WHERE t = $1")
            .bind(goal_id.into_inner())
            .fetch_optional(&mut **tx)
            .await
            .map_err(map_err)?;
    Ok(stored.flatten() == Some(target_self_perspective_id.into_inner()))
}

/// Evidence equality on the Goal's own column, not on the index. The column
/// is the statement; the index rows are its consequence, so comparing the
/// column is comparing what the write actually claimed.
pub(super) async fn goal_evidence_matches(
    tx: &mut Transaction<'_, Postgres>,
    goal_id: GoalId,
    evidence: &[EvidenceTarget],
) -> Result<bool, StorageError> {
    let stored: Option<Vec<uuid::Uuid>> =
        sqlx::query_scalar("SELECT evidence_t FROM proxima_core.goal WHERE t = $1")
            .bind(goal_id.into_inner())
            .fetch_optional(&mut **tx)
            .await
            .map_err(map_err)?;
    let stored = stored
        .unwrap_or_default()
        .into_iter()
        .collect::<HashSet<_>>();
    let requested = evidence
        .iter()
        .map(|target| target.memory_id.into_inner())
        .collect::<HashSet<_>>();
    Ok(stored == requested)
}

pub(super) fn idempotency_conflict(request_id: &str) -> StorageError {
    // The `Display` form is load-bearing: storage-level callers match on the
    // message `idempotency_conflict:{request_id}`. The engine matches the variant.
    StorageError::IdempotencyConflict {
        request_id: request_id.to_string(),
    }
}

use super::{
    EvidenceTarget, GoalId, GoalLifecycleFact, GoalWriteOutcome, InsertedGoal, MemoryId, Owner,
    Postgres, StorageError, Transaction, assert_goal_topology_references, goal_evidence_matches,
    goal_topology_edge_count, idempotency_conflict,
};

/// What a Goal write states about itself once its row is in place, and the
/// authority to record it: the same inputs for every verb, which is why the
/// tail below is one function rather than five.
pub(super) struct LifecycleWrite<'a> {
    pub(super) owner: &'a Owner,
    pub(super) inserted: InsertedGoal,
    pub(super) lifecycle: GoalLifecycleFact,
    pub(super) assignment: MemoryId,
    pub(super) dependencies: &'a [GoalId],
    /// What the write rests on, empty for the verbs that name nothing.
    pub(super) evidence: &'a [EvidenceTarget],
    /// The key this write was claimed under, for the replay conflict below.
    pub(super) request_id: &'a str,
}

/// The tail every Goal write shares: record the lifecycle transition as a
/// Fact, derive the reference rows the Goal's topology columns imply, and
/// report what was written.
///
/// A replay skips both — the rows are already there — and reports the counts
/// the declaration implies rather than re-reading them.
pub(super) async fn lifecycle_outcome(
    tx: &mut Transaction<'_, Postgres>,
    write: LifecycleWrite<'_>,
) -> Result<GoalWriteOutcome, StorageError> {
    if write.inserted.idempotent_replay {
        // The idempotency key is one namespace across every Goal verb, and
        // the body comparison that admitted this replay reads the content
        // columns — `evidence_memory_ids` is not one of them. Evidence is
        // half of what a write claimed, so a key that returns a row resting
        // on other evidence is a conflict, not a replay.
        if !goal_evidence_matches(tx, write.inserted.goal_id, write.evidence).await? {
            return Err(idempotency_conflict(write.request_id));
        }
        return Ok(replay_goal_outcome(
            &write.inserted,
            write.lifecycle,
            goal_topology_edge_count(write.dependencies, write.evidence),
        ));
    }
    // Goal `t` is the transition. No lifecycle Fact / sidecar.
    let lifecycle_memory_id = None;
    let edge_count = assert_goal_topology_references(
        tx,
        write.owner,
        write.inserted.goal_id,
        write.assignment,
        write.dependencies,
        write.evidence,
    )
    .await?;
    Ok(GoalWriteOutcome {
        goal_id: write.inserted.goal_id,
        change_event_seq: write.inserted.change_event_seq,
        lifecycle_memory_id,
        edge_count,
        idempotent_replay: false,
    })
}

fn replay_goal_outcome(
    inserted: &InsertedGoal,
    lifecycle: GoalLifecycleFact,
    edge_count: usize,
) -> GoalWriteOutcome {
    let _ = lifecycle;
    GoalWriteOutcome {
        goal_id: inserted.goal_id,
        change_event_seq: inserted.change_event_seq,
        lifecycle_memory_id: None,
        edge_count,
        idempotent_replay: true,
    }
}

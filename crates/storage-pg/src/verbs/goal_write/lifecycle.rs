use super::{
    EvidenceTarget, GoalId, GoalWriteOutcome, InsertedGoal, MemoryId, Owner, Postgres,
    StorageError, Transaction, assert_goal_topology_references, internal,
};

/// What a Goal write states about itself once its row is in place, and the
/// authority to record it: the same inputs for every verb, which is why the
/// tail below is one function rather than five.
pub(super) struct LifecycleWrite<'a> {
    pub(super) owner: &'a Owner,
    pub(super) inserted: InsertedGoal,
    pub(super) assignment: MemoryId,
    pub(super) dependencies: &'a [GoalId],
    /// What the write rests on, empty for the verbs that name nothing.
    pub(super) evidence: &'a [EvidenceTarget],
}

/// The tail every Goal write shares: record the lifecycle transition as a
/// Fact, derive the reference rows the Goal's topology columns imply, and
/// report what was written.
///
/// A replay is resolved from its persisted command declaration before this
/// tail. Reaching this function with one means a caller skipped that proof.
pub(super) async fn lifecycle_outcome(
    tx: &mut Transaction<'_, Postgres>,
    write: LifecycleWrite<'_>,
) -> Result<GoalWriteOutcome, StorageError> {
    if write.inserted.idempotent_replay {
        return Err(internal(
            "Goal replay reached lifecycle persistence without declaration proof",
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

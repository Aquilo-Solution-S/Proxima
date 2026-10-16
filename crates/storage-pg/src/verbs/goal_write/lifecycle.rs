use super::{
    EvidenceTarget, GoalId, GoalWriteOutcome, InsertedGoal, MemoryId, OwnerId, Postgres,
    StorageError, Transaction, assert_goal_topology_references, internal,
};

/// What a Goal write states about itself once its row is in place, and the
/// authority to record it: the same inputs for every verb, which is why the
/// tail below is one function rather than five.
pub(super) struct LifecycleWrite<'a> {
    pub(super) owner: &'a OwnerId,
    pub(super) inserted: InsertedGoal,
    pub(super) assignment: MemoryId,
    pub(super) dependencies: &'a [GoalId],
    /// What the write rests on, empty for the verbs that name nothing.
    pub(super) evidence: &'a [EvidenceTarget],
}

/// The tail every Goal write shares: assert the reference rows the Goal's
/// topology columns imply and report what was written.
pub(super) async fn lifecycle_outcome(
    tx: &mut Transaction<'_, Postgres>,
    write: LifecycleWrite<'_>,
) -> Result<GoalWriteOutcome, StorageError> {
    if write.inserted.idempotent_replay {
        return Err(internal(
            "Goal replay reached lifecycle persistence without declaration proof",
        ));
    }
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
        edge_count,
        idempotent_replay: false,
    })
}

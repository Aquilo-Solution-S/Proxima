use super::{
    EdgeEndpoint, EdgeKind, EntityKind, EvidenceTarget, GoalId, MemoryId, Owner, Postgres,
    StorageError, Transaction, assert_index_rows_in_tx,
};

/// The reference rows a Goal's own topology implies.
///
/// The Goal row is the home of all three statements — the Perspective it
/// inspires, the Goals it waits on, the memories it rests on — so this is a
/// derivation from columns, not an independent write. Drop the index and
/// re-run this over the goals table and you get the same set back, which is
/// what E7 asks for.
///
/// Nothing here names a kind: every entry is a reference because a reference
/// is what a declared pointer from one node to another IS.
pub(super) async fn assert_goal_topology_references(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    goal_id: GoalId,
    assignment: MemoryId,
    dependencies: &[GoalId],
    evidence: &[EvidenceTarget],
) -> Result<usize, StorageError> {
    let mut targets = Vec::with_capacity(1 + dependencies.len() + evidence.len());
    targets.push(EdgeEndpoint::memory(EntityKind::Perspective, assignment));
    targets.extend(dependencies.iter().copied().map(EdgeEndpoint::goal));
    targets.extend(
        evidence
            .iter()
            .map(|target| EdgeEndpoint::memory(target.kind, target.memory_id)),
    );
    assert_index_rows_in_tx(
        tx.as_mut(),
        owner,
        EdgeEndpoint::goal(goal_id),
        EdgeKind::Reference,
        &targets,
    )
    .await
}

/// How many index rows a Goal write asserts, computed from the declaration
/// rather than from the table — a replay asserts the same rows and must
/// report the same count.
pub(super) fn goal_topology_edge_count(
    dependencies: &[GoalId],
    evidence: &[EvidenceTarget],
) -> usize {
    1 + dependencies.len() + evidence.len()
}

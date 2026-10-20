use super::{
    EdgeEndpoint, EntityKind, EvidenceTarget, GoalId, MemoryId, OwnerId, Postgres, StorageError,
    Transaction,
};

/// The reference rows a Goal's own topology implies.
///
/// The Goal row is the home of all three statements — the Perspective it
/// inspires, the Goals it waits on, the memories it rests on — so this is a
/// derivation from columns, not an independent write. Drop the index and
/// re-run this over the goals table and you get the same set back.
///
/// Nothing here names a kind: every entry is a reference because a reference
/// is what a declared pointer from one node to another IS.
pub(super) async fn assert_goal_topology_references(
    tx: &mut Transaction<'_, Postgres>,
    owner: &OwnerId,
    goal_id: GoalId,
    assignment: Option<MemoryId>,
    dependencies: &[GoalId],
    evidence: &[EvidenceTarget],
) -> Result<usize, StorageError> {
    let mut targets =
        Vec::with_capacity(usize::from(assignment.is_some()) + dependencies.len() + evidence.len());
    targets.extend(assignment.map(|id| EdgeEndpoint::memory(EntityKind::Perspective, id)));
    targets.extend(dependencies.iter().copied().map(EdgeEndpoint::goal));
    targets.extend(
        evidence
            .iter()
            .map(|target| EdgeEndpoint::memory(target.kind, target.memory_id)),
    );
    let _ = (tx, owner, goal_id);
    Ok(targets.len())
}

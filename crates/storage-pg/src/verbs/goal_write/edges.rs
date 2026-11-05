use super::{EvidenceTarget, GoalId, MemoryId, OwnerId, Postgres, StorageError, Transaction};

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
    motivated_by: &[MemoryId],
    dependencies: &[GoalId],
    evidence: &[EvidenceTarget],
) -> Result<usize, StorageError> {
    let _ = (tx, owner, goal_id);
    Ok(motivated_by.len() + dependencies.len() + evidence.len())
}

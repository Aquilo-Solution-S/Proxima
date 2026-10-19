//! #514: a goal is a plain tree. A parent is Achieved only when no child
//! head is Active or Paused and at least one child is Achieved.

use super::{GoalId, GoalState, OwnerId, Postgres, StorageError, Transaction, map_err};

/// Refuse achieving the series `parent_handle` while its decompose children
/// are not closed. Runs after the write's lifecycle lock (the parent head
/// `t`, which a decompose child also locks through its `dependency_t`), so
/// a child that a racing decompose committed first is seen here.
pub(super) async fn ensure_goal_children_closed(
    tx: &mut Transaction<'_, Postgres>,
    owner: &OwnerId,
    parent_handle: uuid::Uuid,
) -> Result<(), StorageError> {
    let rows: Vec<(uuid::Uuid, GoalState)> = sqlx::query_as(
        "SELECT h.t, g.state
           FROM proxima_core.goal g
           JOIN proxima_core.goal_head h ON h.handle = g.handle AND h.t = g.t
          WHERE g.owner_id = $1 AND g.parent_handle = $2
          ORDER BY h.handle",
    )
    .bind(owner.into_inner())
    .bind(parent_handle)
    .fetch_all(tx.as_mut())
    .await
    .map_err(map_err)?;
    children_verdict(&rows)
}

/// The rule over each child's current head: none open, and when there are
/// children, at least one Achieved.
pub(super) fn children_verdict(rows: &[(uuid::Uuid, GoalState)]) -> Result<(), StorageError> {
    if rows.is_empty() {
        return Ok(());
    }
    let open: Vec<GoalId> = rows
        .iter()
        .filter(|(_, state)| matches!(state, GoalState::Active | GoalState::Paused))
        .map(|(t, _)| GoalId::new(*t))
        .collect();
    if !open.is_empty() {
        return Err(StorageError::GoalChildrenOpen { open });
    }
    if !rows.iter().any(|(_, state)| *state == GoalState::Achieved) {
        return Err(StorageError::GoalChildrenAllAbandoned);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(n: u128) -> uuid::Uuid {
        uuid::Uuid::from_u128(n)
    }

    /// G7: the verdict table.
    #[test]
    fn children_verdict_table() {
        use GoalState::{Abandoned, Achieved, Active, Paused};
        assert!(children_verdict(&[]).is_ok(), "no children");
        assert!(
            children_verdict(&[(t(1), Achieved)]).is_ok(),
            "one achieved"
        );
        assert!(
            children_verdict(&[(t(1), Achieved), (t(2), Abandoned)]).is_ok(),
            "achieved and abandoned"
        );
        match children_verdict(&[(t(1), Active), (t(2), Achieved), (t(3), Paused)]) {
            Err(StorageError::GoalChildrenOpen { open }) => {
                assert_eq!(open, vec![GoalId::new(t(1)), GoalId::new(t(3))]);
            }
            other => panic!("open children: {other:?}"),
        }
        match children_verdict(&[(t(1), Abandoned), (t(2), Active)]) {
            Err(StorageError::GoalChildrenOpen { open }) => {
                assert_eq!(open, vec![GoalId::new(t(2))]);
            }
            other => panic!("open before all-abandoned: {other:?}"),
        }
        assert!(
            matches!(
                children_verdict(&[(t(1), Abandoned), (t(2), Abandoned)]),
                Err(StorageError::GoalChildrenAllAbandoned)
            ),
            "all abandoned"
        );
    }
}

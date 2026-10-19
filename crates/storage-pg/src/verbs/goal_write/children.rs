//! #514: a goal is a plain tree, and an open goal (Active or Paused) never
//! sits under a closed one (Achieved or Abandoned). Achieve and abandon check
//! it after the write's lifecycle lock: no child head may be open, and
//! achieve also needs at least one Achieved child.

use super::{GoalId, GoalState, OwnerId, Postgres, StorageError, Transaction, map_err};

/// Which children rule a closing write checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ChildRule {
    /// Abandon: no child head is Active or Paused.
    NoneOpen,
    /// Achieve: no child head is open, and at least one is Achieved.
    ClosedWithAchieved,
}

/// Refuse closing the series `parent_handle` while its decompose children
/// break `rule`. Runs after the write's lifecycle lock (the parent head
/// `t`, which a decompose child also locks through its `dependency_t`), so
/// a child that a racing decompose committed first is seen here.
pub(super) async fn ensure_goal_children_closed(
    tx: &mut Transaction<'_, Postgres>,
    owner: &OwnerId,
    parent_handle: uuid::Uuid,
    rule: ChildRule,
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
    match rule {
        ChildRule::NoneOpen => open_children(&rows),
        ChildRule::ClosedWithAchieved => children_verdict(&rows),
    }
}

/// No child head is Active or Paused; the open ones are named in child
/// handle order.
pub(super) fn open_children(rows: &[(uuid::Uuid, GoalState)]) -> Result<(), StorageError> {
    let open: Vec<GoalId> = rows
        .iter()
        .filter(|(_, state)| matches!(state, GoalState::Active | GoalState::Paused))
        .map(|(t, _)| GoalId::new(*t))
        .collect();
    if open.is_empty() {
        Ok(())
    } else {
        Err(StorageError::GoalChildrenOpen { open })
    }
}

/// The rule over each child's current head: none open, and when there are
/// children, at least one Achieved.
pub(super) fn children_verdict(rows: &[(uuid::Uuid, GoalState)]) -> Result<(), StorageError> {
    if rows.is_empty() {
        return Ok(());
    }
    open_children(rows)?;
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

    /// G7': the abandon rule. It passes wherever no child is open, including
    /// all-Abandoned, where the achieve rule refuses.
    #[test]
    fn open_children_table() {
        use GoalState::{Abandoned, Achieved, Active, Paused};
        assert!(open_children(&[]).is_ok(), "no children");
        assert!(open_children(&[(t(1), Achieved)]).is_ok(), "one achieved");
        assert!(
            open_children(&[(t(1), Achieved), (t(2), Abandoned)]).is_ok(),
            "achieved and abandoned"
        );
        let all_abandoned = [(t(1), Abandoned), (t(2), Abandoned)];
        assert!(open_children(&all_abandoned).is_ok(), "abandon passes");
        assert!(
            matches!(
                children_verdict(&all_abandoned),
                Err(StorageError::GoalChildrenAllAbandoned)
            ),
            "achieve refuses"
        );
        match open_children(&[(t(1), Paused), (t(2), Abandoned), (t(3), Active)]) {
            Err(StorageError::GoalChildrenOpen { open }) => {
                assert_eq!(open, vec![GoalId::new(t(1)), GoalId::new(t(3))]);
            }
            other => panic!("open children: {other:?}"),
        }
    }
}

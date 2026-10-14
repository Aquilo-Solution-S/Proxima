//! Current-memory counts for `proxima://memories/counts`: the heads, never
//! the versions.

use proxima_core::read_models::MemoryCounts;
use proxima_core::{EntityKind, OwnerId, StorageError};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::error::{internal, map_err};
use crate::pg_enums::PgMemoryKind;

/// One row per `memory_head` handle, so a stateful series counts once
/// however many versions it has, and one row per `goal_head` handle in any
/// state. `memory` is not read: its key is `(handle, t)`.
///
/// `read_owners` is the one owner predicate. The per-kind narrowing is the
/// owner RLS on `memory_head` and `goal_head` under the caller's bound scope;
/// no second per-kind filter is added here.
pub(crate) async fn count_memories_on_connection(
    connection: &mut PgConnection,
    read_owners: &[OwnerId],
) -> Result<MemoryCounts, StorageError> {
    let owner_ids: Vec<Uuid> = read_owners
        .iter()
        .copied()
        .map(OwnerId::into_inner)
        .collect();
    let mut counts = MemoryCounts::default();

    let heads: Vec<(PgMemoryKind, i64)> = sqlx::query_as(
        "SELECT kind, count(*)::bigint
           FROM proxima_core.memory_head
          WHERE owner_id = ANY($1::uuid[])
          GROUP BY kind",
    )
    .bind(&owner_ids)
    .fetch_all(&mut *connection)
    .await
    .map_err(map_err)?;
    for (kind, count) in heads {
        *counts.slot(EntityKind::from(kind)) += as_count(count)?;
    }

    let goals: i64 = sqlx::query_scalar(
        "SELECT count(*)::bigint
           FROM proxima_core.goal_head
          WHERE owner_id = ANY($1::uuid[])",
    )
    .bind(&owner_ids)
    .fetch_one(&mut *connection)
    .await
    .map_err(map_err)?;
    *counts.slot(EntityKind::Goal) += as_count(goals)?;
    Ok(counts)
}

fn as_count(count: i64) -> Result<u64, StorageError> {
    u64::try_from(count).map_err(|_| internal(format!("negative row count {count}")))
}

#[cfg(test)]
mod tests {
    use super::as_count;
    use proxima_core::StorageError;

    /// `count(*)` arrives as a signed `bigint`; a negative one is refused as
    /// a backend fault rather than wrapped into a huge count.
    #[test]
    fn a_negative_row_count_is_refused_not_wrapped() {
        assert_eq!(as_count(0).expect("zero"), 0);
        assert_eq!(as_count(412).expect("positive"), 412);
        assert!(matches!(as_count(-1), Err(StorageError::Internal(_))));
    }
}

//! Current-memory counts for `proxima://memories/counts`: the heads, never
//! the versions.

use proxima_core::read_models::MemoryCounts;
use proxima_core::{EntityKind, OwnerId, StorageError};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::error::map_err;
use crate::pg_enums::PgMemoryKind;
use crate::sidecars::int_to_u64;

/// One row per `memory_head` handle, so a stateful series counts once
/// however many versions it has, and one row per `goal_head` handle in any
/// state. `memory` is read only to drop a series whose every version is
/// forgotten: its head stays on the newest forgotten version (the
/// `memory.handle` FK keeps it), and owner RLS hides that row.
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
           FROM proxima_core.memory_head h
          WHERE h.owner_id = ANY($1::uuid[])
            AND EXISTS (SELECT 1 FROM proxima_core.memory m WHERE m.t = h.t)
          GROUP BY kind",
    )
    .bind(&owner_ids)
    .fetch_all(&mut *connection)
    .await
    .map_err(map_err)?;
    for (kind, count) in heads {
        *counts.slot(EntityKind::from(kind)) += int_to_u64(count, "row count")?;
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
    *counts.slot(EntityKind::Goal) += int_to_u64(goals, "goal count")?;
    Ok(counts)
}

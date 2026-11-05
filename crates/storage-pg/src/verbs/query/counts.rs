//! Current-memory counts for `proxiima://memories/counts`: the heads, never
//! the versions.

use proxiima_core::read_models::MemoryCounts;
use proxiima_core::{EntityKind, OwnerId, StorageError};
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
        "WITH goal_visible AS MATERIALIZED (
             SELECT DISTINCT ON (memory_handle) memory_t, memory_handle, owner_id
               FROM proxiima_core.goal_visible_memory_ids() WHERE owner_id=ANY($1::uuid[])
              ORDER BY memory_handle, memory_t DESC
         ), candidates AS (
             SELECT h.handle, h.t, h.kind
               FROM proxiima_core.memory_head h
               JOIN proxiima_core.memory m ON m.t = h.t
              WHERE h.owner_id = ANY($1::uuid[])
             UNION ALL
             SELECT v.memory_handle, v.memory_t, m.kind
               FROM goal_visible v
               JOIN proxiima_core.memory m ON m.t = v.memory_t
         ), latest AS (
             SELECT DISTINCT ON (handle) handle, kind
               FROM candidates
              ORDER BY handle, t DESC
         )
         SELECT kind, count(*)::bigint FROM latest GROUP BY kind",
    )
    .bind(&owner_ids)
    .fetch_all(&mut *connection)
    .await
    .map_err(map_err)?;
    for (kind, count) in heads {
        *counts.slot(EntityKind::from(kind)) += int_to_u64(count, "row count")?;
    }

    let goals: i64 = sqlx::query_scalar(
        "WITH candidates AS (
             SELECT handle FROM proxiima_core.goal_head
              WHERE owner_id = ANY($1::uuid[])
             UNION
             SELECT h.handle
               FROM proxiima_core.goal_head h
               JOIN proxiima_core.goal_access_grant a
                 ON a.goal_handle = h.handle
                AND a.controller_generation = h.authority_generation
              WHERE h.owner_id=ANY($1::uuid[]) AND a.subject_id = NULLIF(current_setting('app.goal_subject', true), '')::uuid
                AND a.read_ceiling >= 4
                AND (a.expires_at IS NULL OR a.expires_at > statement_timestamp())
         ) SELECT count(*)::bigint FROM candidates",
    )
    .bind(&owner_ids)
    .fetch_one(&mut *connection)
    .await
    .map_err(map_err)?;
    *counts.slot(EntityKind::Goal) += int_to_u64(goals, "goal count")?;
    Ok(counts)
}

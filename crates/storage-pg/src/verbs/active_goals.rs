use proxima_core::read_models::ActiveGoalSummary;
use proxima_core::verbs::goal_write::GoalState;
use proxima_core::{GoalId, MemoryId, OwnerRef, StorageError};
use sqlx::PgPool;

use crate::error::map_err;
use crate::verbs::query::read_owner_columns;

pub(crate) async fn list_active_goals(
    pool: &PgPool,
    read_owners: &[OwnerRef],
    self_perspective_memory_id: MemoryId,
    limit: usize,
) -> Result<Vec<ActiveGoalSummary>, StorageError> {
    if read_owners.is_empty() {
        return Ok(Vec::new());
    }
    let (read_owner_kinds, read_owner_ids) = read_owner_columns(read_owners);

    // SQL-POLICY: fixed-fragment — the only interpolation is the shared
    // entity-owner-union constant; every value is bound.
    //
    // The head filter here is deliberately the *readable-successor*
    // variant, not the plain heads-only shape `query_goals` uses: this is
    // the caller's wake view, and a successor the caller cannot read must
    // not silently hide a goal they can.
    let rows: Vec<ActiveGoalRow> = sqlx::query_as(
        // The Goal knows the Perspective it inspires: the assignment is a
        // column on the row, so this reads the statement rather than the
        // index row derived from it.
        "WITH RECURSIVE linked_goals(goal_id) AS (
             SELECT g0.goal_id
               FROM proxima_core.goals g0
              WHERE g0.assignment_perspective_id = $3
                AND EXISTS (
                    SELECT 1
                      FROM proxima_core.memories tm
                      JOIN unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS t(kind, id)
                        ON tm.owner_kind = t.kind
                       AND tm.owner_id IS NOT DISTINCT FROM t.id
                     WHERE tm.memory_id = $3
                )
                AND EXISTS (
                    SELECT 1
                      FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id)
                     WHERE g0.owner_kind = s.kind
                       AND g0.owner_id IS NOT DISTINCT FROM s.id
                )
             UNION
             SELECT child.goal_id
               FROM proxima_core.goals child
               JOIN linked_goals prior ON child.supersedes = prior.goal_id
              WHERE EXISTS (
                    SELECT 1
                      FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id)
                     WHERE child.owner_kind = s.kind
                       AND child.owner_id IS NOT DISTINCT FROM s.id
                )
         )
         SELECT g.goal_id, g.title, ga.memory_id AS goal_activated_memory_id
           FROM proxima_core.goals g
           JOIN linked_goals linked ON linked.goal_id = g.goal_id
           JOIN proxima_core.goal_activated_v1 ga ON ga.goal_id = g.goal_id
          WHERE EXISTS (
                SELECT 1
                  FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id)
                 WHERE g.owner_kind = s.kind
                   AND g.owner_id IS NOT DISTINCT FROM s.id
            )
            AND g.state = $4
            AND NOT EXISTS (
                SELECT 1
                  FROM proxima_core.goals newer
                 WHERE newer.supersedes = g.goal_id
                   AND EXISTS (
                        SELECT 1
                          FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id)
                         WHERE newer.owner_kind = s.kind
                           AND newer.owner_id IS NOT DISTINCT FROM s.id
                   )
            )
          ORDER BY g.created_at DESC, g.goal_id DESC
          LIMIT $5",
    )
    .bind(&read_owner_kinds)
    .bind(&read_owner_ids)
    .bind(self_perspective_memory_id.into_inner())
    .bind(GoalState::Active)
    .bind(i64::try_from(limit).unwrap_or(i64::MAX))
    .fetch_all(pool)
    .await
    .map_err(map_err)?;

    Ok(rows
        .into_iter()
        .map(|row| ActiveGoalSummary {
            goal_id: GoalId::new(row.goal_id),
            goal_activated_memory_id: row.goal_activated_memory_id.map(MemoryId::new),
            title: row.title,
        })
        .collect())
}

#[derive(sqlx::FromRow)]
struct ActiveGoalRow {
    goal_id: uuid::Uuid,
    title: String,
    goal_activated_memory_id: Option<uuid::Uuid>,
}

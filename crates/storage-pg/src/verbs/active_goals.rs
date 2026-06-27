use proxima_core::personality::ActiveGoalSummary;
use proxima_core::verbs::goal_write::GoalState;
use proxima_core::{GoalId, MemoryId, OwnerPrincipalKind, Principal, StorageError};
use sqlx::PgPool;

use crate::error::internal;

pub(crate) async fn list_active_goals(
    pool: &PgPool,
    read_owners: &[Principal],
    self_perspective_memory_id: MemoryId,
    limit: usize,
) -> Result<Vec<ActiveGoalSummary>, StorageError> {
    if read_owners.is_empty() {
        return Ok(Vec::new());
    }
    let (read_owner_kinds, read_owner_ids) = read_owner_columns(read_owners);

    let sql = "WITH RECURSIVE linked_goals(goal_id) AS (
             SELECT e.source_goal_id
               FROM proxima_core.edges e
              WHERE e.relation = 'core/inspires'
                AND e.source_kind = 'Goal'
                AND e.source_goal_id IS NOT NULL
                AND e.target_kind = 'Perspective'
                AND e.target_memory_id = $3
                AND EXISTS (
                    SELECT 1
                      FROM proxima_core.entity_owner eo
                      JOIN unnest($1::proxima_core.owner_principal_kind[], $2::uuid[]) AS s(kind, id)
                        ON eo.owner_principal_kind = s.kind
                       AND eo.owner_principal_id = s.id
                     WHERE eo.entity_id = e.source_goal_id
                )
             UNION
             SELECT child.goal_id
               FROM proxima_core.goals child
               JOIN linked_goals prior ON child.supersedes = prior.goal_id
              WHERE EXISTS (
                    SELECT 1
                      FROM proxima_core.entity_owner eo
                      JOIN unnest($1::proxima_core.owner_principal_kind[], $2::uuid[]) AS s(kind, id)
                        ON eo.owner_principal_kind = s.kind
                       AND eo.owner_principal_id = s.id
                     WHERE eo.entity_id = child.goal_id
                )
         )
         SELECT g.goal_id, g.title, ga.memory_id AS goal_activated_memory_id
           FROM proxima_core.goals g
           JOIN linked_goals linked ON linked.goal_id = g.goal_id
           JOIN proxima_core.goal_activated_v1 ga ON ga.goal_id = g.goal_id
          WHERE EXISTS (
                SELECT 1
                  FROM proxima_core.entity_owner eo
                  JOIN unnest($1::proxima_core.owner_principal_kind[], $2::uuid[]) AS s(kind, id)
                    ON eo.owner_principal_kind = s.kind
                   AND eo.owner_principal_id = s.id
                 WHERE eo.entity_id = g.goal_id
            )
            AND g.state = $4
            AND NOT EXISTS (
                SELECT 1
                  FROM proxima_core.goals newer
                 WHERE newer.supersedes = g.goal_id
                   AND EXISTS (
                        SELECT 1
                          FROM proxima_core.entity_owner eo
                          JOIN unnest($1::proxima_core.owner_principal_kind[], $2::uuid[]) AS s(kind, id)
                            ON eo.owner_principal_kind = s.kind
                           AND eo.owner_principal_id = s.id
                         WHERE eo.entity_id = newer.goal_id
                   )
            )
          ORDER BY g.created_at DESC
          LIMIT $5";

    let rows: Vec<ActiveGoalRow> = sqlx::query_as(sql)
        .bind(&read_owner_kinds)
        .bind(&read_owner_ids)
        .bind(self_perspective_memory_id.into_inner())
        .bind(GoalState::Active)
        .bind(i64::try_from(limit).unwrap_or(i64::MAX))
        .fetch_all(pool)
        .await
        .map_err(internal)?;

    Ok(rows
        .into_iter()
        .map(|row| ActiveGoalSummary {
            goal_id: GoalId::new(row.goal_id),
            goal_activated_memory_id: row.goal_activated_memory_id.map(MemoryId::new),
            title: row.title,
        })
        .collect())
}

fn read_owner_columns(read_owners: &[Principal]) -> (Vec<OwnerPrincipalKind>, Vec<uuid::Uuid>) {
    let kinds = read_owners
        .iter()
        .map(|principal| principal.columns().0)
        .collect();
    let ids = read_owners
        .iter()
        .map(|principal| principal.columns().1)
        .collect();
    (kinds, ids)
}

#[derive(sqlx::FromRow)]
struct ActiveGoalRow {
    goal_id: uuid::Uuid,
    title: String,
    goal_activated_memory_id: Option<uuid::Uuid>,
}

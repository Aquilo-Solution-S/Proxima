use proxima_core::personality::ActiveGoalSummary;
use proxima_core::{GoalId, MemoryId, Owner, Principal, StorageError};
use sqlx::PgPool;

pub(crate) async fn list_active_goals(
    pool: &PgPool,
    owner: &Owner,
    self_perspective_memory_id: MemoryId,
    limit: usize,
) -> Result<Vec<ActiveGoalSummary>, StorageError> {
    let (owner_kind, owner_principal_id) = owner_columns(owner);

    // The goal-activated lifecycle Fact lives in the proxima-goal flavor's
    // sidecar schema. Probe for it so the substrate degrades gracefully
    // (returns goals with `goal_activated_memory_id = None`) when the
    // flavor isn't loaded.
    let activated_table_present: bool =
        sqlx::query_scalar("SELECT to_regclass('proxima_goal.goal_activated_v1') IS NOT NULL")
            .fetch_one(pool)
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?;

    let sql = if activated_table_present {
        "WITH RECURSIVE linked_goals(goal_id) AS (
             SELECT e.source_goal_id
               FROM proxima_core.edges e
              WHERE e.owner_principal_kind = $1
                AND e.owner_principal_id = $2
                AND e.relation = 'core/inspires'
                AND e.source_kind = 'Goal'
                AND e.source_goal_id IS NOT NULL
                AND e.target_kind = 'Perspective'
                AND e.target_memory_id = $3
             UNION
             SELECT child.goal_id
               FROM proxima_core.goals child
               JOIN linked_goals prior ON child.supersedes = prior.goal_id
              WHERE child.owner_principal_kind = $1
                AND child.owner_principal_id = $2
         )
         SELECT g.goal_id, g.title, ga.memory_id AS goal_activated_memory_id
           FROM proxima_core.goals g
           JOIN linked_goals linked ON linked.goal_id = g.goal_id
           LEFT JOIN proxima_goal.goal_activated_v1 ga ON ga.goal_id = g.goal_id
          WHERE g.owner_principal_kind = $1
            AND g.owner_principal_id = $2
            AND g.state = 'Active'
            AND NOT EXISTS (
                SELECT 1
                  FROM proxima_core.goals newer
                 WHERE newer.supersedes = g.goal_id
                   AND newer.owner_principal_kind = $1
                   AND newer.owner_principal_id = $2
            )
          ORDER BY g.created_at DESC
          LIMIT $4"
    } else {
        "WITH RECURSIVE linked_goals(goal_id) AS (
             SELECT e.source_goal_id
               FROM proxima_core.edges e
              WHERE e.owner_principal_kind = $1
                AND e.owner_principal_id = $2
                AND e.relation = 'core/inspires'
                AND e.source_kind = 'Goal'
                AND e.source_goal_id IS NOT NULL
                AND e.target_kind = 'Perspective'
                AND e.target_memory_id = $3
             UNION
             SELECT child.goal_id
               FROM proxima_core.goals child
               JOIN linked_goals prior ON child.supersedes = prior.goal_id
              WHERE child.owner_principal_kind = $1
                AND child.owner_principal_id = $2
         )
         SELECT g.goal_id, g.title, NULL::uuid AS goal_activated_memory_id
           FROM proxima_core.goals g
           JOIN linked_goals linked ON linked.goal_id = g.goal_id
          WHERE g.owner_principal_kind = $1
            AND g.owner_principal_id = $2
            AND g.state = 'Active'
            AND NOT EXISTS (
                SELECT 1
                  FROM proxima_core.goals newer
                 WHERE newer.supersedes = g.goal_id
                   AND newer.owner_principal_kind = $1
                   AND newer.owner_principal_id = $2
            )
          ORDER BY g.created_at DESC
          LIMIT $4"
    };

    let rows: Vec<ActiveGoalRow> = sqlx::query_as(sql)
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(self_perspective_memory_id.into_inner())
        .bind(i64::try_from(limit).unwrap_or(i64::MAX))
        .fetch_all(pool)
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?;

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

fn owner_columns(owner: &Owner) -> (&'static str, uuid::Uuid) {
    match &owner.principal {
        Principal::User(user) => ("User", user.into_inner()),
        Principal::Group(group) => ("Group", group.into_inner()),
    }
}

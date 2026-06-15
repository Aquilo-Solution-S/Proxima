use proxima_core::personality::ActiveGoalSummary;
use proxima_core::verbs::goal_write::GoalState;
use proxima_core::{GoalId, MemoryId, OwnerPrincipalKind, Principal, StorageError};
use sqlx::PgPool;

pub(crate) async fn list_active_goals(
    pool: &PgPool,
    principal: &Principal,
    self_perspective_memory_id: MemoryId,
    limit: usize,
) -> Result<Vec<ActiveGoalSummary>, StorageError> {
    let (owner_kind, owner_principal_id) = owner_columns(principal);

    let sql = "WITH RECURSIVE linked_goals(goal_id) AS (
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
           JOIN proxima_core.goal_activated_v1 ga ON ga.goal_id = g.goal_id
          WHERE g.owner_principal_kind = $1
            AND g.owner_principal_id = $2
            AND g.state = $4
            AND NOT EXISTS (
                SELECT 1
                  FROM proxima_core.goals newer
                 WHERE newer.supersedes = g.goal_id
                   AND newer.owner_principal_kind = $1
                   AND newer.owner_principal_id = $2
            )
          ORDER BY g.created_at DESC
          LIMIT $5";

    let rows: Vec<ActiveGoalRow> = sqlx::query_as(sql)
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(self_perspective_memory_id.into_inner())
        .bind(GoalState::Active)
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

fn owner_columns(principal: &Principal) -> (OwnerPrincipalKind, uuid::Uuid) {
    match principal {
        Principal::User(user) => (OwnerPrincipalKind::User, user.into_inner()),
        Principal::Group(group) => (OwnerPrincipalKind::Group, group.into_inner()),
    }
}

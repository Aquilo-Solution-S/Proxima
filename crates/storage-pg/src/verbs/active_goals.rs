use proxima_core::personality::ActiveGoalSummary;
use proxima_core::{GoalId, MemoryId, Owner, Principal, SchemaId, SchemaVersion, StorageError};
use sqlx::PgPool;

pub(crate) async fn list_active_goals(
    pool: &PgPool,
    owner: &Owner,
    self_perspective_memory_id: MemoryId,
    limit: usize,
) -> Result<Vec<ActiveGoalSummary>, StorageError> {
    let (owner_kind, owner_principal_id) = owner_columns(owner);
    let rows: Vec<ActiveGoalRow> = sqlx::query_as(
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
         SELECT g.goal_id, g.schema_id, g.schema_version, g.title, g.text, g.payload
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
          LIMIT $4",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(self_perspective_memory_id.into_inner())
    .bind(i64::try_from(limit).unwrap_or(i64::MAX))
    .fetch_all(pool)
    .await
    .map_err(|e| StorageError::Internal(e.to_string()))?;

    rows.into_iter()
        .map(|row| {
            let schema_version = u32::try_from(row.schema_version).map_err(|_| {
                StorageError::Internal(format!(
                    "invalid goal schema_version {} for {}",
                    row.schema_version, row.goal_id
                ))
            })?;
            Ok(ActiveGoalSummary {
                goal_id: GoalId::new(row.goal_id),
                schema_id: SchemaId::new(row.schema_id),
                schema_version: SchemaVersion::new(schema_version),
                title: row.title,
                text: row.text,
                payload: row.payload,
            })
        })
        .collect()
}

#[derive(sqlx::FromRow)]
struct ActiveGoalRow {
    goal_id: uuid::Uuid,
    schema_id: String,
    schema_version: i32,
    title: String,
    text: String,
    payload: Vec<u8>,
}

fn owner_columns(owner: &Owner) -> (&'static str, uuid::Uuid) {
    match &owner.principal {
        Principal::User(user) => ("User", user.into_inner()),
        Principal::Group(group) => ("Group", group.into_inner()),
    }
}

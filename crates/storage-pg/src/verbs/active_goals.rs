use proxima_core::read_models::ActiveGoalSummary;
use proxima_core::verbs::goal_write::GoalState;
use proxima_core::{GoalId, MemoryId, OwnerRef, StorageError};
use sqlx::PgConnection;

use crate::error::map_err;

pub(crate) async fn list_active_goals_on_connection(
    connection: &mut PgConnection,
    read_owners: &[OwnerRef],
    self_perspective_memory_id: MemoryId,
    limit: usize,
) -> Result<Vec<ActiveGoalSummary>, StorageError> {
    if read_owners.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<uuid::Uuid> = read_owners
        .iter()
        .copied()
        .map(OwnerRef::stored_owner_id)
        .collect();
    let rows: Vec<ActiveGoalRow> = sqlx::query_as("SELECT g.handle AS goal_id, g.title, NULL::uuid AS goal_activated_memory_id FROM proxima_core.goal_head h JOIN proxima_core.goal g ON g.handle = h.handle AND g.t = h.t WHERE g.owner_id = ANY($1::uuid[]) AND g.state = $2 AND g.assignment_t = $3 ORDER BY g.t DESC, g.handle DESC LIMIT $4")
        .bind(&ids).bind(GoalState::Active).bind(self_perspective_memory_id.into_inner()).bind(i64::try_from(limit).unwrap_or(i64::MAX)).fetch_all(&mut *connection).await.map_err(map_err)?;
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

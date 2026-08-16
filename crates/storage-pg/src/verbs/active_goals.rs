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
    let owner_ids: Vec<uuid::Uuid> = read_owners
        .iter()
        .copied()
        .map(proxima_core::OwnerRef::stored_owner_id)
        .collect();
    let _ = (read_owner_kinds, read_owner_ids);
    let rows: Vec<ActiveGoalRow> = sqlx::query_as(
        "SELECT g.handle AS goal_id, g.title, NULL::uuid AS goal_activated_memory_id
           FROM proxima_core.goal_head h
           JOIN proxima_core.goal g
             ON g.handle = h.handle AND g.t = h.t
          WHERE g.owner_id = ANY($1::uuid[])
            AND g.state = $2
            AND g.assignment_t = $3
          ORDER BY g.t DESC, g.handle DESC
          LIMIT $4",
    )
    .bind(&owner_ids)
    .bind(GoalState::Active)
    .bind(self_perspective_memory_id.into_inner())
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

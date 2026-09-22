//! Goal-owned wake candidate reads.

use proxima_core::{GoalId, GoalWakeCandidate, GoalWakeCandidateRequest, MemoryId, StorageError};
use sqlx::PgConnection;

use crate::error::map_err;

type WakeRow = (
    uuid::Uuid,
    Option<uuid::Uuid>,
    Option<String>,
    Vec<String>,
    String,
);

pub(crate) async fn list_goal_wake_candidates_on_connection(
    connection: &mut PgConnection,
    req: &GoalWakeCandidateRequest<'_>,
) -> Result<Vec<GoalWakeCandidate>, StorageError> {
    let ids: Vec<uuid::Uuid> = req
        .actor_read_owners
        .iter()
        .copied()
        .map(proxima_core::OwnerRef::stored_owner_id)
        .collect();
    let rows: Vec<(uuid::Uuid, Vec<String>, String)> = sqlx::query_as("SELECT g.t, w.tool_ids, w.prompt FROM proxima_core.goal_head h JOIN proxima_core.goal g ON g.handle = h.handle AND g.t = h.t JOIN proxima_core.wake_config w ON w.wake_id = g.wake_id WHERE g.owner_id = ANY($1::uuid[]) AND g.state = 'Active' AND g.wake_id IS NOT NULL AND ((w.trigger_kind = 'fact_memory' AND w.trigger_t = $2) OR (w.trigger_kind = 'fact_schema' AND w.trigger_schema_id = $3) ) ORDER BY g.t DESC LIMIT $4").bind(&ids).bind(req.trigger_fact_id.into_inner()).bind(req.trigger_schema_id.as_str()).bind(i64::try_from(req.limit).unwrap_or(i64::MAX)).fetch_all(&mut *connection).await.map_err(map_err)?;
    Ok(rows
        .into_iter()
        .map(|(id, tools, prompt)| GoalWakeCandidate {
            goal_id: GoalId::new(id),
            tool_ids: tools,
            prompt,
            hard_memories: Vec::new(),
            actor_write_owners: req.actor_write_owners.to_vec(),
        })
        .collect())
}

pub(crate) async fn load_goal_wake_configs_on_connection(
    connection: &mut PgConnection,
    read_owners: &[proxima_core::OwnerRef],
    goal_ids: &[GoalId],
) -> Result<Vec<proxima_core::read_models::GoalWakeConfigRow>, StorageError> {
    if goal_ids.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<uuid::Uuid> = goal_ids.iter().map(|id| id.into_inner()).collect();
    let owners: Vec<uuid::Uuid> = read_owners
        .iter()
        .copied()
        .map(proxima_core::OwnerRef::stored_owner_id)
        .collect();
    let rows: Vec<WakeRow> = sqlx::query_as("SELECT g.t, w.trigger_t, w.trigger_schema_id, w.tool_ids, w.prompt FROM proxima_core.goal g JOIN proxima_core.wake_config w ON w.wake_id = g.wake_id WHERE g.t = ANY($1::uuid[]) AND g.owner_id = ANY($2::uuid[])").bind(&ids).bind(&owners).fetch_all(&mut *connection).await.map_err(map_err)?;
    Ok(rows
        .into_iter()
        .map(
            |(id, t, s, tools, prompt)| proxima_core::read_models::GoalWakeConfigRow {
                goal_id: GoalId::new(id),
                trigger_memory_id: t.map(MemoryId::new),
                trigger_schema_id: s.map(proxima_core::SchemaId::new),
                trigger_schema_version: None,
                tool_ids: tools,
                prompt,
                hard_memories: Vec::new(),
            },
        )
        .collect())
}

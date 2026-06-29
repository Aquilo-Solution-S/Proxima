//! `read_mcp_call_history` storage verb.

use proxima_core::StorageError;
use proxima_core::verbs::mcp_call_history::{
    MAX_MCP_CALL_HISTORY_LIMIT, McpCallHistoryRequest, McpCallHistoryResponse, McpCallRecord,
};
use sqlx::PgPool;

use crate::error::map_err;

pub(crate) async fn read_mcp_call_history(
    pool: &PgPool,
    req: &McpCallHistoryRequest,
) -> Result<McpCallHistoryResponse, StorageError> {
    let (owner_kind, owner_id) = req.principal.columns();
    let limit = i64::from(req.limit.min(MAX_MCP_CALL_HISTORY_LIMIT));

    let rows = sqlx::query_as::<
        _,
        (
            time::OffsetDateTime,
            String,
            bool,
            Option<String>,
            Option<Vec<u8>>,
            bool,
        ),
    >(
        "SELECT memories.created_at,
                  fact.tool_name,
                  fact.ok,
                  fact.error,
                  io.body,
                  fact.io_truncated
             FROM proxima_core.mcp_call_logged_v1 fact
             JOIN proxima_core.memories memories USING (memory_id)
             LEFT JOIN proxima_core.citation_mappings cm USING (memory_id)
             LEFT JOIN proxima_core.cited_mcp_call_io_v1 io USING (cited_object_id)
            WHERE EXISTS (
                    SELECT 1
                      FROM (SELECT memory_id AS entity_id, owner_kind, owner_id FROM proxima_core.memories UNION ALL SELECT goal_id AS entity_id, owner_kind, owner_id FROM proxima_core.goals) eo
                     WHERE eo.entity_id = memories.memory_id
                       AND eo.owner_kind = $1
                       AND eo.owner_id = $2
)
              AND ($3::text IS NULL OR fact.actor_oid = $3)
            ORDER BY memories.created_at DESC
            LIMIT $4",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .bind(req.actor_oid.as_deref())
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(map_err)?;

    let calls = rows
        .into_iter()
        .map(
            |(at, tool_name, ok, error, io_body, io_truncated)| McpCallRecord {
                at,
                tool_name,
                ok,
                error,
                io_body,
                io_truncated,
            },
        )
        .collect();

    Ok(McpCallHistoryResponse { calls })
}

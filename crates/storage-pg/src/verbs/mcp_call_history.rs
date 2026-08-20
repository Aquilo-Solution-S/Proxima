//! `read_mcp_call_history` storage verb.

use proxima_core::verbs::mcp_call_history::{
    MAX_MCP_CALL_HISTORY_LIMIT, McpCallHistoryRequest, McpCallHistoryResponse, McpCallRecord,
};
use proxima_core::{MemoryId, StorageError};
use sqlx::PgPool;

use crate::error::map_err;

type HistoryRowDb = (
    time::OffsetDateTime,
    uuid::Uuid,
    String,
    bool,
    Option<String>,
    Option<Vec<u8>>,
    bool,
);

pub(crate) async fn read_mcp_call_history(
    pool: &PgPool,
    req: &McpCallHistoryRequest,
) -> Result<McpCallHistoryResponse, StorageError> {
    let owner_id = req.owner.stored_owner_id();
    let limit = i64::from(req.limit.min(MAX_MCP_CALL_HISTORY_LIMIT));
    let _ = req.include_body;

    let cursor_predicate = if req.before.is_some() {
        "AND (COALESCE(uuid_extract_timestamp(fact.t), TIMESTAMPTZ '1970-01-01'), fact.t) < ($4, $5)"
    } else {
        ""
    };

    // Scoped by the SIDECAR's own `owner_id`, with no join to `memory` at
    // all. The row records who made a tool call; an owner transfer moves
    // the Memory and leaves the row behind, so joining through
    // `memory.owner_id` would make the caller's own history vanish the
    // moment it handed the Memory on — and would show it to whoever
    // received it. `t` is a UUIDv7, so it carries its own timestamp and the
    // Memory row was never needed for the ordering either.
    // SQL-POLICY: fixed-fragment — cursor_predicate is a compile-time literal.
    let sql = format!(
        "SELECT COALESCE(uuid_extract_timestamp(fact.t), TIMESTAMPTZ '1970-01-01'),
                  fact.t,
                  fact.tool_name,
                  fact.ok,
                  fact.error,
                  NULL::bytea AS body,
                  fact.io_truncated
             FROM proxima_core.mcp_call_logged_v1 fact
            WHERE fact.owner_id = $1
              AND ($2::text IS NULL OR fact.actor_oid = $2)
              {cursor_predicate}
            ORDER BY 1 DESC, fact.t DESC
            LIMIT $3",
    );

    // SQL-POLICY: fixed-fragment
    let mut query = sqlx::query_as::<_, HistoryRowDb>(sqlx::AssertSqlSafe(sql))
        .bind(owner_id)
        .bind(req.actor_oid.as_deref())
        .bind(limit);
    if let Some((before_at, before_id)) = req.before {
        query = query.bind(before_at).bind(before_id);
    }
    let rows = query.fetch_all(pool).await.map_err(map_err)?;

    let calls = rows
        .into_iter()
        .map(
            |(at, memory_id, tool_name, ok, error, io_body, io_truncated)| McpCallRecord {
                at,
                memory_id: MemoryId::new(memory_id),
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

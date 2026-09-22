//! `read_mcp_call_history` storage verb.

use proxima_core::verbs::mcp_call_history::{
    MAX_MCP_CALL_HISTORY_LIMIT, McpCallHistoryRequest, McpCallHistoryResponse, McpCallRecord,
};
use proxima_core::{MemoryId, StorageError};
use sqlx::PgConnection;

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

pub(crate) async fn read_mcp_call_history_on_connection(
    connection: &mut PgConnection,
    req: &McpCallHistoryRequest,
) -> Result<McpCallHistoryResponse, StorageError> {
    let owner_id = req.owner.stored_owner_id();
    let limit = i64::from(req.limit.min(MAX_MCP_CALL_HISTORY_LIMIT));
    let predicate = if req.before.is_some() {
        "AND (COALESCE(uuid_extract_timestamp(fact.t), TIMESTAMPTZ '1970-01-01'), fact.t) < ($4, $5)"
    } else {
        ""
    };
    let sql = format!(
        "SELECT COALESCE(uuid_extract_timestamp(fact.t), TIMESTAMPTZ '1970-01-01'), fact.t, fact.tool_name, fact.ok, fact.error, NULL::bytea AS body, fact.io_truncated FROM proxima_core.mcp_call_logged_v1 fact WHERE fact.owner_id = $1 AND ($2::text IS NULL OR fact.actor_oid = $2) {predicate} ORDER BY 1 DESC, fact.t DESC LIMIT $3"
    );
    // SQL-POLICY: fixed-fragment
    let mut q = sqlx::query_as::<_, HistoryRowDb>(sqlx::AssertSqlSafe(sql))
        .bind(owner_id)
        .bind(req.actor_oid.as_deref())
        .bind(limit);
    if let Some((at, id)) = req.before {
        q = q.bind(at).bind(id);
    }
    let calls = q
        .fetch_all(&mut *connection)
        .await
        .map_err(map_err)?
        .into_iter()
        .map(
            |(at, id, tool_name, ok, error, io_body, io_truncated)| McpCallRecord {
                at,
                memory_id: MemoryId::new(id),
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

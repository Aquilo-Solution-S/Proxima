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
    let (owner_kind, owner_id) = req.owner.columns();
    let limit = i64::from(req.limit.min(MAX_MCP_CALL_HISTORY_LIMIT));

    // The inline I/O body + its two citation joins are the dominant payload
    // cost; skip them entirely unless the caller opted in. Both joins are
    // LEFT and only feed `io.body`, so dropping them cannot lose rows — it
    // only removes the body column (and any 1:N citation fan-out).
    let (body_select, body_joins) = if req.include_body {
        (
            "io.body",
            "LEFT JOIN proxima_core.citation_mappings cm USING (memory_id) \
             LEFT JOIN proxima_core.cited_mcp_call_io_v1 io USING (cited_object_id)",
        )
    } else {
        ("NULL::bytea", "")
    };
    // Keyset cursor: strictly older than (created_at, memory_id). ORDER BY
    // carries the same tiebreak so the `<` predicate pages without gaps or
    // repeats. Binds $5/$6 only when a cursor is present.
    let cursor_predicate = if req.before.is_some() {
        "AND (memories.created_at, memories.memory_id) < ($5, $6)"
    } else {
        ""
    };

    // SQL-POLICY: fixed-fragment — every interpolated fragment is a compile
    // time literal selected by a bool; no value ever reaches the SQL text.
    //
    // `tombstoned_at IS NULL` matches every other memory read: MCP-call
    // facts are currently exempt from retention tombstoning
    // (`retention_maintenance.rs`), but this read must not depend on that
    // cross-file invariant staying true.
    let sql = format!(
        "SELECT memories.created_at,
                  memories.memory_id,
                  fact.tool_name,
                  fact.ok,
                  fact.error,
                  {body_select} AS body,
                  fact.io_truncated
             FROM proxima_core.mcp_call_logged_v1 fact
             JOIN proxima_core.memories memories USING (memory_id)
             {body_joins}
            WHERE memories.tombstoned_at IS NULL
              AND memories.owner_kind = $1
              AND memories.owner_id IS NOT DISTINCT FROM $2
              AND ($3::text IS NULL OR fact.actor_oid = $3)
              {cursor_predicate}
            ORDER BY memories.created_at DESC, memories.memory_id DESC
            LIMIT $4",
    );

    // SQL-POLICY: fixed-fragment (cursor_predicate and the include_body join are
    // fixed literal fragments; owner, actor, cursor, and limit are all bound)
    let mut query = sqlx::query_as::<_, HistoryRowDb>(sqlx::AssertSqlSafe(sql))
        .bind(owner_kind)
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

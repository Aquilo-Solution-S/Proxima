//! Call connections between current `proxima-code/code-chunk-v1` heads.
//!
//! `code_chunk_call_v1` keys its caller by revision (`t`, the row's own
//! chunk) and its callee by series handle: ingest names every callee of a
//! file before any of the file's chunks has a `t`. Reading a connection
//! therefore needs the head join on both sides, and `flavors/code` may not
//! name `proxima_core.*`, so the join lives here.
//!
//! **This returns candidates, not results.** Every `t` it emits still goes
//! through the caller's authorized read (Query `HeadsOnly`). Nothing here
//! decides visibility.

use proxima_core::{SchemaId, StorageError};
use sqlx::PgExecutor;
use uuid::Uuid;

use crate::error::map_err;

/// `$1` is the page of chunk heads, `$2` the chunk schema, `$3` the pair cap.
///
/// A page chunk is a caller by its `t` and a callee by its handle, one
/// `UNION` branch each so each side reaches the index through its own
/// btree. A caller row survives only while its chunk is the head of its
/// series: a caller's superseded revisions keep their index rows, and
/// without the head join a callee would list each of them as another caller.
const HEAD_CHUNK_CALL_PAIRS_SQL: &str = "WITH page AS (
        SELECT m.handle
          FROM proxima_core.memory m
          JOIN proxima_core.memory_head h ON h.handle = m.handle AND h.t = m.t
         WHERE m.t = ANY($1::uuid[])
           AND h.schema_id = $2
    ),
    touching AS (
        SELECT e.caller_memory_id, e.callee_memory_id
          FROM proxima_code.code_chunk_call_v1 e
         WHERE e.caller_memory_id = ANY($1::uuid[])
        UNION
        SELECT e.caller_memory_id, e.callee_memory_id
          FROM page
          JOIN proxima_code.code_chunk_call_v1 e ON e.callee_memory_id = page.handle
    ),
    pairs AS (
        SELECT p.caller_memory_id, p.callee_memory_id
          FROM touching p
          JOIN proxima_core.memory caller ON caller.t = p.caller_memory_id
          JOIN proxima_core.memory_head caller_head
            ON caller_head.handle = caller.handle AND caller_head.t = caller.t
         WHERE caller_head.schema_id = $2
         ORDER BY p.caller_memory_id, p.callee_memory_id
         LIMIT $3
    )
    SELECT p.caller_memory_id AS caller_t,
           p.callee_memory_id AS callee_handle,
           callee_head.t AS callee_t
      FROM pairs p
      LEFT JOIN proxima_core.memory_head callee_head
        ON callee_head.handle = p.callee_memory_id
       AND callee_head.schema_id = $2
     ORDER BY p.caller_memory_id, p.callee_memory_id";

#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
#[doc(hidden)]
#[must_use]
pub fn head_chunk_call_pairs_sql_for_tests() -> &'static str {
    HEAD_CHUNK_CALL_PAIRS_SQL
}

/// One caller→callee connection whose caller is a current chunk head.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct HeadChunkCallPair {
    /// The caller chunk revision the index row belongs to.
    pub caller_t: Uuid,
    /// The callee series handle the index row stores; it keys the sites.
    pub callee_handle: Uuid,
    /// The callee series' current head, `None` when it has none.
    pub callee_t: Option<Uuid>,
}

/// Connections into and out of `chunk_ts`, at most `limit` pairs, ordered
/// by `(caller_t, callee_handle)`.
///
/// # Errors
///
/// Returns `StorageError::Internal` on query failure.
pub async fn head_chunk_call_pairs<'e, E: PgExecutor<'e>>(
    pool: E,
    schema_id: &SchemaId,
    chunk_ts: &[Uuid],
    limit: i64,
) -> Result<Vec<HeadChunkCallPair>, StorageError> {
    if chunk_ts.is_empty() || limit <= 0 {
        return Ok(Vec::new());
    }
    sqlx::query_as(HEAD_CHUNK_CALL_PAIRS_SQL)
        .bind(chunk_ts)
        .bind(schema_id.as_str())
        .bind(limit)
        .fetch_all(pool)
        .await
        .map_err(map_err)
}

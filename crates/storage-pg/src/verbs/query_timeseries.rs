//! Query starts at memory_head.
#![allow(clippy::missing_errors_doc, clippy::doc_markdown)]

use proxima_core::StorageError;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::map_err;
use crate::verbs::memory_timeseries::MemoryRow;

pub async fn query_heads(
    pool: &PgPool,
    owner_id: Uuid,
    schema_id: &str,
) -> Result<Vec<MemoryRow>, StorageError> {
    sqlx::query_as(
        "SELECT m.handle, m.t, m.kind::text, m.owner_id, m.source_id, m.ingest_key,
                m.origins, m.refs, m.goal_refs, m.occurred_at
           FROM proxima_core.memory_head h
           JOIN proxima_core.memory m ON m.handle = h.handle AND m.t = h.t
          WHERE h.owner_id = $1 AND h.schema_id = $2
          ORDER BY h.handle",
    )
    .bind(owner_id)
    .bind(schema_id)
    .fetch_all(pool)
    .await
    .map_err(map_err)
}

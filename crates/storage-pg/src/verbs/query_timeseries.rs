//! Query starts at memory_head. ChangeHistory pages announce.seq.
//! Publish-to-World is `transfer_to_world` (in-place series owner UPDATE).
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
                m.origins, m.refs
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

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AnnounceRow {
    pub seq: Uuid,
    pub owner_id: Uuid,
    pub op: String,
    pub entity: String,
    pub handle: Uuid,
    pub t: Uuid,
}

pub async fn change_history(
    pool: &PgPool,
    owner_id: Uuid,
    after: Option<Uuid>,
    limit: i64,
) -> Result<Vec<AnnounceRow>, StorageError> {
    sqlx::query_as(
        "SELECT seq, owner_id, op::text, entity::text, handle, t
           FROM proxima_core.announce
          WHERE owner_id = $1
            AND ($2::uuid IS NULL OR seq > $2)
          ORDER BY seq
          LIMIT $3",
    )
    .bind(owner_id)
    .bind(after)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(map_err)
}

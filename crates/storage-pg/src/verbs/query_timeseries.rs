//! Query starts at memory_head. ChangeHistory pages announce.seq.
//! Publish copies the head onto a new World handle.
#![allow(clippy::missing_errors_doc, clippy::doc_markdown)]

use proxima_core::{OwnerRef, StorageError};
use sqlx::{PgPool, Postgres, Transaction};
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
          WHERE h.owner_id = $1 AND m.schema_id = $2
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

/// Copy the hot head onto a new World handle + new t. Old handle unchanged.
pub async fn publish_head(
    tx: &mut Transaction<'_, Postgres>,
    source_handle: Uuid,
) -> Result<(Uuid, Uuid), StorageError> {
    let row = sqlx::query_as::<_, (Uuid, String, Uuid, String)>(
        "SELECT m.t, m.kind::text, m.owner_id, m.schema_id
           FROM proxima_core.memory_head h
           JOIN proxima_core.memory m ON m.handle = h.handle AND m.t = h.t
          WHERE h.handle = $1",
    )
    .bind(source_handle)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)?
    .ok_or(StorageError::NotFound)?;
    let (_src_t, kind, _owner_id, schema_id) = row;
    let world = OwnerRef::World.stored_owner_id();
    let new_handle = Uuid::now_v7();
    let new_t: Uuid = sqlx::query_scalar("SELECT uuidv7()")
        .fetch_one(tx.as_mut())
        .await
        .map_err(map_err)?;

    sqlx::query(
        "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
         VALUES ($1, $2::proxima_core.memory_kind, $3, $4, $5)",
    )
    .bind(new_handle)
    .bind(&kind)
    .bind(&schema_id)
    .bind(world)
    .bind(new_t)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;

    sqlx::query(
        "INSERT INTO proxima_core.memory (handle, t, kind, owner_id, schema_id, origins, refs)
         SELECT $1, $2, kind, $3, schema_id, origins, refs
           FROM proxima_core.memory
          WHERE handle = $4
            AND t = (SELECT t FROM proxima_core.memory_head WHERE handle = $4)",
    )
    .bind(new_handle)
    .bind(new_t)
    .bind(world)
    .bind(source_handle)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;

    sqlx::query(
        "INSERT INTO proxima_core.announce (owner_id, op, entity, handle, t)
         VALUES ($1, 'append', 'memory', $2, $3)",
    )
    .bind(world)
    .bind(new_handle)
    .bind(new_t)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;

    Ok((new_handle, new_t))
}

//! Current `memory_head` rows for code-flavor series listed by natural key.
//!
//! These functions *list* current heads of a `(repo[, path])`.
//! Compile-time SQL; every value is `$`-bound. Search admits by
//! `Engine::query` `HeadsOnly`, not by filtering an id list here.
//!
//! Ingest callers use the owner-only variants (same series
//! `existing_*_handle` will advance). `open_file` uses the owner∪World
//! variants, then `Engine::query` for real visibility.

use std::collections::HashSet;

use proxima_core::{Owner, SchemaId, StorageError};
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::map_err;

/// One current file-revision head for an owned series.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct FileRevisionHeadRow {
    pub t: Uuid,
    pub file_path: String,
    pub content_sha256: Vec<u8>,
    pub state: String,
}

/// Current file-revision heads of `repo_id` owned by `owner`.
///
/// # Errors
///
/// Returns `StorageError::Internal` on query failure.
pub async fn owned_file_revision_heads(
    pool: &PgPool,
    owner: Owner,
    schema_id: &SchemaId,
    repo_id: Uuid,
) -> Result<Vec<FileRevisionHeadRow>, StorageError> {
    sqlx::query_as(
        "SELECT fr.t, fr.file_path, fr.content_sha256, fr.state::text AS state
           FROM proxima_code.file_revision_v1 fr
           JOIN proxima_core.memory m ON m.t = fr.t
           JOIN proxima_core.memory_head h ON h.handle = m.handle AND h.t = m.t
          WHERE h.owner_id = $1
            AND fr.repo_id = $2
            AND h.schema_id = $3
          ORDER BY fr.file_path ASC",
    )
    .bind(owner.stored_owner_id())
    .bind(repo_id)
    .bind(schema_id.as_str())
    .fetch_all(pool)
    .await
    .map_err(map_err)
}

/// Current file-revision `t`s for one path in owner∪World, owner first.
///
/// # Errors
///
/// Returns `StorageError::Internal` on query failure.
pub async fn readable_file_revision_head_ts(
    pool: &PgPool,
    owner: Owner,
    schema_id: &SchemaId,
    repo_id: Uuid,
    file_path: &str,
) -> Result<Vec<Uuid>, StorageError> {
    let owner_id = owner.stored_owner_id();
    let world_id = proxima_core::OwnerRef::World.stored_owner_id();
    sqlx::query_scalar(
        "SELECT fr.t
           FROM proxima_code.file_revision_v1 fr
           JOIN proxima_core.memory m ON m.t = fr.t
           JOIN proxima_core.memory_head h ON h.handle = m.handle AND h.t = m.t
          WHERE h.owner_id IN ($1, $2)
            AND fr.repo_id = $3
            AND fr.file_path = $4
            AND h.schema_id = $5
          ORDER BY (h.owner_id = $1) DESC, fr.t DESC",
    )
    .bind(owner_id)
    .bind(world_id)
    .bind(repo_id)
    .bind(file_path)
    .bind(schema_id.as_str())
    .fetch_all(pool)
    .await
    .map_err(map_err)
}

/// One current owned chunk series for a file (`(repo, path, index)`).
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct ChunkSeriesHead {
    pub chunk_index: i32,
    pub handle: Uuid,
    pub state: String,
}

/// Current owned chunk series of one file, any `state`.
///
/// One series per `chunk_index` at head. Duplicate indexes are a
/// constraint violation, not a last-write-wins map.
///
/// # Errors
///
/// Returns `StorageError::ConstraintViolation` when two heads share an
/// index. Returns `StorageError::Internal` on query failure.
pub async fn owned_chunk_series_heads(
    pool: &PgPool,
    owner: Owner,
    schema_id: &SchemaId,
    repo_id: Uuid,
    file_path: &str,
) -> Result<Vec<ChunkSeriesHead>, StorageError> {
    let rows = sqlx::query_as(
        "SELECT c.chunk_index, h.handle, c.state::text AS state
           FROM proxima_code.code_chunk_v1 c
           JOIN proxima_core.memory m ON m.t = c.t
           JOIN proxima_core.memory_head h ON h.handle = m.handle AND h.t = m.t
          WHERE h.owner_id = $1
            AND c.repo_id = $2
            AND c.file_path = $3
            AND h.schema_id = $4
          ORDER BY c.chunk_index ASC",
    )
    .bind(owner.stored_owner_id())
    .bind(repo_id)
    .bind(file_path)
    .bind(schema_id.as_str())
    .fetch_all(pool)
    .await
    .map_err(map_err)?;
    unique_chunk_series_heads(rows)
}

pub(crate) fn unique_chunk_series_heads(
    rows: Vec<ChunkSeriesHead>,
) -> Result<Vec<ChunkSeriesHead>, StorageError> {
    let mut seen = HashSet::new();
    for row in &rows {
        if !seen.insert(row.chunk_index) {
            return Err(StorageError::ConstraintViolation(format!(
                "duplicate chunk_index {} at current head",
                row.chunk_index
            )));
        }
    }
    Ok(rows)
}

/// Present chunk indexes at the current head of each owned series for a file.
///
/// # Errors
///
/// Returns `StorageError::ConstraintViolation` when two heads share an
/// index. Returns `StorageError::Internal` on query failure.
pub async fn owned_present_chunk_indexes(
    pool: &PgPool,
    owner: Owner,
    schema_id: &SchemaId,
    repo_id: Uuid,
    file_path: &str,
) -> Result<Vec<i32>, StorageError> {
    let heads = owned_chunk_series_heads(pool, owner, schema_id, repo_id, file_path).await?;
    Ok(heads
        .into_iter()
        .filter(|head| head.state == "Present")
        .map(|head| head.chunk_index)
        .collect())
}

/// Present chunk head `t`s for one file in owner∪World.
///
/// # Errors
///
/// Returns `StorageError::Internal` on query failure.
pub async fn readable_chunk_head_ts_for_file(
    pool: &PgPool,
    owner: Owner,
    schema_id: &SchemaId,
    repo_id: Uuid,
    file_path: &str,
) -> Result<Vec<Uuid>, StorageError> {
    let owner_id = owner.stored_owner_id();
    let world_id = proxima_core::OwnerRef::World.stored_owner_id();
    sqlx::query_scalar(
        "SELECT c.t
           FROM proxima_code.code_chunk_v1 c
           JOIN proxima_core.memory m ON m.t = c.t
           JOIN proxima_core.memory_head h ON h.handle = m.handle AND h.t = m.t
          WHERE h.owner_id IN ($1, $2)
            AND c.repo_id = $3
            AND c.file_path = $4
            AND c.state = 'Present'
            AND h.schema_id = $5
          ORDER BY c.chunk_index ASC",
    )
    .bind(owner_id)
    .bind(world_id)
    .bind(repo_id)
    .bind(file_path)
    .bind(schema_id.as_str())
    .fetch_all(pool)
    .await
    .map_err(map_err)
}

#[cfg(test)]
mod tests {
    use super::{ChunkSeriesHead, unique_chunk_series_heads};
    use proxima_core::StorageError;
    use uuid::Uuid;

    #[test]
    fn duplicate_chunk_index_at_head_is_constraint() {
        let handle = Uuid::now_v7();
        let err = unique_chunk_series_heads(vec![
            ChunkSeriesHead {
                chunk_index: 1,
                handle,
                state: "Present".into(),
            },
            ChunkSeriesHead {
                chunk_index: 1,
                handle: Uuid::now_v7(),
                state: "Tombstone".into(),
            },
        ])
        .expect_err("dup index");
        assert!(
            matches!(err, StorageError::ConstraintViolation(ref message) if message.contains("chunk_index 1")),
            "{err}"
        );
    }

    #[test]
    fn head_joined_series_sql_predicates_head_owner_and_schema() {
        let src = include_str!("code_series_heads.rs");
        assert!(
            src.contains("h.owner_id"),
            "HeadsOnly owner filter must hit memory_head"
        );
        assert!(
            src.contains("h.schema_id"),
            "HeadsOnly schema filter must hit memory_head"
        );
        let owner = format!("{}{}", "m.owner", "_id");
        let schema = format!("{}{}", "m.schema", "_id");
        assert!(
            !src.contains(&owner),
            "code series heads must not predicate memory owner"
        );
        assert!(
            !src.contains(&schema),
            "code series heads must not predicate memory schema"
        );
    }
}

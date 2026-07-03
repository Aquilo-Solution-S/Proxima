use std::time::Duration;

use proxima_core::storage_ports::OwnerWritePermit;
use proxima_core::{Cursor, Owner, StorageError};
use sqlx::PgPool;

use crate::error::map_err;

/// Read one owner-scoped opaque source cursor.
///
/// # Errors
///
/// Returns `StorageError::Internal` for SQL failures.
pub(crate) async fn load_source_cursor(
    pool: &PgPool,
    owner: &Owner,
    source: &str,
) -> Result<Option<Cursor>, StorageError> {
    let (owner_kind, owner_id) = owner.columns();
    let row: Option<Vec<u8>> = sqlx::query_scalar(
        "SELECT cursor
           FROM proxima_core.source_cursors
          WHERE owner_kind = $1
            AND owner_id = $2
            AND source = $3",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .bind(source)
    .fetch_optional(pool)
    .await
    .map_err(map_err)?;
    Ok(row.map(Cursor::from_bytes))
}

/// Upsert one owner-scoped opaque source cursor.
///
/// # Errors
///
/// Returns `StorageError::Internal` or `ConstraintViolation` for SQL failures.
pub(crate) async fn store_source_cursor(
    pool: &PgPool,
    permit: &OwnerWritePermit,
    source: &str,
    cursor: &Cursor,
) -> Result<(), StorageError> {
    let owner = permit.owner();
    let (owner_kind, owner_id) = owner.columns();
    sqlx::query(
        "INSERT INTO proxima_core.source_cursors
            (owner_kind, owner_id, source, cursor)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (owner_kind, owner_id, source)
         DO UPDATE SET
             cursor = EXCLUDED.cursor,
             updated_at = now()",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .bind(source)
    .bind(cursor.as_bytes())
    .execute(pool)
    .await
    .map_err(map_err)?;
    Ok(())
}

/// Return how long ago one owner-scoped source cursor was updated.
///
/// # Errors
///
/// Returns `StorageError::Internal` for SQL failures or duration overflow.
pub(crate) async fn source_cursor_age(
    pool: &PgPool,
    owner: &Owner,
    source: &str,
) -> Result<Option<Duration>, StorageError> {
    let (owner_kind, owner_id) = owner.columns();
    let updated_at: Option<time::OffsetDateTime> = sqlx::query_scalar(
        "SELECT updated_at
           FROM proxima_core.source_cursors
          WHERE owner_kind = $1
            AND owner_id = $2
            AND source = $3",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .bind(source)
    .fetch_optional(pool)
    .await
    .map_err(map_err)?;
    updated_at
        .map(|updated_at| {
            let age = time::OffsetDateTime::now_utc() - updated_at;
            if age.is_negative() {
                return Ok(Duration::ZERO);
            }
            age.try_into().map_err(|_| {
                StorageError::Internal("source cursor age overflowed std::time::Duration".into())
            })
        })
        .transpose()
}

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
    owner: &Owner,
    source: &str,
    cursor: &Cursor,
) -> Result<(), StorageError> {
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

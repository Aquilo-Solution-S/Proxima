use proxima_core::{Owner, StorageError};
use sqlx::PgPool;

use crate::error::map_err;
use crate::verbs::consolidate::owner_columns;

/// Upsert the owner-scoped Fact-retention duration.
///
/// # Errors
///
/// Returns `StorageError::Internal` or `ConstraintViolation` for SQL failures.
pub async fn upsert_fact_retention(
    pool: &PgPool,
    owner: &Owner,
    seconds: i64,
) -> Result<(), StorageError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(owner);
    sqlx::query(
        "INSERT INTO proxima_core.owner_fact_retention
            (owner_principal_kind, owner_principal_id, owner_org_id, retention_seconds)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (owner_principal_kind, owner_principal_id, owner_org_id)
         DO UPDATE SET
             retention_seconds = EXCLUDED.retention_seconds,
             updated_at = now()",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(seconds)
    .execute(pool)
    .await
    .map_err(map_err)?;
    Ok(())
}

/// Read the owner-scoped Fact-retention duration.
///
/// # Errors
///
/// Returns `StorageError::Internal` for SQL failures.
pub async fn get_fact_retention(pool: &PgPool, owner: &Owner) -> Result<Option<i64>, StorageError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(owner);
    sqlx::query_scalar(
        "SELECT retention_seconds
           FROM proxima_core.owner_fact_retention
          WHERE owner_principal_kind = $1
            AND owner_principal_id = $2
            AND owner_org_id = $3",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .fetch_optional(pool)
    .await
    .map_err(map_err)
}

/// Clear the owner-scoped Fact-retention duration.
///
/// # Errors
///
/// Returns `StorageError::Internal` for SQL failures.
pub async fn clear_fact_retention(pool: &PgPool, owner: &Owner) -> Result<bool, StorageError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(owner);
    let result = sqlx::query(
        "DELETE FROM proxima_core.owner_fact_retention
          WHERE owner_principal_kind = $1
            AND owner_principal_id = $2
            AND owner_org_id = $3",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .execute(pool)
    .await
    .map_err(map_err)?;
    Ok(result.rows_affected() > 0)
}

use proxima_core::{Owner, StorageError};
use sqlx::{PgPool, Postgres, Transaction};

use crate::error::map_err;

type Tx<'a> = Transaction<'a, Postgres>;

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
    let (owner_kind, owner_id) = owner.columns();
    sqlx::query(
        "INSERT INTO proxima_core.owner_fact_retention
            (owner_kind, owner_id, retention_seconds)
         VALUES ($1, $2, $3)
         ON CONFLICT (owner_kind, owner_id)
         DO UPDATE SET
             retention_seconds = EXCLUDED.retention_seconds,
             updated_at = now()",
    )
    .bind(owner_kind)
    .bind(owner_id)
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
    let (owner_kind, owner_id) = owner.columns();
    sqlx::query_scalar(
        "SELECT retention_seconds
           FROM proxima_core.owner_fact_retention
          WHERE owner_kind = $1
            AND owner_id = $2",
    )
    .bind(owner_kind)
    .bind(owner_id)
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
    let (owner_kind, owner_id) = owner.columns();
    let result = sqlx::query(
        "DELETE FROM proxima_core.owner_fact_retention
          WHERE owner_kind = $1
            AND owner_id = $2",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .execute(pool)
    .await
    .map_err(map_err)?;
    Ok(result.rows_affected() > 0)
}

pub(crate) async fn lock_legal_hold_tx(tx: &mut Tx<'_>, owner: &Owner) -> Result<(), StorageError> {
    let (owner_kind, owner_id) = owner.columns();
    sqlx::query(
        "SELECT pg_advisory_xact_lock(
             hashtextextended(
                 'proxima-owner-legal-hold:' || $1::text || ':' || coalesce($2::text, ''),
                 0
             )
         )",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(())
}

pub(crate) async fn legal_hold_active_tx(
    tx: &mut Tx<'_>,
    owner: &Owner,
) -> Result<bool, StorageError> {
    let (owner_kind, owner_id) = owner.columns();
    sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1
               FROM proxima_core.owner_legal_holds
              WHERE owner_kind = $1
                AND owner_id IS NOT DISTINCT FROM $2
                AND hold_active
         )",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(map_err)
}

/// Set an owner-scoped legal/security hold.
///
/// # Errors
///
/// Returns `StorageError::Internal` or `ConstraintViolation` for SQL failures.
pub(crate) async fn set_legal_hold(pool: &PgPool, owner: &Owner) -> Result<(), StorageError> {
    let mut tx = pool.begin().await.map_err(map_err)?;
    lock_legal_hold_tx(&mut tx, owner).await?;
    let (owner_kind, owner_id) = owner.columns();
    sqlx::query(
        "INSERT INTO proxima_core.owner_legal_holds
            (owner_kind, owner_id, hold_active)
         VALUES ($1, $2, true)
         ON CONFLICT (owner_kind, owner_id)
         DO UPDATE SET
             hold_active = true,
             updated_at = now()",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .execute(&mut *tx)
    .await
    .map_err(map_err)?;
    tx.commit().await.map_err(map_err)
}

/// Read whether an owner-scoped legal/security hold is active.
///
/// # Errors
///
/// Returns `StorageError::Internal` for SQL failures.
pub(crate) async fn get_legal_hold(pool: &PgPool, owner: &Owner) -> Result<bool, StorageError> {
    let (owner_kind, owner_id) = owner.columns();
    sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1
               FROM proxima_core.owner_legal_holds
              WHERE owner_kind = $1
                AND owner_id IS NOT DISTINCT FROM $2
                AND hold_active
         )",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .fetch_one(pool)
    .await
    .map_err(map_err)
}

/// Clear an owner-scoped legal/security hold.
///
/// # Errors
///
/// Returns `StorageError::Internal` for SQL failures.
pub(crate) async fn clear_legal_hold(pool: &PgPool, owner: &Owner) -> Result<bool, StorageError> {
    let mut tx = pool.begin().await.map_err(map_err)?;
    lock_legal_hold_tx(&mut tx, owner).await?;
    let (owner_kind, owner_id) = owner.columns();
    let result = sqlx::query(
        "DELETE FROM proxima_core.owner_legal_holds
          WHERE owner_kind = $1
            AND owner_id IS NOT DISTINCT FROM $2",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .execute(&mut *tx)
    .await
    .map_err(map_err)?;
    tx.commit().await.map_err(map_err)?;
    Ok(result.rows_affected() > 0)
}

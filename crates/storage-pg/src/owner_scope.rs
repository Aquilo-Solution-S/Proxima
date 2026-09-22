//! Storage is the only writer of the transaction-local owner scope.

use proxima_core::{AccessKind, OwnerRef, OwnerScope, StorageError};
use sqlx::{PgConnection, PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::error::map_err;

/// Begin a transaction guarded by authenticated owner authority.
///
/// The returned transaction must execute every statement of the operation,
/// including sidecar reads. Pool queries do not inherit its scope. Each retry
/// must call this function again; commit and rollback remove the local settings.
///
/// # Errors
///
/// Refuses expired authority and propagates database errors without issuing
/// application queries outside the scoped transaction.
pub async fn begin_owner_transaction(
    pool: &PgPool,
    scope: &OwnerScope,
) -> Result<Transaction<'static, Postgres>, StorageError> {
    let mut transaction = pool.begin().await.map_err(map_err)?;
    bind_owner_scope(&mut transaction, scope).await?;
    Ok(transaction)
}

fn owner_ids(owners: Vec<OwnerRef>) -> Vec<Uuid> {
    owners.into_iter().map(OwnerRef::stored_owner_id).collect()
}

pub(crate) async fn finish_transaction<T>(
    transaction: Transaction<'static, Postgres>,
    result: Result<T, StorageError>,
) -> Result<T, StorageError> {
    match result {
        Ok(value) => {
            transaction.commit().await.map_err(map_err)?;
            Ok(value)
        }
        Err(error) => {
            let _ = transaction.rollback().await;
            Err(error)
        }
    }
}

pub(crate) async fn bind_owner_scope(
    connection: &mut PgConnection,
    scope: &OwnerScope,
) -> Result<(), StorageError> {
    if scope.is_expired() {
        return Err(StorageError::ConstraintViolation(
            "authenticated owner scope has expired".into(),
        ));
    }
    // PostgreSQL serializes the bound UUID arrays. No header, owner text, or
    // caller-supplied role can enter this function in place of the witness.
    sqlx::query(
        "SELECT set_config('app.proxima_scope', 'owner', true),
                set_config('app.owner', $1::uuid[]::text, true),
                set_config('app.read_abstraction', $2::uuid[]::text, true),
                set_config('app.read_perspective', $3::uuid[]::text, true),
                set_config('app.read_goal', $4::uuid[]::text, true),
                set_config('app.write_owner', $5::uuid[]::text, true),
                set_config('app.write_abstraction', $6::uuid[]::text, true),
                set_config('app.write_perspective', $7::uuid[]::text, true),
                set_config('app.write_goal', $8::uuid[]::text, true),
                set_config('app.manage_owner', $9::uuid[]::text, true)",
    )
    .bind(owner_ids(scope.readable_owners(AccessKind::Fact)))
    .bind(owner_ids(scope.readable_owners(AccessKind::Abstraction)))
    .bind(owner_ids(scope.readable_owners(AccessKind::Perspective)))
    .bind(owner_ids(scope.readable_owners(AccessKind::Goal)))
    .bind(owner_ids(scope.writable_owners(AccessKind::Fact)))
    .bind(owner_ids(scope.writable_owners(AccessKind::Abstraction)))
    .bind(owner_ids(scope.writable_owners(AccessKind::Perspective)))
    .bind(owner_ids(scope.writable_owners(AccessKind::Goal)))
    .bind(owner_ids(scope.managed_owners()))
    .execute(connection)
    .await
    .map_err(map_err)?;
    Ok(())
}

/// The compatibility release keeps legacy callers working only while the
/// database has not activated owner RLS. This is not a fallback from failed
/// scope binding: an authenticated witness always takes the scoped path.
///
/// # Errors
///
/// Refuses missing authority after RLS activation and expired authenticated
/// authority, and returns database errors without running the application body.
#[doc(hidden)]
pub async fn begin_compatible_owner_transaction(
    pool: &PgPool,
    scope: Option<&OwnerScope>,
) -> Result<Transaction<'static, Postgres>, StorageError> {
    if let Some(scope) = scope {
        return begin_owner_transaction(pool, scope).await;
    }
    let mut transaction = pool.begin().await.map_err(map_err)?;
    let enforced: bool = sqlx::query_scalar(
        "SELECT EXISTS (
            SELECT 1 FROM pg_catalog.pg_class relation
            JOIN pg_catalog.pg_namespace namespace ON namespace.oid = relation.relnamespace
            WHERE namespace.nspname = 'proxima_core'
              AND relation.relkind IN ('r', 'p')
              AND (relation.relname = 'owner_rls_epoch'
                   OR relation.relrowsecurity OR relation.relforcerowsecurity)
        )",
    )
    .fetch_one(&mut *transaction)
    .await
    .map_err(map_err)?;
    if enforced {
        transaction.rollback().await.map_err(map_err)?;
        return Err(StorageError::ConstraintViolation(
            "owner RLS requires an authenticated OwnerScope".into(),
        ));
    }
    Ok(transaction)
}

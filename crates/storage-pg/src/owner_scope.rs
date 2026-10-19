//! Storage is the only writer of the transaction-local owner scope.

use proxima_core::{AccessKind, OwnerId, OwnerScope, StorageError};
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

fn owner_ids(owners: Vec<OwnerId>) -> Vec<Uuid> {
    owners.into_iter().map(OwnerId::into_inner).collect()
}

pub(crate) fn require_owner_scope(scope: Option<&OwnerScope>) -> Result<&OwnerScope, StorageError> {
    scope.ok_or_else(|| {
        StorageError::ConstraintViolation(
            "authenticated OwnerScope is required for owner-scoped storage".into(),
        )
    })
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
                set_config('app.manage_owner', $9::uuid[]::text, true),
                set_config('app.subject', $10::uuid::text, true)",
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
    .bind(scope.subject().into_inner())
    .execute(connection)
    .await
    .map_err(map_err)?;
    Ok(())
}

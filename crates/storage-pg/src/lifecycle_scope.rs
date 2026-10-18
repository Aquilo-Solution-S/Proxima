//! The lifecycle scope: an owner transaction that also sees forgotten
//! memories.
//!
//! The owner read policy on `proxima_core.memory` hides a row whose
//! `forgotten_at` is set, and every sidecar policy that reaches its parent
//! through `memory` inherits that. The verbs that act on forgotten rows —
//! forget itself, restore, erase, transfer, export and replay — run here
//! instead. This module is the only writer of the GUC the policy reads; the
//! type is crate-private, so no host or flavor can construct it.

use proxima_core::{OwnerScope, StorageError};
use sqlx::{PgConnection, PgPool, Postgres, Transaction};

use crate::error::map_err;

/// Proof that a transaction was opened in the lifecycle scope.
#[derive(Debug)]
pub(crate) struct PgLifecycleScope {
    _sealed: (),
}

impl PgLifecycleScope {
    /// Begin an owner transaction that also sees forgotten memories.
    #[cfg_attr(not(test), expect(dead_code, reason = "restore adopts it (#488 S14)"))]
    ///
    /// # Errors
    ///
    /// Everything [`crate::begin_owner_transaction`] refuses, and database
    /// errors setting the scope.
    pub(crate) async fn begin(
        pool: &PgPool,
        scope: &OwnerScope,
    ) -> Result<(Self, Transaction<'static, Postgres>), StorageError> {
        let mut transaction = crate::owner_scope::begin_owner_transaction(pool, scope).await?;
        let lifecycle = Self::enter(&mut transaction).await?;
        Ok((lifecycle, transaction))
    }

    /// Widen a transaction already bound to owner scope. The setting is
    /// transaction-local: commit and rollback remove it.
    pub(crate) async fn enter(connection: &mut PgConnection) -> Result<Self, StorageError> {
        sqlx::query("SELECT set_config('app.proxima_lifecycle', 'on', true)")
            .execute(connection)
            .await
            .map_err(map_err)?;
        Ok(Self { _sealed: () })
    }

    /// Narrow the transaction back to owner reads that hide forgotten
    /// memories, for a lifecycle read inside an ordinary owner write.
    pub(crate) async fn leave(self, connection: &mut PgConnection) -> Result<(), StorageError> {
        sqlx::query("SELECT set_config('app.proxima_lifecycle', 'off', true)")
            .execute(connection)
            .await
            .map_err(map_err)?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "lifecycle_scope_pg_tests.rs"]
mod tests;

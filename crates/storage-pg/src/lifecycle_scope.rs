//! The lifecycle scope: an owner transaction that also sees forgotten
//! memories.
//!
//! The owner read policy on `proxima_core.memory` hides a row whose
//! `forgotten_at` is set, and every sidecar policy that reaches its parent
//! through `memory` inherits that. The verbs that act on forgotten rows —
//! forget itself, restore, erase, transfer, export and replay — run here
//! instead. This module is the only writer of the GUC the policy reads; the
//! type is crate-private, so no host or flavor can construct it.

use proxima_core::StorageError;
use sqlx::PgConnection;

use crate::error::map_err;

/// Proof that a transaction was opened in the lifecycle scope.
///
/// The scope ends only through [`Self::finish`], which every caller runs on
/// the success and the error path alike: a verb that fails after entering
/// (a lost race, a kind mismatch, an absent id) is not a SQL error, so the
/// transaction stays usable and would otherwise keep seeing forgotten rows.
#[derive(Debug)]
#[must_use = "end the lifecycle scope with `finish` on every path"]
pub(crate) struct PgLifecycleScope {
    _sealed: (),
}

impl PgLifecycleScope {
    /// Begin an owner transaction that also sees forgotten memories. The
    /// verbs enter the scope inside a transaction they already hold; only
    /// the scope's own tests open one this way.
    ///
    /// # Errors
    ///
    /// Everything [`crate::begin_owner_transaction`] refuses, and database
    /// errors setting the scope.
    #[cfg(test)]
    pub(crate) async fn begin(
        pool: &sqlx::PgPool,
        scope: &proxima_core::OwnerScope,
    ) -> Result<(Self, sqlx::Transaction<'static, sqlx::Postgres>), StorageError> {
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
    /// memories, whatever `result` the scoped work produced, and return that
    /// result. The work's own error wins over a failure to narrow: after a
    /// SQL error the transaction is aborted and the reset cannot run, but no
    /// later statement can either.
    pub(crate) async fn finish<T>(
        self,
        connection: &mut PgConnection,
        result: Result<T, StorageError>,
    ) -> Result<T, StorageError> {
        let left = self.leave(connection).await;
        let value = result?;
        left?;
        Ok(value)
    }

    async fn leave(self, connection: &mut PgConnection) -> Result<(), StorageError> {
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

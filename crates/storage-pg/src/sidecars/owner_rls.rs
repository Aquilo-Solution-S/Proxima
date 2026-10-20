use sqlx::{PgConnection, Postgres, Transaction};

use super::PgSidecarRegistryFrozen;
use crate::error::map_err;
use proxima_core::StorageError;

impl PgSidecarRegistryFrozen {
    /// Synchronize sidecars whose owner follows memory in the caller's migration transaction.
    ///
    /// The database helper owns marker replacement, owner RLS and owner indexes;
    /// it touches only the schemas derived from this frozen composition.
    ///
    /// # Errors
    ///
    /// Refuses a transaction outside platform scope and propagates database errors.
    pub async fn sync_owner_follows(
        &self,
        tx: &mut Transaction<'_, Postgres>,
    ) -> Result<(), StorageError> {
        sqlx::query("SELECT proxima_core.sync_owner_follows($1, $2)")
            .bind(self.flavor_schema_names())
            .bind(self.owner_follows_memory_sidecar_tables())
            .execute(&mut **tx)
            .await
            .map_err(map_err)?;
        Ok(())
    }

    /// Check Follows markers and owner indexes without changing the database.
    ///
    /// The connection belongs to the caller's platform-scoped preflight. Markers
    /// in other compositions' schemas are outside this check. Policy structure
    /// remains the responsibility of [`crate::assert_runtime_rls`].
    ///
    /// # Errors
    ///
    /// Returns `Internal` for a marker mismatch or an expected sidecar without a
    /// valid btree index leading with `owner_id`, and propagates database errors.
    pub async fn ensure_owner_follows(&self, conn: &mut PgConnection) -> Result<(), StorageError> {
        let expected = self.owner_follows_memory_sidecar_tables();
        let mut actual: Vec<String> = sqlx::query_scalar(
            "SELECT table_name FROM proxima_core.owner_follows
              WHERE split_part(table_name, '.', 1) = ANY($1)",
        )
        .bind(self.flavor_schema_names())
        .fetch_all(&mut *conn)
        .await
        .map_err(map_err)?;
        actual.sort_unstable();
        if actual != expected {
            return Err(StorageError::Internal(format!(
                "owner_follows markers differ from the frozen sidecars: expected {expected:?}, \
                 found {actual:?}; run migrations"
            )));
        }
        let unindexed: Vec<String> = sqlx::query_scalar(
            "SELECT expected.table_name FROM unnest($1::text[]) AS expected(table_name)
              WHERE NOT EXISTS (
                  SELECT 1 FROM pg_catalog.pg_class AS c
                  JOIN pg_catalog.pg_namespace AS n ON n.oid = c.relnamespace
                  JOIN pg_catalog.pg_attribute AS a ON a.attrelid = c.oid
                  JOIN pg_catalog.pg_index AS i ON i.indrelid = c.oid
                  JOIN pg_catalog.pg_class AS ic ON ic.oid = i.indexrelid
                  JOIN pg_catalog.pg_am AS am ON am.oid = ic.relam
                  WHERE n.nspname = split_part(expected.table_name, '.', 1)
                    AND c.relname = split_part(expected.table_name, '.', 2)
                    AND c.relkind IN ('r', 'p')
                    AND a.attname = 'owner_id' AND NOT a.attisdropped AND a.attnum > 0
                    AND i.indisvalid AND am.amname = 'btree' AND i.indkey[0] = a.attnum
              ) ORDER BY expected.table_name",
        )
        .bind(&expected)
        .fetch_all(&mut *conn)
        .await
        .map_err(map_err)?;
        if !unindexed.is_empty() {
            return Err(StorageError::Internal(format!(
                "owner_follows sidecars lack a valid btree index leading with owner_id: \
                 {unindexed:?}; run migrations"
            )));
        }
        Ok(())
    }
}

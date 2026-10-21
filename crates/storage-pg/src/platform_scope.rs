//! Boot-bound authority for the explicit platform RLS policy.

use proxima_core::StorageError;
use sqlx::{PgPool, Postgres, Transaction};

use crate::error::map_err;

/// A validated platform connection pool. Request handlers cannot promote an
/// owner witness into this capability: its constructor checks database identity
/// and ownership using the supplied platform credentials.
#[derive(Clone)]
pub struct PgPlatformScope {
    pool: PgPool,
    database_oid: i64,
    role_oid: i64,
    schemas: std::sync::Arc<[String]>,
}

impl std::fmt::Debug for PgPlatformScope {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PgPlatformScope")
            .field("database_oid", &self.database_oid)
            .field("role_oid", &self.role_oid)
            .field("schemas", &self.schemas)
            .finish_non_exhaustive()
    }
}

impl PgPlatformScope {
    /// Construct a lazy authority for facade composition tests that never
    /// execute a platform query.
    ///
    /// # Panics
    ///
    /// Panics only if the fixed test URL is rejected by `SQLx`'s URL parser.
    #[cfg(feature = "test-fixtures")]
    #[must_use]
    pub fn for_tests() -> Self {
        Self {
            pool: PgPool::connect_lazy("postgres://invalid/proxima")
                .expect("test platform pool URL is syntactically valid"),
            database_oid: 0,
            role_oid: 0,
            schemas: std::sync::Arc::from([String::from("proxima_core")]),
        }
    }

    /// Validate the platform pool after migrations, before admitting runtime
    /// traffic or starting maintenance. `schemas` comes from host composition.
    ///
    /// # Errors
    ///
    /// Refuses superuser/BYPASSRLS credentials, empty or missing schemas, and
    /// roles that do not own every table and view in the platform scope, and
    /// unsafe views, definers, materialized views or foreign tables.
    pub async fn new(pool: PgPool, schemas: &[&str]) -> Result<Self, StorageError> {
        if schemas.is_empty() || schemas.iter().any(|schema| schema.is_empty()) {
            return Err(refused("platform schema inventory is empty"));
        }
        // One held connection for every check: acquiring per statement
        // grows the pool during validation (see `assert_runtime_rls_on`).
        let mut conn = pool.acquire().await.map_err(map_err)?;
        let (database_oid, role_oid, superuser, bypass): (i64, i64, bool, bool) = sqlx::query_as(
            "SELECT d.oid::bigint, r.oid::bigint, r.rolsuper, r.rolbypassrls
                   FROM pg_catalog.pg_database d CROSS JOIN pg_catalog.pg_roles r
                  WHERE d.datname = current_database() AND r.rolname = current_user",
        )
        .fetch_one(&mut *conn)
        .await
        .map_err(map_err)?;
        if superuser || bypass {
            return Err(refused(
                "platform data role must obey FORCE ROW LEVEL SECURITY",
            ));
        }
        crate::rls_guard::assert_platform_catalog(&mut conn, schemas, true).await?;
        drop(conn);
        Ok(Self {
            pool,
            database_oid,
            role_oid,
            schemas: schemas.iter().map(|schema| (*schema).to_owned()).collect(),
        })
    }

    /// Union composed schemas with stored flavor surfaces for runtime
    /// checks, and the persisted store number, read in one platform
    /// transaction so a boot takes one platform connection for both.
    pub(crate) async fn boot_catalog(&self) -> Result<(Vec<String>, Option<i32>), StorageError> {
        let mut transaction = self.begin().await?;
        let mut schemas = self.schemas.to_vec();
        let registered: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT split_part(table_name, '.', 1)
               FROM proxima_core.flavor_surface",
        )
        .fetch_all(&mut *transaction)
        .await
        .map_err(map_err)?;
        let store_number: Option<i32> = sqlx::query_scalar(
            "SELECT store_number FROM proxima_core.store_identity WHERE singleton",
        )
        .fetch_optional(&mut *transaction)
        .await
        .map_err(map_err)?;
        transaction.commit().await.map_err(map_err)?;
        schemas.extend(registered);
        schemas.sort();
        schemas.dedup();
        Ok((schemas, store_number))
    }

    /// Begin a platform transaction under its explicit policy, including on
    /// tables owned by this role. No implicit RLS bypass is used.
    ///
    /// # Errors
    ///
    /// Refuses a pooled connection whose database or effective role changed
    /// since construction, or whose role acquired superuser/BYPASSRLS.
    pub async fn begin(&self) -> Result<Transaction<'static, Postgres>, StorageError> {
        let mut transaction = self.pool.begin().await.map_err(map_err)?;
        self.bind_transaction(&mut transaction).await?;
        Ok(transaction)
    }

    /// Borrow the host's transaction for protected authority reads and its
    /// declared membership edits, including before authenticating an owner.
    ///
    /// Declare all groups, people and registered-owner targets the operation
    /// needs before acquiring host revision/claim mutation locks. Existing
    /// tree, owner-kind and person-projection fences remain on this actual
    /// transaction through commit or rollback. Reads see earlier own writes.
    /// The caller owns transaction completion and whole-transaction retry.
    ///
    /// # Errors
    ///
    /// Refuses a database/effective role that differs from this boot-validated
    /// capability, fixed-snapshot isolation, unknown registered owners/groups,
    /// and group-registered person targets. A footprint changed while waiting
    /// returns [`StorageError::Retryable`]. Roll back the whole transaction on
    /// failure; no partially fenced session is returned.
    pub async fn authority_on<'session, 'connection>(
        &self,
        transaction: &'session mut Transaction<'connection, Postgres>,
        targets: crate::AuthorityTargets,
    ) -> Result<crate::PgAuthoritySession<'session, 'connection>, StorageError> {
        crate::PgAuthoritySession::new(self, transaction, targets).await
    }

    /// Begin the owner-export snapshot: a `REPEATABLE READ READ ONLY`
    /// platform transaction that holds the export/erase fence in SHARE mode.
    ///
    /// The fence is the transaction's first statement. A REPEATABLE READ
    /// snapshot is taken at the first statement that is not transaction
    /// control, and `LOCK TABLE` is the one lock that takes none, so the
    /// snapshot starts only after any erase holding the fence (ROW EXCLUSIVE)
    /// committed. A lock taken by a `SELECT`, such as an advisory lock
    /// function, would start the snapshot before its wait and read the state
    /// the erase replaced. Everything is transaction-scoped, so a rollback,
    /// a cancellation or a pooler's reuse of the server connection leaves no
    /// lock behind.
    ///
    /// The wait for the fence is bounded by a transaction-local
    /// `lock_timeout` of 5 s, the bound an erase puts on its own wait for the
    /// fence. An export queued behind a longer erase returns
    /// [`StorageError::Retryable`] saying so instead of holding a pooled
    /// connection for the length of the erase. Once the fence is held,
    /// `SET LOCAL lock_timeout TO DEFAULT` gives the rest of the export the
    /// connection's default (the role, database or startup value), so only
    /// the fence wait is bounded. (`SET LOCAL`, like `LOCK TABLE`, takes no
    /// snapshot; reading the prior value would.)
    pub(crate) async fn begin_export_snapshot(
        &self,
    ) -> Result<Transaction<'static, Postgres>, StorageError> {
        let mut transaction = self.pool.begin().await.map_err(map_err)?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *transaction)
            .await
            .map_err(map_err)?;
        sqlx::query("SET LOCAL lock_timeout = '5s'")
            .execute(&mut *transaction)
            .await
            .map_err(map_err)?;
        sqlx::query("LOCK TABLE proxima_core.export_erase_fence IN SHARE MODE")
            .execute(&mut *transaction)
            .await
            .map_err(|error| match &error {
                sqlx::Error::Database(db) if db.code().as_deref() == Some("55P03") => {
                    StorageError::Retryable(
                        "owner export waited 5 s for the export/erase fence: an owner or \
                         source erase is running; retry the export after it commits"
                            .into(),
                    )
                }
                _ => map_err(error),
            })?;
        sqlx::query("SET LOCAL lock_timeout TO DEFAULT")
            .execute(&mut *transaction)
            .await
            .map_err(map_err)?;
        self.bind_transaction(&mut transaction).await?;
        Ok(transaction)
    }

    pub(crate) async fn bind_transaction(
        &self,
        connection: &mut sqlx::PgConnection,
    ) -> Result<(), StorageError> {
        // One statement: the scope settings take effect only for this
        // transaction, and a refusal drops the transaction (the caller rolls
        // the whole transaction back on any error), so checking the identity
        // and setting the scope need not be two round trips.
        let valid: bool = sqlx::query_scalar(
            "SELECT d.oid::bigint = $1 AND r.oid::bigint = $2
                    AND NOT r.rolsuper AND NOT r.rolbypassrls,
                    set_config('app.proxima_scope', 'platform', true),
                    set_config('app.owner', '{}', true),
                    set_config('app.read_abstraction', '{}', true),
                    set_config('app.read_perspective', '{}', true),
                    set_config('app.read_goal', '{}', true),
                    set_config('app.write_owner', '{}', true),
                    set_config('app.write_abstraction', '{}', true),
                    set_config('app.write_perspective', '{}', true),
                    set_config('app.write_goal', '{}', true),
                    set_config('app.manage_owner', '{}', true)
               FROM pg_catalog.pg_database d CROSS JOIN pg_catalog.pg_roles r
              WHERE d.datname = current_database() AND r.rolname = current_user",
        )
        .bind(self.database_oid)
        .bind(self.role_oid)
        .fetch_one(&mut *connection)
        .await
        .map_err(map_err)?;
        if !valid {
            return Err(refused("platform database identity changed after boot"));
        }
        Ok(())
    }
}

fn refused(message: &str) -> StorageError {
    StorageError::ConstraintViolation(message.to_owned())
}

/// Begin a transaction under the boot-bound platform authority.
pub(crate) async fn begin_platform_transaction(
    _runtime_pool: &PgPool,
    platform: &PgPlatformScope,
) -> Result<Transaction<'static, Postgres>, StorageError> {
    platform.begin().await
}

/// Open the transaction enclosing one migration source. `SQLx`'s per-file
/// transactions become savepoints; scope and DDL/data changes commit together.
///
/// # Errors
/// Refuses enforcing databases unless the migration connection has effective
/// ownership of every application table and view and obeys FORCE RLS itself.
pub async fn begin_migration_transaction(
    connection: &mut sqlx::PgConnection,
) -> Result<Transaction<'_, Postgres>, StorageError> {
    use sqlx::Connection;
    let mut tx = connection.begin().await.map_err(map_err)?;
    let (enforcing, safe, ownership_mismatch): (bool, bool, Option<String>) = sqlx::query_as(
        "SELECT EXISTS (
            SELECT 1 FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
             WHERE left(n.nspname,8) = 'proxima_'
               AND (c.relname='owner_rls_epoch'
                    OR (c.relkind IN ('r','p') AND (c.relrowsecurity OR c.relforcerowsecurity)))
         ), NOT r.rolsuper AND NOT r.rolbypassrls, (
            SELECT format('%I.%I', n.nspname, c.relname)
              FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
             WHERE left(n.nspname,8) = 'proxima_'
               AND c.relkind IN ('r','p','v','m','f')
               AND NOT pg_has_role(current_user,c.relowner,'USAGE')
             ORDER BY n.nspname, c.relname LIMIT 1
         ) FROM pg_roles r WHERE r.rolname=current_user",
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(map_err)?;
    if enforcing && !safe {
        return Err(refused(
            "migration role must own the schema and obey FORCE RLS",
        ));
    }
    if enforcing && let Some(relation) = ownership_mismatch {
        return Err(refused(&format!("migration role does not own {relation}")));
    }
    sqlx::query("SELECT set_config('app.proxima_scope', 'platform', true)")
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;
    Ok(tx)
}

pub(crate) async fn begin_owner_lifecycle_transaction(
    pool: &PgPool,
    platform: &PgPlatformScope,
    permit: &proxima_core::storage_ports::OwnerWritePermit,
) -> Result<Transaction<'static, Postgres>, StorageError> {
    if let Some(scope) = permit.owner_scope() {
        if !scope.may_write(permit.owner(), permit.access_kind()) {
            return Err(refused(
                "lifecycle owner is outside authenticated authority",
            ));
        }
    } else {
        return Err(refused(
            "authenticated OwnerScope is required for owner lifecycle maintenance",
        ));
    }
    begin_platform_transaction(pool, platform).await
}

#[cfg(test)]
mod tests {
    /// The 5 s bound covers the fence wait only: the export's reads run under
    /// the connection's own `lock_timeout`, and the snapshot is still
    /// `REPEATABLE READ READ ONLY`.
    #[tokio::test]
    async fn the_export_bound_ends_with_the_fence_wait() {
        let (pg, database) = crate::test_fixtures::fresh_pg("export_lock_timeout_reset").await;
        let platform = pg.platform_scope_for_host();
        let mut plain = platform.begin().await.expect("platform transaction");
        let default: String = sqlx::query_scalar("SHOW lock_timeout")
            .fetch_one(&mut *plain)
            .await
            .expect("default lock_timeout");
        plain.rollback().await.expect("rollback");

        let mut export = platform
            .begin_export_snapshot()
            .await
            .expect("export snapshot");
        let (lock_timeout, isolation, read_only): (String, String, String) = sqlx::query_as(
            "SELECT current_setting('lock_timeout'),
                    current_setting('transaction_isolation'),
                    current_setting('transaction_read_only')",
        )
        .fetch_one(&mut *export)
        .await
        .expect("export settings");
        export.rollback().await.expect("rollback");

        assert_ne!(
            lock_timeout, "5s",
            "the fence bound does not outlive the fence wait"
        );
        assert_eq!(lock_timeout, default);
        assert_eq!(isolation, "repeatable read");
        assert_eq!(read_only, "on");
        drop(pg);
        proxima_pg_testkit::drop_db(&database)
            .await
            .expect("drop task database");
    }
}

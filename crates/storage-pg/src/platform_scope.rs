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
        let (database_oid, role_oid, superuser, bypass): (i64, i64, bool, bool) = sqlx::query_as(
            "SELECT d.oid::bigint, r.oid::bigint, r.rolsuper, r.rolbypassrls
                   FROM pg_catalog.pg_database d CROSS JOIN pg_catalog.pg_roles r
                  WHERE d.datname = current_database() AND r.rolname = current_user",
        )
        .fetch_one(&pool)
        .await
        .map_err(map_err)?;
        if superuser || bypass {
            return Err(refused(
                "platform data role must obey FORCE ROW LEVEL SECURITY",
            ));
        }
        crate::rls_guard::assert_platform_catalog(&pool, schemas, true).await?;
        Ok(Self {
            pool,
            database_oid,
            role_oid,
            schemas: schemas.iter().map(|schema| (*schema).to_owned()).collect(),
        })
    }

    /// The composed schema inventory this authority was validated over. The
    /// serving storage checks its runtime role's RLS over the same list.
    pub(crate) fn schemas(&self) -> Vec<&str> {
        self.schemas.iter().map(String::as_str).collect()
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
    pub(crate) async fn detached_connection(&self) -> Result<sqlx::PgConnection, StorageError> {
        Ok(self.pool.acquire().await.map_err(map_err)?.detach())
    }

    pub(crate) async fn bind_transaction(
        &self,
        connection: &mut sqlx::PgConnection,
    ) -> Result<(), StorageError> {
        let valid: bool = sqlx::query_scalar(
            "SELECT d.oid::bigint = $1 AND r.oid::bigint = $2
                    AND NOT r.rolsuper AND NOT r.rolbypassrls
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
        sqlx::query(
            "SELECT set_config('app.proxima_scope', 'platform', true),
                    set_config('app.owner', '{}', true),
                    set_config('app.read_abstraction', '{}', true),
                    set_config('app.read_perspective', '{}', true),
                    set_config('app.read_goal', '{}', true),
                    set_config('app.write_owner', '{}', true),
                    set_config('app.write_abstraction', '{}', true),
                    set_config('app.write_perspective', '{}', true),
                    set_config('app.write_goal', '{}', true),
                    set_config('app.manage_owner', '{}', true)",
        )
        .execute(&mut *connection)
        .await
        .map_err(map_err)?;
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

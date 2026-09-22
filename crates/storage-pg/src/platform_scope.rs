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
}

impl std::fmt::Debug for PgPlatformScope {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PgPlatformScope")
            .field("database_oid", &self.database_oid)
            .field("role_oid", &self.role_oid)
            .finish_non_exhaustive()
    }
}

impl PgPlatformScope {
    /// Validate the platform pool after migrations, before admitting runtime
    /// traffic or starting maintenance. `schemas` comes from host composition.
    ///
    /// # Errors
    ///
    /// Refuses superuser/BYPASSRLS credentials, empty or missing schemas, and
    /// roles that do not own every relation in the requested platform scope.
    pub async fn new(pool: PgPool, schemas: &[&str]) -> Result<Self, StorageError> {
        if schemas.is_empty() || schemas.iter().any(|schema| schema.is_empty()) {
            return Err(refused("platform schema inventory is empty"));
        }
        let names = schemas
            .iter()
            .map(|name| (*name).to_owned())
            .collect::<Vec<_>>();
        let mut transaction = pool.begin().await.map_err(map_err)?;
        let (database_oid, role_oid, superuser, bypass): (i64, i64, bool, bool) = sqlx::query_as(
            "SELECT d.oid::bigint, r.oid::bigint, r.rolsuper, r.rolbypassrls
                   FROM pg_catalog.pg_database d CROSS JOIN pg_catalog.pg_roles r
                  WHERE d.datname = current_database() AND r.rolname = current_user",
        )
        .fetch_one(&mut *transaction)
        .await
        .map_err(map_err)?;
        if superuser || bypass {
            return Err(refused(
                "platform data role must obey FORCE ROW LEVEL SECURITY",
            ));
        }
        let relations: Vec<(String, bool)> = sqlx::query_as(
            "SELECT n.nspname::text, pg_has_role(current_user, c.relowner, 'USAGE')
               FROM pg_catalog.pg_class c
               JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
              WHERE n.nspname = ANY($1::text[]) AND c.relkind IN ('r', 'p')",
        )
        .bind(&names)
        .fetch_all(&mut *transaction)
        .await
        .map_err(map_err)?;
        if names
            .iter()
            .any(|name| !relations.iter().any(|(schema, _)| schema == name))
            || relations.iter().any(|(_, owns)| !owns)
        {
            return Err(refused(
                "platform role does not own the complete composed schema",
            ));
        }
        if crate::owner_rls_enforced(&pool, schemas).await? {
            crate::rls_guard::assert_platform_rls(&pool, schemas).await?;
        }
        transaction.commit().await.map_err(map_err)?;
        Ok(Self {
            pool,
            database_oid,
            role_oid,
        })
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

/// Compatibility exists only before enforcement; never derives platform authority
/// from a tenant owner or an absent owner witness.
pub(crate) async fn begin_platform_transaction(
    runtime_pool: &PgPool,
    platform: Option<&PgPlatformScope>,
) -> Result<Transaction<'static, Postgres>, StorageError> {
    match platform {
        Some(scope) => scope.begin().await,
        None => crate::begin_compatible_owner_transaction(runtime_pool, None).await,
    }
}

/// Open the transaction enclosing one migration source. `SQLx`'s per-file
/// transactions become savepoints; scope and DDL/data changes commit together.
///
/// # Errors
/// Refuses enforcing databases unless the migration connection has effective
/// ownership of every application table and obeys FORCE RLS itself.
pub async fn begin_migration_transaction(
    connection: &mut sqlx::PgConnection,
) -> Result<Transaction<'_, Postgres>, StorageError> {
    use sqlx::Connection;
    let mut tx = connection.begin().await.map_err(map_err)?;
    let (enforcing, safe): (bool, bool) = sqlx::query_as(
        "SELECT EXISTS (
            SELECT 1 FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
             WHERE left(n.nspname,8) = 'proxima_'
               AND c.relkind IN ('r','p')
               AND (c.relrowsecurity OR c.relforcerowsecurity OR c.relname='owner_rls_epoch')
         ), NOT r.rolsuper AND NOT r.rolbypassrls AND NOT EXISTS (
            SELECT 1 FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
             WHERE left(n.nspname,8) = 'proxima_'
               AND c.relkind IN ('r','p')
               AND NOT pg_has_role(current_user,c.relowner,'USAGE')
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
    sqlx::query("SELECT set_config('app.proxima_scope', 'platform', true)")
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;
    Ok(tx)
}

pub(crate) async fn begin_owner_lifecycle_transaction(
    pool: &PgPool,
    platform: Option<&PgPlatformScope>,
    permit: &proxima_core::storage_ports::OwnerWritePermit,
) -> Result<Transaction<'static, Postgres>, StorageError> {
    if let Some(scope) = permit.owner_scope() {
        if !scope.may_write(permit.owner(), permit.access_kind()) {
            return Err(refused(
                "lifecycle owner is outside authenticated authority",
            ));
        }
    } else {
        crate::begin_compatible_owner_transaction(pool, None)
            .await?
            .rollback()
            .await
            .map_err(map_err)?;
    }
    begin_platform_transaction(pool, platform).await
}

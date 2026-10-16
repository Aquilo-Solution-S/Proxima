use proxima_core::StorageError;
use proxima_storage_pg::{PgPlatformScope, PgPoolConfig, PgStorage, PgTuning, core_migrator};
use sqlx::migrate::Migrator;
use std::borrow::Cow;

use proxima_pg_testkit::{
    DbGuard, FNV_OFFSET_BASIS, create_db_from_template, create_named_db_from_template, db_url,
    drop_db, ensure_template, fnv1a64_extend, split_role_urls,
};

#[must_use]
pub fn core_template_name() -> String {
    let mut hash = FNV_OFFSET_BASIS;
    for migration in core_migrator_before_owner_rls().iter() {
        hash = fnv1a64_extend(hash, &migration.version.to_be_bytes());
        hash = fnv1a64_extend(hash, migration.checksum.as_ref());
    }
    format!("proxima_tmpl_core_{hash:016x}")
}

/// Owner-RLS activation begins at migration 0014. Historical fixtures stop
/// immediately before it; current boot fixtures then apply the full embedded
/// migrator in production order.
pub const OWNER_RLS_MIGRATION_VERSIONS: [i64; 6] = [14, 17, 19, 22, 23, 24];

/// Core migration lane used by historical domain fixtures.
///
/// The exact migration prefix before owner-RLS activation. No production path
/// calls this helper.
#[must_use]
pub fn core_migrator_before_owner_rls() -> Migrator {
    let mut migrator = core_migrator();
    migrator.migrations = Cow::Owned(
        migrator
            .iter()
            .filter(|migration| migration.version < OWNER_RLS_MIGRATION_VERSIONS[0])
            .cloned()
            .collect(),
    );
    migrator
}

/// Build the [`core_template_name`] template once per migration set:
/// [`core_migrator_before_owner_rls`] run on an empty database.
///
/// # Errors
///
/// Returns admin connection, template build, or migration errors.
pub async fn ensure_core_template() -> Result<String, sqlx::Error> {
    let template = core_template_name();
    ensure_template(&template, |pool| async move {
        core_migrator_before_owner_rls()
            .run(&pool)
            .await
            .map_err(sqlx::Error::from)
    })
    .await?;
    Ok(template)
}

/// Create `name` already migrated through [`core_migrator_before_owner_rls`],
/// cloned from the core template: the drop-in for `create_db` followed by
/// Drop it with `drop_db` like any other test database.
///
/// # Errors
///
/// Returns admin connection, template build, or clone errors.
pub async fn create_core_db(name: &str) -> Result<(), sqlx::Error> {
    let template = ensure_core_template().await?;
    create_named_db_from_template(name, &template).await
}

/// Clone a fresh test database from the core migrated template.
///
/// The returned [`DbGuard`] drops the clone when the test passes and keeps
/// it (printing a `psql` URL) when the test panics.
///
/// # Panics
///
/// Panics when the local test Postgres admin connection, template
/// creation, clone creation, or cloned database connection fails.
pub async fn fresh_pg(prefix: &str) -> (PgStorage, DbGuard) {
    let template = ensure_core_template()
        .await
        .unwrap_or_else(|e| panic!("PG required for tests but admin connect failed: {e}"));

    let db_name = create_db_from_template(prefix, &template)
        .await
        .unwrap_or_else(|e| panic!("PG required for tests but admin connect failed: {e}"));
    let (runtime_url, platform_url) = split_role_urls(&db_name)
        .await
        .unwrap_or_else(|e| panic!("PG required for tests but role provisioning failed: {e}"));
    let migration = proxima_storage_pg::PgMigrationStorage::connect_with_config(
        &platform_url,
        PgPoolConfig::default(),
        PgTuning::default(),
    )
    .await
    .unwrap_or_else(|e| panic!("PG required for tests but migration connect failed: {e}"));
    migration
        .run_migrations()
        .await
        .unwrap_or_else(|e| panic!("PG required for tests but migration failed: {e}"));
    let platform_pool = sqlx::PgPool::connect(&platform_url)
        .await
        .unwrap_or_else(|e| panic!("PG required for tests but platform connect failed: {e}"));
    let platform_scope = PgPlatformScope::new(platform_pool, &["proxima_core"])
        .await
        .unwrap_or_else(|e| panic!("PG required for tests but platform validation failed: {e}"));
    let storage = match PgStorage::connect_with_config(
        &runtime_url,
        PgPoolConfig::default(),
        PgTuning::default(),
        platform_scope,
    )
    .await
    {
        Ok(pg) => pg,
        Err(err) => {
            let _ = drop_db(&db_name).await;
            panic!("PG required for tests but unavailable: {err}");
        }
    };
    let admin_pool = sqlx::PgPool::connect(&db_url(&db_name))
        .await
        .unwrap_or_else(|e| panic!("PG required for tests but admin pool failed: {e}"));
    (
        storage.with_admin_pool_for_tests(admin_pool),
        DbGuard::adopt(db_name),
    )
}

/// Connect a disposable database through the real split-role boot sequence.
///
/// Test callers pass the administrator URL used to create the database. The
/// database name is taken from its final path component; role provisioning,
/// migrations, platform validation, and runtime connection then happen in
/// the same order as production boot.
///
/// # Errors
///
/// Refuses an invalid database URL or failed role provisioning, migration,
/// platform validation, or database connection.
#[cfg(feature = "test-fixtures")]
pub async fn connect_for_tests(url: String) -> Result<PgStorage, StorageError> {
    let database = url
        .rsplit('/')
        .next()
        .and_then(|path| path.split('?').next())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| {
            StorageError::Unavailable("test database URL has no database name".into())
        })?;
    let (runtime_url, platform_url) = split_role_urls(database)
        .await
        .map_err(|error| StorageError::Unavailable(error.to_string()))?;
    let migration = proxima_storage_pg::PgMigrationStorage::connect_with_config(
        &platform_url,
        PgPoolConfig::default(),
        PgTuning::default(),
    )
    .await?;
    migration.run_migrations().await?;
    let platform_pool = sqlx::PgPool::connect(&platform_url)
        .await
        .map_err(|error| StorageError::Unavailable(error.to_string()))?;
    let platform_scope = PgPlatformScope::new(platform_pool, &["proxima_core"]).await?;
    let storage = PgStorage::connect_with_config(
        &runtime_url,
        PgPoolConfig::default(),
        PgTuning::default(),
        platform_scope,
    )
    .await?;
    let admin_pool = sqlx::PgPool::connect(&url)
        .await
        .map_err(|error| StorageError::Unavailable(error.to_string()))?;
    Ok(storage.with_admin_pool_for_tests(admin_pool))
}

impl PgStorage {
    /// Test-only serving connection using the real split-role boot sequence.
    ///
    /// # Errors
    ///
    /// Returns the provisioning, migration, validation, or connection error
    /// from [`connect_for_tests`].
    #[cfg(feature = "test-fixtures")]
    pub async fn connect_for_tests(url: String) -> Result<Self, StorageError> {
        connect_for_tests(url).await
    }
}

/// Register `group` as a group in `proxima_core.owners`, the way a host
/// creates one before writing under it. Idempotent; an id already
/// registered as a user is a `Conflict`.
///
/// # Errors
///
/// Storage errors from the registration.
pub async fn register_group_for_tests(
    pool: &sqlx::PgPool,
    group: proxima_core::GroupId,
) -> Result<(), proxima_core::StorageError> {
    let mut conn = pool
        .acquire()
        .await
        .map_err(|err| proxima_core::StorageError::Unavailable(err.to_string()))?;
    crate::access::owner_columns::register_group(&mut conn, group).await
}

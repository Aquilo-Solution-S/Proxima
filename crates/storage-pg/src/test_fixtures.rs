use proxima_core::StorageError;
use proxima_storage_pg::{PgStorage, core_migrator};
use sqlx::migrate::Migrator;
use std::borrow::Cow;

use proxima_pg_testkit::{
    DbGuard, FNV_OFFSET_BASIS, create_db_from_template, create_named_db_from_template, db_url,
    drop_db, ensure_template, fnv1a64_extend,
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

/// Owner-RLS activation (0014), narrowed policies (0017), hardened definer
/// search paths (0019), the coarse pin-lock mode (0022), kind-scoped policies
/// (0023) and the per-table installer (0024). The enforced fixtures stage
/// exactly these, in order. 0019, 0022 and 0023 rewrite 0014's routines and
/// policies; applied before it, 0014 would undo them. 0024 redefines the
/// installer 0023 redefined; applied before it, 0023 would undo that.
pub const OWNER_RLS_MIGRATION_VERSIONS: [i64; 6] = [14, 17, 19, 22, 23, 24];

/// Core migration lane used by historical domain fixtures.
///
/// Every core migration except [`OWNER_RLS_MIGRATION_VERSIONS`], which are
/// tested by the enforced boot fixtures. Keeping them out of this lane
/// preserves the pre-activation schema for invariant tests; later migrations
/// still apply. No production path calls this helper.
#[must_use]
pub fn core_migrator_before_owner_rls() -> Migrator {
    let mut migrator = core_migrator();
    migrator.migrations = Cow::Owned(
        migrator
            .iter()
            .filter(|migration| !OWNER_RLS_MIGRATION_VERSIONS.contains(&migration.version))
            .cloned()
            .collect(),
    );
    migrator
}

impl PgStorage {
    /// Run the historical pre-owner-RLS core lane for domain fixtures.
    ///
    /// # Errors
    /// Returns migration or database errors.
    pub async fn run_before_owner_rls_migrations(&self) -> Result<(), StorageError> {
        core_migrator_before_owner_rls()
            .run(self.pool_for_tests())
            .await
            .map_err(|error| StorageError::Internal(error.to_string()))
    }
}

/// Prepare a database built by [`core_migrator_before_owner_rls`] for a run of
/// the full core migrator on top of it, which then applies the owner-RLS lane.
///
/// The lane's base also ran every migration after the lane's last member, so
/// tables such as 0025's `goal_change_v1` and 0026's `publication_consumer`
/// and `publication_delivery` exist before 0014's whole-schema
/// census sees the schema, and 0014 refuses a table it has never classified
/// (production applies 0014 first). This drops those tables and forgets the
/// ledger rows after the lane, so the next run applies them after it; those
/// migrations are idempotent and install a new table's owner-RLS themselves.
/// A database whose lane has already run is left alone.
///
/// # Errors
///
/// Returns the database error of the statement.
pub async fn reopen_migrations_after_owner_rls_lane(
    pool: &sqlx::PgPool,
) -> Result<(), sqlx::Error> {
    let lane_end = OWNER_RLS_MIGRATION_VERSIONS
        .iter()
        .max()
        .copied()
        .unwrap_or_default();
    // SQL-POLICY: fixed-fragment — a version number from the const above.
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "DO $reopen$
         BEGIN
             IF NOT EXISTS (SELECT 1 FROM public._sqlx_migrations WHERE version = {first}) THEN
                 DROP TABLE IF EXISTS proxima_core.goal_change_v1;
                 DROP TABLE IF EXISTS proxima_core.publication_delivery;
                 DROP TABLE IF EXISTS proxima_core.publication_consumer;
                 DELETE FROM public._sqlx_migrations WHERE version > {lane_end};
             END IF;
         END
         $reopen$",
        first = OWNER_RLS_MIGRATION_VERSIONS[0],
    )))
    .execute(pool)
    .await?;
    Ok(())
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
/// [`PgStorage::run_before_owner_rls_migrations`], which then finds every
/// migration applied. Drop it with `drop_db` like any other test database.
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
    let url = db_url(&db_name);
    match PgStorage::connect(&url).await {
        Ok(pg) => (pg, DbGuard::adopt(db_name)),
        Err(err) => {
            let _ = drop_db(&db_name).await;
            panic!("PG required for tests but unavailable: {err}");
        }
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

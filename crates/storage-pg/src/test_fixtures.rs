#[path = "test_fixtures/operator_proofs.rs"]
pub mod operator_proofs;

use proxima_core::StorageError;
use proxima_storage_pg::{PgStorage, core_migrator};
use sqlx::migrate::Migrator;
use std::borrow::Cow;

use proxima_pg_testkit::{
    DbGuard, FNV_OFFSET_BASIS, create_db_from_template, db_url, drop_db, ensure_template,
    fnv1a64_extend,
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

/// `0014_v015_owner_rls.sql`.
const OWNER_RLS_MIGRATION_VERSION: i64 = 14;

/// Core migration lane used by historical domain fixtures.
///
/// Every core migration except the owner-RLS activation (0014), which is
/// tested by the enforced boot fixtures. Keeping it out of this lane preserves
/// the pre-activation schema for invariant tests; later migrations still
/// apply. No production path calls this helper.
#[must_use]
pub fn core_migrator_before_owner_rls() -> Migrator {
    let mut migrator = core_migrator();
    migrator.migrations = Cow::Owned(
        migrator
            .iter()
            .filter(|migration| migration.version != OWNER_RLS_MIGRATION_VERSION)
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
    let template = core_template_name();
    ensure_template(&template, |pool| async move {
        core_migrator_before_owner_rls()
            .run(&pool)
            .await
            .map_err(sqlx::Error::from)
    })
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

//! Split-role test databases whose core lane is already migrated.
//!
//! `create_db` + `split_role_urls` + boot runs the whole core lane in every
//! test. [`create_split_core_db`] clones a template that holds exactly what
//! that produces — split roles provisioned, then the core lane applied as the
//! platform role through boot's own [`run_core_and_flavor_migrations`] — so
//! boot finds every core migration applied and runs only the flavor lanes the
//! test composes. Tests that exercise a fresh migration keep `create_db`.

use proxima::host::run_core_and_flavor_migrations;
use proxima_pg_testkit::{
    FNV_OFFSET_BASIS, create_named_db_from_template, ensure_template, fnv1a64_extend,
    split_role_urls,
};
use proxima_storage_pg::{PgPoolConfig, PgStorage, PgTuning, core_migrator};

fn split_core_template_name() -> String {
    let mut hash = FNV_OFFSET_BASIS;
    for migration in core_migrator().iter() {
        hash = fnv1a64_extend(hash, &migration.version.to_be_bytes());
        hash = fnv1a64_extend(hash, migration.checksum.as_ref());
    }
    format!("proxima_tmpl_split_{hash:016x}")
}

/// The drop-in for `create_db(name)` before `split_role_urls(name)` and a
/// boot: `name` is created with the split roles provisioned and the core
/// lane migrated. Drop it with `drop_db` like any other test database.
///
/// # Errors
///
/// Returns admin connection, template build, or clone errors.
pub async fn create_split_core_db(name: &str) -> Result<(), sqlx::Error> {
    let template = split_core_template_name();
    ensure_template(&template, |pool| async move {
        let staging: String = sqlx::query_scalar("SELECT current_database()")
            .fetch_one(&pool)
            .await?;
        pool.close().await;
        let (_, platform_url) = split_role_urls(&staging).await?;
        let pg = PgStorage::connect_for_migrations_with_config(
            &platform_url,
            PgPoolConfig::default(),
            PgTuning::default(),
        )
        .await
        .map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
        let migrated = run_core_and_flavor_migrations(&pg, [])
            .await
            .map_err(|error| sqlx::Error::Protocol(error.to_string()));
        pg.pool_for_tests().close().await;
        migrated.map(|_| ())
    })
    .await?;
    create_named_db_from_template(name, &template).await
}

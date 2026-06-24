use proxima_storage_pg::{PgStorage, core_migrator};

use proxima_pg_testkit::{
    FNV_OFFSET_BASIS, create_db_from_template, db_url, drop_db, ensure_template, fnv1a64_extend,
};

#[must_use]
pub fn core_template_name() -> String {
    let mut hash = FNV_OFFSET_BASIS;
    for migration in core_migrator().iter() {
        hash = fnv1a64_extend(hash, &migration.version.to_be_bytes());
        hash = fnv1a64_extend(hash, migration.checksum.as_ref());
    }
    format!("proxima_tmpl_core_{hash:016x}")
}

/// Clone a fresh test database from the core migrated template.
///
/// # Panics
///
/// Panics when the local test Postgres admin connection, template
/// creation, clone creation, or cloned database connection fails.
pub async fn fresh_pg(prefix: &str) -> (PgStorage, String) {
    let template = core_template_name();
    ensure_template(&template, |pool| async move {
        core_migrator().run(&pool).await.map_err(sqlx::Error::from)
    })
    .await
    .unwrap_or_else(|e| panic!("PG required for tests but admin connect failed: {e}"));

    let db_name = create_db_from_template(prefix, &template)
        .await
        .unwrap_or_else(|e| panic!("PG required for tests but admin connect failed: {e}"));
    let url = db_url(&db_name);
    match PgStorage::connect(&url).await {
        Ok(pg) => (pg, db_name),
        Err(err) => {
            let _ = drop_db(&db_name).await;
            panic!("PG required for tests but unavailable: {err}");
        }
    }
}

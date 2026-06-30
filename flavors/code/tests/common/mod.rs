#![allow(dead_code, clippy::missing_panics_doc)]

use std::path::Path;
use std::process::Command;

use proxima_core::{Owner, OwnerRef, UserId};
use proxima_pg_testkit::{
    FNV_OFFSET_BASIS, create_db_from_template, db_url, drop_db, ensure_template, fnv1a64_extend,
};
use proxima_storage_pg::{
    PgSidecarRegistry, PgSidecarRegistryFrozen, PgStorage, core_migrator, register_core_pg_sidecars,
};
use uuid::Uuid;

pub async fn migrated_db() -> (String, PgStorage) {
    let template = code_template_name();
    ensure_template(&template, |pool| async move {
        core_migrator()
            .run(&pool)
            .await
            .map_err(sqlx::Error::from)?;
        proxima_code::migrator()
            .run(&pool)
            .await
            .map_err(sqlx::Error::from)
    })
    .await
    .unwrap_or_else(|e| panic!("PG required for tests but admin connect failed: {e}"));

    let db_name = create_db_from_template("proxima_code_test", &template)
        .await
        .unwrap_or_else(|e| panic!("PG required for tests but admin connect failed: {e}"));
    let pg = match PgStorage::connect(&db_url(&db_name)).await {
        Ok(pg) => pg,
        Err(err) => {
            let _ = drop_db(&db_name).await;
            panic!("PG required for tests but unavailable: {err}");
        }
    }
    .with_sidecars(code_pg_sidecars());
    (db_name, pg)
}

#[must_use]
pub fn test_owner() -> Owner {
    OwnerRef::Personal(UserId::new(Uuid::now_v7()))
}

pub fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .expect("git command");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

pub async fn insert_home(
    pool: &sqlx::PgPool,
    entity_id: Uuid,
    owner: &Owner,
) -> Result<(), sqlx::Error> {
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(owner);
    sqlx::query(
        "UPDATE proxima_core.memories
            SET owner_kind = $2, owner_id = $3
          WHERE memory_id = $1",
    )
    .bind(entity_id)
    .bind(owner_kind)
    .bind(owner_id)
    .execute(pool)
    .await
    .map(|_| ())
}

pub fn write_file(repo: &Path, path: &str, contents: &str) {
    let full = repo.join(path);
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent).expect("create parent");
    }
    std::fs::write(full, contents).expect("write file");
}

#[derive(Debug)]
pub struct TestDb {
    pub name: String,
    pub pg: PgStorage,
}

impl TestDb {
    pub async fn fresh() -> Self {
        let (name, pg) = migrated_db().await;
        Self { name, pg }
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        let name = self.name.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Runtime::new().expect("drop runtime");
            runtime.block_on(async {
                let _ = drop_db(&name).await;
            });
        })
        .join()
        .expect("drop db thread");
    }
}

fn code_template_name() -> String {
    let mut hash = FNV_OFFSET_BASIS;
    for migration in core_migrator().iter() {
        hash = fnv1a64_extend(hash, &migration.version.to_be_bytes());
        hash = fnv1a64_extend(hash, migration.checksum.as_ref());
    }
    for migration in proxima_code::migrator().iter() {
        hash = fnv1a64_extend(hash, &migration.version.to_be_bytes());
        hash = fnv1a64_extend(hash, migration.checksum.as_ref());
    }
    format!("proxima_tmpl_code_{hash:016x}")
}

fn code_pg_sidecars() -> PgSidecarRegistryFrozen {
    let registry = proxima_code::schema_registry();
    let mut sidecars = PgSidecarRegistry::new();
    register_core_pg_sidecars(&mut sidecars);
    proxima_code::register_pg_sidecars(&mut sidecars);
    sidecars
        .freeze_against(registry.schemas())
        .expect("code test PG sidecars match code schema registry")
}

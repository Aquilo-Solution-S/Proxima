#![allow(dead_code, clippy::missing_panics_doc)]

use std::path::Path;
use std::process::Command;

use proxima_core::storage_ports::OwnerWritePermit;
use proxima_core::verbs::schema::PayloadKind;
use proxima_core::{
    AccessKind, AuthPath, AuthzContext, Engine, FlavorRegistry, FlavorRegistryFrozen, Owner,
    OwnerRef, Role, SchemaId, SchemaVersion, UserId,
};
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

pub const TEST_CITED_BLOB_SCHEMA_ID: &str = "test/cited_blob";
pub const TEST_CITATION_BLOB_SCHEMA_ID: &str = "test/citation_blob";

pub fn code_registry_with_test_citations() -> FlavorRegistryFrozen {
    let mut registry = FlavorRegistry::new();
    proxima_code::register(&mut registry).expect("code schema registration");
    registry
        .try_add_opaque_schema(
            SchemaId::new(TEST_CITED_BLOB_SCHEMA_ID.into()),
            SchemaVersion::new(1),
            PayloadKind::CitedObject,
        )
        .expect("opaque cited-object registration");
    registry
        .try_add_opaque_schema(
            SchemaId::new(TEST_CITATION_BLOB_SCHEMA_ID.into()),
            SchemaVersion::new(1),
            PayloadKind::CitationMapping,
        )
        .expect("opaque citation-mapping registration");
    registry.freeze_or_panic_for_tests()
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

pub async fn owner_write_permit(
    owner: &Owner,
    kind: AccessKind,
) -> Result<OwnerWritePermit, Box<dyn std::error::Error>> {
    let authz = match owner {
        OwnerRef::Personal(user_id) => AuthzContext::for_subject(*user_id, AuthPath::HostBearer),
        OwnerRef::Group(_) => AuthzContext::for_subject_with_role(
            UserId::new(Uuid::now_v7()),
            [(*owner, Role::admin())],
            AuthPath::HostBearer,
        ),
        OwnerRef::World => AuthzContext::denied_for_owner(owner),
    };
    let engine = Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests());
    Ok(engine.authorize_owner_write(&authz, owner, kind).await?)
}

pub async fn insert_home(
    pool: &sqlx::PgPool,
    entity_id: Uuid,
    owner: &Owner,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE proxima_core.memory SET owner_id = $2 WHERE t = $1")
        .bind(entity_id)
        .bind(owner.stored_owner_id())
        .execute(pool)
        .await
        .map(|_| ())
}

/// Insert one timeseries Memory row. Returns `(handle, t)`.
pub async fn seed_memory(
    pool: &sqlx::PgPool,
    owner: &Owner,
    schema_id: &str,
    kind: &str,
    t: Option<Uuid>,
    handle: Option<Uuid>,
    origins: &[Uuid],
) -> Result<(Uuid, Uuid), sqlx::Error> {
    let handle = handle.unwrap_or_else(Uuid::now_v7);
    let t = t.unwrap_or_else(Uuid::now_v7);
    let owner_id = owner.stored_owner_id();
    sqlx::query(
        "INSERT INTO proxima_core.owners (owner_id, kind)
         VALUES ($1, $2::proxima_core.owner_kind) ON CONFLICT DO NOTHING",
    )
    .bind(owner_id)
    .bind(proxima_core::OwnerRefKind::of(owner).as_str())
    .execute(pool)
    .await?;
    let head_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM proxima_core.memory_head WHERE handle = $1)",
    )
    .bind(handle)
    .fetch_one(pool)
    .await?;
    if head_exists {
        sqlx::query("UPDATE proxima_core.memory_head SET t = $2 WHERE handle = $1")
            .bind(handle)
            .bind(t)
            .execute(pool)
            .await?;
    } else {
        sqlx::query(
            "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
             VALUES ($1, $2::proxima_core.memory_kind, $3, $4, $5)",
        )
        .bind(handle)
        .bind(kind)
        .bind(schema_id)
        .bind(owner_id)
        .bind(t)
        .execute(pool)
        .await?;
    }
    sqlx::query(
        "INSERT INTO proxima_core.memory (handle, t, kind, owner_id, origins)
         VALUES ($1, $2, $3::proxima_core.memory_kind, $4, $5)",
    )
    .bind(handle)
    .bind(t)
    .bind(kind)
    .bind(owner_id)
    .bind(origins)
    .execute(pool)
    .await?;
    Ok((handle, t))
}

/// Insert one timeseries Goal row. Returns `(handle, t)`.
pub async fn seed_goal(
    pool: &sqlx::PgPool,
    owner: &Owner,
    schema_id: &str,
    title: &str,
    request_id: &str,
    t: Option<Uuid>,
    assignment_t: Option<Uuid>,
) -> Result<(Uuid, Uuid), sqlx::Error> {
    let handle = Uuid::now_v7();
    let t = t.unwrap_or_else(Uuid::now_v7);
    let owner_id = owner.stored_owner_id();
    sqlx::query(
        "INSERT INTO proxima_core.owners (owner_id, kind)
         VALUES ($1, $2::proxima_core.owner_kind) ON CONFLICT DO NOTHING",
    )
    .bind(owner_id)
    .bind(proxima_core::OwnerRefKind::of(owner).as_str())
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.goal_head (handle, schema_id, owner_id, t)
         VALUES ($1, $2, $3, $4)",
    )
    .bind(handle)
    .bind(schema_id)
    .bind(owner_id)
    .bind(t)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.goal (handle, t, owner_id, title, state, request_id, assignment_t)
         VALUES ($1, $2, $3, $4, 'Active', $5, $6)",
    )
    .bind(handle)
    .bind(t)
    .bind(owner_id)
    .bind(title)
    .bind(request_id)
    .bind(assignment_t)
    .execute(pool)
    .await?;
    Ok((handle, t))
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

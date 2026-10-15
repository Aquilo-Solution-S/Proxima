#![allow(dead_code, clippy::missing_panics_doc)]

use std::path::Path;
use std::process::Command;

use proxima_core::storage_ports::OwnerWritePermit;
use proxima_core::{
    AccessKind, AuthPath, AuthzContext, Engine, FlavorRegistry, FlavorRegistryFrozen, OwnerId,
    Role, SchemaId, SchemaVersion, UserId,
};
use proxima_pg_testkit::{
    DbGuard, FNV_OFFSET_BASIS, create_db_from_template, db_url, drop_db, ensure_template,
    fnv1a64_extend,
};
use proxima_storage_pg::test_fixtures::core_migrator_before_owner_rls;
use proxima_storage_pg::{
    PgSidecarRegistry, PgSidecarRegistryFrozen, PgStorage, register_core_pg_sidecars,
};
use sqlx::migrate::Migrator;
use std::borrow::Cow;
use uuid::Uuid;

pub async fn migrated_db() -> (String, PgStorage) {
    let template = code_template_name();
    ensure_code_template(
        &template,
        core_migrator_before_owner_rls(),
        pre_owner_rls_code_migrator(),
    )
    .await;
    open_migrated_db(&template).await
}

/// Build the owner-RLS installer fixture in production order: core stops
/// before 0014, while the code schema stops before its v0.0.15 policy block.
/// The later core and code migrations are then applied by the test under test.
pub async fn migrated_db_before_owner_rls() -> (String, PgStorage) {
    let template = code_template_before_owner_rls_name();
    let mut core = proxima_storage_pg::core_migrator();
    core.migrations = Cow::Owned(
        core.iter()
            .filter(|migration| migration.version < 14)
            .cloned()
            .collect(),
    );
    ensure_code_template(&template, core, code_migrator_before_v015()).await;
    open_migrated_db(&template).await
}

async fn ensure_code_template(template: &str, core_lane: Migrator, flavor_lane: Migrator) {
    ensure_template(template, |pool| async move {
        core_lane.run(&pool).await.map_err(sqlx::Error::from)?;
        flavor_lane.run(&pool).await.map_err(sqlx::Error::from)
    })
    .await
    .unwrap_or_else(|e| panic!("PG required for tests but admin connect failed: {e}"));
}

async fn open_migrated_db(template: &str) -> (String, PgStorage) {
    let db_name = create_db_from_template("proxima_code_test", template)
        .await
        .unwrap_or_else(|e| panic!("PG required for tests but admin connect failed: {e}"));
    let pg = match PgStorage::connect(&db_url(&db_name)).await {
        Ok(pg) => pg,
        Err(err) => {
            let _ = drop_db(&db_name).await;
            panic!("PG required for tests but unavailable: {err}");
        }
    }
    .with_sidecars(code_pg_sidecars())
    .with_flavors(&proxima_code::schema_registry());
    (db_name, pg)
}

/// The code lane's owner-RLS files: the v0.0.15 hand-written policies, the
/// v0.0.20 installer call that replaces them and the v0.0.29 kind-rule call.
/// As in core's lane, the historical fixtures leave exactly these out and
/// apply every later file.
const OWNER_RLS_MIGRATION_VERSIONS: [i64; 3] =
    [20_260_922_000_020, 20_260_924_000_020, 20_260_929_000_020];

fn pre_owner_rls_code_migrator() -> Migrator {
    let mut migrator = proxima_code::migrator();
    migrator.migrations = Cow::Owned(
        migrator
            .iter()
            .filter(|migration| !OWNER_RLS_MIGRATION_VERSIONS.contains(&migration.version))
            .cloned()
            .collect(),
    );
    migrator
}

fn code_migrator_before_v015() -> Migrator {
    let mut migrator = proxima_code::migrator();
    migrator.migrations = Cow::Owned(
        migrator
            .iter()
            .filter(|migration| migration.version < 20_260_922_000_020)
            .cloned()
            .collect(),
    );
    migrator
}

pub const TEST_CITED_BLOB_SCHEMA_ID: &str = "test/cited_blob";

pub fn code_registry_with_test_citations() -> FlavorRegistryFrozen {
    let mut registry = FlavorRegistry::new();
    proxima_code::register(&mut registry).expect("code schema registration");
    registry.add_opaque_cited_object_schema(
        SchemaId::new(TEST_CITED_BLOB_SCHEMA_ID.into()),
        SchemaVersion::new(1),
    );
    registry.freeze_or_panic_for_tests()
}

/// Run the code flavor's own projection maintenance statement for a
/// hand-seeded sidecar row.
///
/// Tests that write sidecar rows with raw SQL (they are exercising the READ
/// path) must keep the projection themselves. They do it with the
/// generator's statement, never a restated INSERT.
pub async fn project_code(
    pool: &sqlx::PgPool,
    t: uuid::Uuid,
    schema_id: &str,
    language: Option<&str>,
) -> Result<(), sqlx::Error> {
    let contract = &proxima_code::contract::CODE_FLAVOR_CONTRACT;
    let schema = contract
        .schemas
        .iter()
        .find(|schema| schema.schema_id().as_str() == schema_id)
        .expect("declared schema");
    let sql = proxima_storage_pg::projection::projection_insert_sql(contract, schema)
        .expect("the generator emits a valid statement");
    // SQL-POLICY: generated
    sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(t)
        .bind(language)
        .bind(schema_id)
        .execute(pool)
        .await?;
    Ok(())
}

#[must_use]
pub fn test_owner() -> OwnerId {
    OwnerId::from(UserId::new(Uuid::now_v7()))
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
    owner: &OwnerId,
    kind: AccessKind,
) -> Result<OwnerWritePermit, Box<dyn std::error::Error>> {
    let authz = AuthzContext::for_subject_with_role(
        UserId::new(Uuid::now_v7()),
        [(*owner, Role::admin())],
        AuthPath::HostBearer,
    );
    let engine = Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests());
    Ok(engine.authorize_owner_write(&authz, owner, kind).await?)
}

/// Insert one timeseries Memory row. Returns `(handle, t)`.
pub async fn seed_memory(
    pool: &sqlx::PgPool,
    owner: &OwnerId,
    schema_id: &str,
    kind: &str,
    t: Option<Uuid>,
    handle: Option<Uuid>,
    origins: &[Uuid],
) -> Result<(Uuid, Uuid), sqlx::Error> {
    seed_memory_with_sidecars(pool, owner, schema_id, kind, t, handle, origins, &[]).await
}

/// Same as [`seed_memory`], with `memory.sidecar_tables` set at insert
/// (the row is append-only).
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub async fn seed_memory_with_sidecars(
    pool: &sqlx::PgPool,
    owner: &OwnerId,
    schema_id: &str,
    kind: &str,
    t: Option<Uuid>,
    handle: Option<Uuid>,
    origins: &[Uuid],
    sidecar_tables: &[&str],
) -> Result<(Uuid, Uuid), sqlx::Error> {
    let mut tx = pool.begin().await?;
    let seeded = seed_memory_with_sidecars_in_tx(
        &mut tx,
        owner,
        schema_id,
        kind,
        t,
        handle,
        origins,
        sidecar_tables,
    )
    .await?;
    tx.commit().await?;
    Ok(seeded)
}

/// [`seed_memory_with_sidecars`] inside a caller's transaction, for a fixture
/// that has to write the sidecar rows its stamp promises.
///
/// Since `0009_declared_sidecar_presence.sql` a memory row that names a
/// registered sidecar table it has no row in is refused at `COMMIT`, so the
/// stamp and the rows have to land together and the caller's inserts need the
/// same transaction the memory row went into.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub async fn seed_memory_with_sidecars_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    owner: &OwnerId,
    schema_id: &str,
    kind: &str,
    t: Option<Uuid>,
    handle: Option<Uuid>,
    origins: &[Uuid],
    sidecar_tables: &[&str],
) -> Result<(Uuid, Uuid), sqlx::Error> {
    let handle = handle.unwrap_or_else(Uuid::now_v7);
    let t = t.unwrap_or_else(Uuid::now_v7);
    let mut origins = origins.to_vec();
    if kind != "fact" && origins.is_empty() {
        let (_, fact_t) = Box::pin(seed_memory_with_sidecars_in_tx(
            tx,
            owner,
            "core/test-fact-v1",
            "fact",
            None,
            None,
            &[],
            &[],
        ))
        .await?;
        if kind == "perspective" {
            let (_, abs_t) = Box::pin(seed_memory_with_sidecars_in_tx(
                tx,
                owner,
                "core/test-abs-v1",
                "abstraction",
                None,
                None,
                &[fact_t],
                &[],
            ))
            .await?;
            origins.push(abs_t);
        } else {
            origins.push(fact_t);
        }
    }
    let owner_id = owner.into_inner();
    sqlx::query(
        "INSERT INTO proxima_core.owners (owner_id, kind)
         VALUES ($1, 'personal'::proxima_core.owner_kind) ON CONFLICT DO NOTHING",
    )
    .bind(owner_id)
    .execute(&mut **tx)
    .await?;
    let head_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM proxima_core.memory_head WHERE handle = $1)",
    )
    .bind(handle)
    .fetch_one(&mut **tx)
    .await?;
    if head_exists {
        sqlx::query("UPDATE proxima_core.memory_head SET t = $2 WHERE handle = $1")
            .bind(handle)
            .bind(t)
            .execute(&mut **tx)
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
        .execute(&mut **tx)
        .await?;
    }
    let tables: Vec<String> = sidecar_tables.iter().map(|s| (*s).to_string()).collect();
    let content_id: Option<Uuid> = if kind == "fact" {
        None
    } else {
        let mut hash = [0_u8; 32];
        hash[..16].copy_from_slice(t.as_bytes());
        hash[16..].copy_from_slice(t.as_bytes());
        Some(
            sqlx::query_scalar(
                "INSERT INTO proxima_core.content (owner_id, schema_id, content_hash)
                 VALUES ($1, $2, $3)
                 RETURNING content_id",
            )
            .bind(owner_id)
            .bind(schema_id)
            .bind(hash.as_slice())
            .fetch_one(&mut **tx)
            .await?,
        )
    };
    sqlx::query(
        "INSERT INTO proxima_core.memory
            (handle, t, kind, owner_id, schema_id, origins, sidecar_tables, content_id, occurred_at)
         VALUES ($1, $2, $3::proxima_core.memory_kind, $4, $5, $6, $7, $8, CASE WHEN $3::proxima_core.memory_kind = 'fact' THEN uuid_extract_timestamp($2) END)",
    )
    .bind(handle)
    .bind(t)
    .bind(kind)
    .bind(owner_id)
    .bind(schema_id)
    .bind(origins)
    .bind(&tables)
    .bind(content_id)
    .execute(&mut **tx)
    .await?;
    Ok((handle, t))
}

/// Insert one timeseries Goal row. Returns `(handle, t)`.
pub async fn seed_goal(
    pool: &sqlx::PgPool,
    owner: &OwnerId,
    schema_id: &str,
    title: &str,
    request_id: &str,
    t: Option<Uuid>,
    assignment_t: Option<Uuid>,
) -> Result<(Uuid, Uuid), sqlx::Error> {
    let handle = Uuid::now_v7();
    let t = t.unwrap_or_else(Uuid::now_v7);
    let owner_id = owner.into_inner();
    sqlx::query(
        "INSERT INTO proxima_core.owners (owner_id, kind)
         VALUES ($1, 'personal'::proxima_core.owner_kind) ON CONFLICT DO NOTHING",
    )
    .bind(owner_id)
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
    _guard: DbGuard,
    pub name: String,
    pub pg: PgStorage,
}

impl TestDb {
    pub async fn fresh() -> Self {
        let (name, pg) = migrated_db().await;
        let guard = DbGuard::adopt(name.clone());
        Self {
            _guard: guard,
            name,
            pg,
        }
    }

    pub async fn fresh_before_owner_rls() -> Self {
        let (name, pg) = migrated_db_before_owner_rls().await;
        let guard = DbGuard::adopt(name.clone());
        Self {
            _guard: guard,
            name,
            pg,
        }
    }
}

fn code_template_name() -> String {
    let mut hash = FNV_OFFSET_BASIS;
    for migration in core_migrator_before_owner_rls().iter() {
        hash = fnv1a64_extend(hash, &migration.version.to_be_bytes());
        hash = fnv1a64_extend(hash, migration.checksum.as_ref());
    }
    for migration in pre_owner_rls_code_migrator().iter() {
        hash = fnv1a64_extend(hash, &migration.version.to_be_bytes());
        hash = fnv1a64_extend(hash, migration.checksum.as_ref());
    }
    format!("proxima_tmpl_code_{hash:016x}")
}

fn code_template_before_owner_rls_name() -> String {
    let mut hash = FNV_OFFSET_BASIS;
    for migration in proxima_storage_pg::core_migrator()
        .iter()
        .filter(|migration| migration.version < 14)
    {
        hash = fnv1a64_extend(hash, &migration.version.to_be_bytes());
        hash = fnv1a64_extend(hash, migration.checksum.as_ref());
    }
    for migration in code_migrator_before_v015().iter() {
        hash = fnv1a64_extend(hash, &migration.version.to_be_bytes());
        hash = fnv1a64_extend(hash, migration.checksum.as_ref());
    }
    format!("proxima_tmpl_code_before_owner_rls_{hash:016x}")
}

/// Register a repository, because every repo-scoped payload declares the
/// `code-repo` lifecycle scope and the substrate fences its admission on a
/// registry row being there.
///
/// A fixture that mints a `repo_id` and writes rows under it without this is
/// asking the Engine to admit a write into a scope that does not exist,
/// which is the refusal `flavors/code/tests/repo_fence_pg.rs` exists to pin.
///
/// # Panics
///
/// Panics when the registration fails.
pub async fn register_fixture_repo(pool: &sqlx::PgPool, owner: &OwnerId, repo_id: Uuid) {
    proxima_code::testkit::register_repo(
        pool,
        None,
        owner,
        repo_id,
        &format!("/tmp/proxima-code-fixture-{repo_id}"),
        "test fixture",
        &proxima_code::RepoScope::default(),
    )
    .await
    .expect("fixture repo registration");
}

/// This flavor's frozen PG sidecar registry — the same handle production
/// composes.
pub fn code_pg_sidecars() -> PgSidecarRegistryFrozen {
    let registry = proxima_code::schema_registry();
    let mut sidecars = PgSidecarRegistry::new();
    register_core_pg_sidecars(&mut sidecars);
    proxima_code::register_pg_sidecars(&mut sidecars);
    sidecars
        .freeze_against(&registry)
        .expect("code test PG sidecars match code schema registry")
}

/// Current production migrations, with the caller's admin pool retained only
/// for independently seeding and checking fixture data.
pub async fn apply_current_migrations(pg: &PgStorage) -> Result<(), Box<dyn std::error::Error>> {
    let database: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(pg.pool_for_tests())
        .await?;
    let (_, platform_url) = proxima_pg_testkit::split_role_urls(&database).await?;
    proxima_storage_pg::test_fixtures::reopen_migrations_after_owner_rls_lane(pg.pool_for_tests())
        .await?;
    let platform = PgStorage::connect_for_migrations_with_config(
        &platform_url,
        proxima_storage_pg::PgPoolConfig::default(),
        proxima_storage_pg::PgTuning::default(),
    )
    .await?;
    proxima::run_core_and_flavor_migrations(
        &platform,
        <proxima_code::CodeFlavor as proxima::flavor::FlavorBundle>::migrators(),
    )
    .await?;
    Ok(())
}

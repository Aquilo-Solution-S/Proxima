//! Boot the embedded facade with automatic owner-RLS activation using split
//! platform/runtime roles.  This is deliberately a boot test: the lower-level
//! policy census and role refusal matrix live in storage-pg's RLS tests.

use async_trait::async_trait;
use proxima::flavor::{FlavorBundle, FlavorRegistry, FlavorRegistryError, NamedMigrator};
use proxima::{AppInfo, FactWrite, FlavorApp, Proxima, QueryRequest, ToolScope};
use proxima_core::{
    AccessKind, AgentNoteV1, AuthError, AuthPath, Authenticator, AuthzContext, Credentials,
    ErrorCode, GroupId, OwnerId, OwnerRoles, UserId,
};
use proxima_pg_testkit::{admin_url, create_db, db_url, drop_db, unique_db_name};
use proxima_storage_pg::PgPoolConfig;
use sqlx::postgres::PgConnectOptions;
use sqlx::{AssertSqlSafe, ConnectOptions, PgPool};
use std::str::FromStr;
use std::sync::OnceLock;

static BOOT_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

struct SyntheticAuthenticator {
    subject: UserId,
}
#[async_trait]
impl Authenticator for SyntheticAuthenticator {
    async fn authenticate(&self, _: &Credentials) -> Result<AuthzContext, AuthError> {
        Ok(AuthzContext::server_resolved(
            OwnerRoles::for_subject(self.subject, []).unwrap(),
            AuthPath::HostBearer,
        ))
    }
}

fn ident(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

async fn exec(pool: &PgPool, sql: impl Into<String>) {
    sqlx::raw_sql(AssertSqlSafe(sql.into()))
        .execute(pool)
        .await
        .expect("fixture SQL");
}

/// The platform role migrates with no parameter ACL: owner RLS must activate
/// without any superuser-only grant.
async fn assert_no_scope_parameter_grant(admin: &PgPool, platform: &str) {
    let granted: bool =
        sqlx::query_scalar("SELECT has_parameter_privilege($1, 'app.proxima_scope', 'SET')")
            .bind(platform)
            .fetch_one(admin)
            .await
            .expect("parameter privilege probe");
    assert!(
        !granted,
        "fixture platform role must not hold SET on app.proxima_scope"
    );
}

async fn cleanup(database: &str, admin: PgPool, runtime: &str, platform: &str) {
    admin.close().await;
    drop_db(database).await.expect("drop database");
    let control = PgPool::connect(&admin_url())
        .await
        .expect("control connection");
    // SQL-POLICY: fixed-fragment — uniquely generated fixture roles, quoted identifiers.
    exec(&control, format!("DROP ROLE IF EXISTS {}", ident(runtime))).await;
    exec(&control, format!("DROP ROLE IF EXISTS {}", ident(platform))).await;
    control.close().await;
}

async fn setup() -> (String, PgPool, String, String, OwnerId, String, String) {
    let (database, admin, runtime_url, platform_url, runtime, platform) = setup_fresh().await;
    let migration = proxima_storage_pg::PgMigrationStorage::connect_with_config(
        &platform_url,
        PgPoolConfig::default(),
        proxima_storage_pg::PgTuning::default(),
    )
    .await
    .expect("platform storage");
    migration.run_migrations().await.expect("core schema");
    migration.clone_pool_for_backend().close().await;
    let owner = OwnerId::from(UserId::new(uuid::Uuid::now_v7()));
    sqlx::query("INSERT INTO proxima_core.owners (owner_id, kind) VALUES ($1, 'personal')")
        .bind(owner.into_inner())
        .execute(&admin)
        .await
        .expect("fixture owner");
    (
        database,
        admin,
        runtime_url,
        platform_url,
        owner,
        runtime,
        platform,
    )
}

async fn setup_fresh() -> (String, PgPool, String, String, String, String) {
    let database = unique_db_name("proxima_owner_rls_fresh");
    create_db(&database).await.expect("create database");
    let admin = PgPool::connect(&db_url(&database)).await.expect("admin");
    exec(
        &admin,
        "CREATE EXTENSION IF NOT EXISTS vector; CREATE EXTENSION IF NOT EXISTS btree_gin; CREATE EXTENSION IF NOT EXISTS pg_trgm",
    )
    .await;
    let suffix = uuid::Uuid::now_v7().simple().to_string();
    let platform = format!("owner_rls_fresh_platform_{suffix}");
    let runtime = format!("owner_rls_fresh_runtime_{suffix}");
    let password = format!("pw_{suffix}");
    for role in [&platform, &runtime] {
        exec(
            &admin,
            format!(
                "CREATE ROLE {} LOGIN PASSWORD '{}' NOSUPERUSER NOBYPASSRLS",
                ident(role),
                password
            ),
        )
        .await;
    }
    exec(
        &admin,
        format!(
            "GRANT CREATE ON DATABASE {} TO {}; GRANT USAGE, CREATE ON SCHEMA public TO {}; ALTER DEFAULT PRIVILEGES FOR ROLE {} GRANT USAGE ON SCHEMAS TO {}; ALTER DEFAULT PRIVILEGES FOR ROLE {} GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO {}; ALTER DEFAULT PRIVILEGES FOR ROLE {} GRANT USAGE, SELECT ON SEQUENCES TO {}",
            ident(&database), ident(&platform), ident(&platform), ident(&platform), ident(&runtime),
            ident(&platform), ident(&runtime), ident(&platform), ident(&runtime)
        ),
    )
    .await;
    assert_no_scope_parameter_grant(&admin, &platform).await;
    let runtime_url = PgConnectOptions::from_str(&db_url(&database))
        .expect("database URL")
        .username(&runtime)
        .password(&password)
        .to_url_lossy()
        .to_string();
    let platform_url = PgConnectOptions::from_str(&db_url(&database))
        .expect("database URL")
        .username(&platform)
        .password(&password)
        .to_url_lossy()
        .to_string();
    (
        database,
        admin,
        runtime_url,
        platform_url,
        runtime,
        platform,
    )
}

struct CodeApp;

impl FlavorBundle for CodeApp {
    fn register(registry: &mut FlavorRegistry) -> Result<(), FlavorRegistryError> {
        proxima_code::CodeFlavor::register(registry)
    }

    fn register_pg_sidecars(registry: &mut proxima::flavor::PgSidecarRegistry) {
        proxima_code::CodeFlavor::register_pg_sidecars(registry);
    }

    fn migrators() -> Vec<NamedMigrator> {
        proxima_code::CodeFlavor::migrators()
    }
}

impl FlavorApp for CodeApp {
    fn app_info() -> AppInfo {
        AppInfo {
            id: "owner-rls-boot-test",
            title: "Owner RLS Boot Test",
            version: "1",
        }
    }
}

async fn boot(
    runtime_url: String,
    platform_url: Option<String>,
    owner: OwnerId,
    skip_migrations: bool,
) -> Result<proxima::BuiltProxima, proxima::ProximaError> {
    let mut app = Proxima::<CodeApp>::app()
        .database_url(runtime_url)
        .owner(owner)
        .tool_scope(ToolScope::All)
        .pg_pool_config(PgPoolConfig::default());
    if let Some(platform_url) = platform_url {
        app = app.platform_database_url(platform_url);
    }
    if skip_migrations {
        app = app.skip_migrations();
    }
    app.build().await
}

#[tokio::test]
async fn split_role_boot_accepts_runtime_with_platform_scope() {
    let _guard = BOOT_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let (database, admin, runtime_url, platform_url, owner, runtime, platform) = setup().await;
    let result = boot(
        runtime_url.clone(),
        Some(platform_url.clone()),
        owner,
        false,
    )
    .await;
    let booted = result.expect("split-role boot");
    let authz = proxima_core::authenticate(
        &SyntheticAuthenticator {
            subject: owner.as_user(),
        },
        &Credentials::Bearer("synthetic-token".into()),
    )
    .await
    .expect("verified synthetic authentication");
    let note = AgentNoteV1 {
        note_id: uuid::Uuid::now_v7(),
        title: "RLS boot roundtrip".into(),
        body: "verified owner scope".into(),
        tags: Vec::new(),
        idempotency_key: Some("owner-rls-boot".into()),
    };
    let outcome = booted
        .host()
        .engine()
        .ingest_fact(&authz, FactWrite::new(owner, "test/owner-rls", &note))
        .await
        .expect("verified fact ingest");
    let response = booted
        .host()
        .engine()
        .query(&authz, &QueryRequest::readable())
        .await
        .expect("verified owner query");
    assert!(
        response
            .memories
            .iter()
            .any(|memory| memory.id == outcome.memory_id)
    );
    let verify = PgPool::connect(&runtime_url)
        .await
        .expect("runtime verify pool");
    let mut verify_tx = verify.begin().await.expect("runtime verify transaction");
    sqlx::query("SELECT set_config('app.proxima_scope', 'owner', true), set_config('app.owner', $1::text, true)")
        .bind(format!("{{{}}}", owner.into_inner()))
        .execute(&mut *verify_tx)
        .await
        .expect("bind existing owner scope");
    let existing_owner: i64 =
        sqlx::query_scalar("SELECT count(*) FROM proxima_core.owners WHERE owner_id = $1")
            .bind(owner.into_inner())
            .fetch_one(&mut *verify_tx)
            .await
            .expect("existing owner survives upgrade");
    assert_eq!(existing_owner, 1);
    verify_tx
        .rollback()
        .await
        .expect("rollback verify transaction");
    verify.close().await;
    booted.shutdown().await;
    let second = boot(runtime_url, Some(platform_url), owner, false)
        .await
        .expect("second automatic migration is idempotent");
    second.shutdown().await;

    cleanup(&database, admin, &runtime, &platform).await;
}

#[tokio::test]
async fn fresh_split_role_boot_migrates_core_and_code_automatically() {
    let _guard = BOOT_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let (database, admin, runtime_url, platform_url, runtime, platform) = setup_fresh().await;
    let owner = OwnerId::from(UserId::new(uuid::Uuid::now_v7()));
    let first = boot(
        runtime_url.clone(),
        Some(platform_url.clone()),
        owner,
        false,
    )
    .await
    .expect("fresh core+Code automatic migration");
    first.shutdown().await;
    let (base_tables, flavor_tables): (i64, i64) = sqlx::query_as(
        "SELECT
           (SELECT count(*) FROM information_schema.tables WHERE table_schema = 'proxima_core'),
           (SELECT count(*) FROM information_schema.tables WHERE table_schema = 'proxima_code')",
    )
    .fetch_one(&admin)
    .await
    .expect("fresh schema census");
    assert!(base_tables > 0);
    assert!(flavor_tables > 0);
    exec(
        &admin,
        format!(
            "INSERT INTO proxima_core.owners(owner_id, kind) VALUES ('{}', 'personal')",
            owner.into_inner()
        ),
    )
    .await;
    let second = boot(runtime_url, Some(platform_url), owner, true)
        .await
        .expect("fresh second boot");
    second.shutdown().await;
    cleanup(&database, admin, &runtime, &platform).await;
}

async fn assert_insecure_single_owner_roundtrip(owner: OwnerId) {
    let _guard = BOOT_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let (database, admin, runtime_url, platform_url, runtime, platform) = setup_fresh().await;
    let built = Proxima::<CodeApp>::app()
        .database_url(runtime_url)
        .platform_database_url(platform_url)
        .owner(owner)
        .allow_insecure_single_owner()
        .tool_scope(ToolScope::All)
        .build()
        .await
        .expect("insecure single-owner boot with enforcing split roles");
    let authz = built.single_owner_authz().expect("explicitly opted in");
    assert_eq!(authz.auth_path(), AuthPath::HostBearer);
    let scope = authz.owner_scope().expect("sealed host owner scope");
    assert_eq!(scope.readable_owners(AccessKind::Fact), vec![owner]);
    assert_eq!(scope.writable_owners(AccessKind::Fact), vec![owner]);

    let foreign_subject = UserId::new(uuid::Uuid::now_v7());
    let foreign_owner = OwnerId::from(foreign_subject);
    let foreign_authz = proxima_core::authenticate(
        &SyntheticAuthenticator {
            subject: foreign_subject,
        },
        &Credentials::Bearer("synthetic-token".into()),
    )
    .await
    .expect("foreign owner authentication");
    let note = AgentNoteV1 {
        note_id: uuid::Uuid::now_v7(),
        title: "Insecure host owner RLS".into(),
        body: "Only the configured owner can read and write".into(),
        tags: Vec::new(),
        idempotency_key: None,
    };
    let engine = built.host().engine();
    let foreign = engine
        .ingest_fact(
            &foreign_authz,
            FactWrite::new(foreign_owner, "test/foreign-owner", &note),
        )
        .await
        .expect("seed foreign owner's private fact");
    let own = engine
        .ingest_fact(&authz, FactWrite::new(owner, "test/insecure-owner", &note))
        .await
        .expect("raw single-owner context writes under owner RLS");
    let response = engine
        .query(&authz, &QueryRequest::readable())
        .await
        .expect("raw single-owner context reads under owner RLS");
    assert!(response.memories.iter().any(|row| row.id == own.memory_id));
    assert!(response.memories.iter().all(|row| row.owner == owner));
    assert!(
        response
            .memories
            .iter()
            .all(|row| row.id != foreign.memory_id)
    );
    let denied = engine
        .ingest_fact(
            &authz,
            FactWrite::new(foreign_owner, "test/forbidden-owner", &note),
        )
        .await
        .expect_err("single-owner context cannot write the foreign owner");
    assert_eq!(denied.code, ErrorCode::Forbidden);
    assert!(authz.narrowed_to_owner(foreign_owner).is_none());
    built.shutdown().await;
    cleanup(&database, admin, &runtime, &platform).await;
}

#[tokio::test]
async fn insecure_personal_owner_reads_and_writes_under_split_role_rls() {
    assert_insecure_single_owner_roundtrip(OwnerId::from(UserId::new(uuid::Uuid::now_v7()))).await;
}

#[tokio::test]
async fn insecure_group_owner_reads_and_writes_under_split_role_rls() {
    assert_insecure_single_owner_roundtrip(OwnerId::from(GroupId::new(uuid::Uuid::now_v7()))).await;
}

#[tokio::test]
async fn split_role_boot_refuses_missing_platform_scope() {
    let _guard = BOOT_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let (database, admin, runtime_url, _platform_url, owner, runtime, platform) = setup().await;
    let result = boot(runtime_url, None, owner, false).await;
    assert!(
        result.is_err(),
        "enforced owner RLS must require a platform database URL"
    );
    cleanup(&database, admin, &runtime, &platform).await;
}

#[tokio::test]
async fn split_role_boot_refuses_runtime_table_owner() {
    let _guard = BOOT_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let (database, admin, runtime_url, platform_url, owner, runtime, platform) = setup().await;
    boot(
        runtime_url.clone(),
        Some(platform_url.clone()),
        owner,
        false,
    )
    .await
    .expect("initial automatic migration");
    exec(&admin, format!("DO $$ DECLARE r record; BEGIN FOR r IN SELECT n.nspname, c.relname FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = 'proxima_core' AND c.relkind IN ('r','p') LOOP EXECUTE format('ALTER TABLE %I.%I OWNER TO {}', r.nspname, r.relname); END LOOP; END $$", ident(&runtime))).await;
    assert!(
        boot(runtime_url, Some(platform_url), owner, true)
            .await
            .is_err()
    );
    cleanup(&database, admin, &runtime, &platform).await;
}

#[tokio::test]
async fn split_role_boot_refuses_runtime_bypassrls() {
    let _guard = BOOT_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let (database, admin, runtime_url, platform_url, owner, runtime, platform) = setup().await;
    // SQL-POLICY: fixed-fragment — generated role identifier is quoted.
    boot(
        runtime_url.clone(),
        Some(platform_url.clone()),
        owner,
        false,
    )
    .await
    .expect("initial automatic migration");
    // SQL-POLICY: fixed-fragment — the generated fixture role is identifier-quoted.
    exec(&admin, format!("ALTER ROLE {} BYPASSRLS", ident(&runtime))).await;
    assert!(
        boot(runtime_url, Some(platform_url), owner, true)
            .await
            .is_err()
    );
    cleanup(&database, admin, &runtime, &platform).await;
}

#[tokio::test]
async fn split_role_boot_refuses_missing_force_rls() {
    let _guard = BOOT_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let (database, admin, runtime_url, platform_url, owner, runtime, platform) = setup().await;
    boot(
        runtime_url.clone(),
        Some(platform_url.clone()),
        owner,
        false,
    )
    .await
    .expect("initial automatic migration");
    exec(
        &admin,
        "ALTER TABLE proxima_core.memory NO FORCE ROW LEVEL SECURITY",
    )
    .await;
    assert!(
        boot(runtime_url, Some(platform_url), owner, true)
            .await
            .is_err()
    );
    cleanup(&database, admin, &runtime, &platform).await;
}

#[tokio::test]
async fn split_role_boot_refuses_unprotected_new_table() {
    let _guard = BOOT_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let (database, admin, runtime_url, platform_url, owner, runtime, platform) = setup().await;
    boot(
        runtime_url.clone(),
        Some(platform_url.clone()),
        owner,
        false,
    )
    .await
    .expect("initial automatic migration");
    exec(&admin, "CREATE TABLE proxima_core.unprotected_sidecar (id uuid PRIMARY KEY, owner_id uuid NOT NULL)").await;
    assert!(
        boot(runtime_url, Some(platform_url), owner, true)
            .await
            .is_err()
    );
    cleanup(&database, admin, &runtime, &platform).await;
}

#[tokio::test]
async fn split_role_boot_refuses_mismatched_baseline_checksum() {
    let _guard = BOOT_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let (database, admin, runtime_url, platform_url, owner, runtime, platform) = setup().await;
    boot(
        runtime_url.clone(),
        Some(platform_url.clone()),
        owner,
        false,
    )
    .await
    .expect("initial automatic migration");
    sqlx::query(
        "UPDATE public._sqlx_migrations SET checksum = decode('00', 'hex') WHERE version = 33",
    )
    .execute(&admin)
    .await
    .expect("corrupt baseline checksum");
    assert!(
        boot(runtime_url, Some(platform_url), owner, true)
            .await
            .is_err()
    );
    cleanup(&database, admin, &runtime, &platform).await;
}

#[tokio::test]
async fn split_role_boot_refuses_unrelated_newer_ledger_version() {
    let _guard = BOOT_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let (database, admin, runtime_url, platform_url, owner, runtime, platform) = setup().await;
    boot(
        runtime_url.clone(),
        Some(platform_url.clone()),
        owner,
        false,
    )
    .await
    .expect("initial automatic migration");
    sqlx::query("INSERT INTO public._sqlx_migrations (version, description, success, checksum, execution_time) VALUES (9000, 'unrelated', true, decode('00', 'hex'), 0)")
        .execute(&admin).await.expect("insert unrelated ledger row");
    assert!(
        boot(runtime_url, Some(platform_url), owner, true)
            .await
            .is_err()
    );
    cleanup(&database, admin, &runtime, &platform).await;
}

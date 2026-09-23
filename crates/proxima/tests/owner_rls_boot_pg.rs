//! Boot the embedded facade with automatic owner-RLS activation using split
//! platform/runtime roles.  This is deliberately a boot test: the lower-level
//! policy census and role refusal matrix live in storage-pg's RLS tests.

use async_trait::async_trait;
use proxima::{EmbedConfig, FactWrite, ProximaBuilder, QueryRequest};
use proxima_core::{
    AgentNoteV1, AuthError, AuthPath, Authenticator, AuthzContext, Credentials, Owner, OwnerRoles,
    UserId,
};
use proxima_pg_testkit::{admin_url, create_db, db_url, drop_db, unique_db_name};
use proxima_storage_pg::PgPoolConfig;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
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

async fn role_pool(database: &str, role: &str, password: &str) -> PgPool {
    let options = PgConnectOptions::from_str(&db_url(database))
        .expect("database URL")
        .username(role)
        .password(password);
    PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options)
        .await
        .expect("role connection")
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

async fn provision_platform_role(admin: &PgPool, platform: &str, runtime: &str, password: &str) {
    for role in [platform, runtime] {
        exec(
            admin,
            format!(
                "CREATE ROLE {} LOGIN PASSWORD '{}' NOSUPERUSER NOBYPASSRLS",
                ident(role),
                password
            ),
        )
        .await;
    }
    exec(admin, format!("DO $$ DECLARE r record; BEGIN FOR r IN SELECT n.nspname, c.relname FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = 'proxima_core' AND c.relkind IN ('r','p') LOOP EXECUTE format('ALTER TABLE %I.%I OWNER TO %I', r.nspname, r.relname, '{platform}'); END LOOP; END $$")).await;
    // SQL-POLICY: fixed-fragment — generated role identifiers are quoted.
    exec(admin, format!("ALTER SCHEMA proxima_core OWNER TO {p}; GRANT USAGE, CREATE ON SCHEMA proxima_core TO {p}", p = ident(platform))).await;
    exec(admin, format!("DO $$ DECLARE r record; BEGIN FOR r IN SELECT n.nspname, p.proname, pg_get_function_identity_arguments(p.oid) AS args FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace WHERE n.nspname = 'proxima_core' LOOP EXECUTE format('ALTER FUNCTION %I.%I(%s) OWNER TO {platform}', r.nspname, r.proname, r.args); END LOOP; END $$")).await;
}

async fn setup() -> (String, PgPool, String, String, Owner, String, String) {
    let database = unique_db_name("proxima_owner_rls_boot");
    create_db(&database).await.expect("create database");
    let admin = PgPool::connect(&db_url(&database)).await.expect("admin");
    exec(&admin, "CREATE EXTENSION IF NOT EXISTS pg_trgm").await;
    proxima_storage_pg::test_fixtures::core_migrator_before_owner_rls()
        .run(&admin)
        .await
        .expect("core migrations");

    let suffix = uuid::Uuid::now_v7().simple().to_string();
    let platform = format!("owner_rls_platform_{suffix}");
    let runtime = format!("owner_rls_runtime_{suffix}");
    let password = format!("pw_{suffix}");
    let owner = Owner::Personal(UserId::new(uuid::Uuid::now_v7()));
    exec(
        &admin,
        format!(
            "INSERT INTO proxima_core.owners (owner_id, kind) VALUES ('{}', 'personal')",
            owner.stored_owner_id()
        ),
    )
    .await;
    provision_platform_role(&admin, &platform, &runtime, &password).await;
    assert_no_scope_parameter_grant(&admin, &platform).await;
    exec(
        &admin,
        format!(
            "GRANT USAGE, CREATE ON SCHEMA public TO {p}; ALTER TABLE public._sqlx_migrations OWNER TO {p}",
            p = ident(&platform)
        ),
    )
    .await;
    exec(
        &admin,
        format!(
            "GRANT CREATE ON DATABASE {} TO {}; ALTER DEFAULT PRIVILEGES FOR ROLE {} GRANT USAGE ON SCHEMAS TO {}; ALTER DEFAULT PRIVILEGES FOR ROLE {} GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO {}; ALTER DEFAULT PRIVILEGES FOR ROLE {} GRANT USAGE, SELECT ON SEQUENCES TO {}",
            ident(&database), ident(&platform), ident(&platform), ident(&runtime),
            ident(&platform), ident(&runtime), ident(&platform), ident(&runtime)
        ),
    )
    .await;
    let platform_pool = role_pool(&database, &platform, &password).await;
    exec(
        &admin,
        format!(
            "GRANT USAGE ON SCHEMA proxima_core TO {}; GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA proxima_core TO {}; GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA proxima_core TO {}; GRANT SELECT ON TABLE public._sqlx_migrations TO {}",
            ident(&runtime), ident(&runtime), ident(&runtime), ident(&runtime)
        ),
    )
    .await;
    let runtime_pool = role_pool(&database, &runtime, &password).await;
    runtime_pool.close().await;
    platform_pool.close().await;
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

async fn boot(
    runtime_url: String,
    platform_url: Option<String>,
    owner: Owner,
    skip_migrations: bool,
) -> Result<proxima::EmbeddedProxima, proxima::EmbedError> {
    let mut builder = ProximaBuilder::new(
        EmbedConfig {
            database_url: runtime_url,
            platform_database_url: platform_url,
            s3: None,
        },
        owner,
    )
    .pg_pool_config(PgPoolConfig::default())
    .bundle::<proxima_code::CodeFlavor>();
    if skip_migrations {
        builder = builder.skip_migrations();
    }
    builder.boot().await
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
            subject: match owner {
                Owner::Personal(subject) => subject,
                Owner::Group(_) => unreachable!(),
            },
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
        .engine
        .ingest_fact(&authz, FactWrite::new(owner, "test/owner-rls", &note))
        .await
        .expect("verified fact ingest");
    let response = booted
        .engine
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
        .bind(format!("{{{}}}", owner.stored_owner_id()))
        .execute(&mut *verify_tx)
        .await
        .expect("bind existing owner scope");
    let existing_owner: i64 =
        sqlx::query_scalar("SELECT count(*) FROM proxima_core.owners WHERE owner_id = $1")
            .bind(owner.stored_owner_id())
            .fetch_one(&mut *verify_tx)
            .await
            .expect("existing owner survives upgrade");
    assert_eq!(existing_owner, 1);
    verify_tx
        .rollback()
        .await
        .expect("rollback verify transaction");
    verify.close().await;
    booted.engine.stop(booted.handle);
    let second = boot(runtime_url, Some(platform_url), owner, false)
        .await
        .expect("second automatic migration is idempotent");
    second.engine.stop(second.handle);

    cleanup(&database, admin, &runtime, &platform).await;
}

#[tokio::test]
async fn fresh_split_role_boot_migrates_core_and_code_automatically() {
    let _guard = BOOT_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let (database, admin, runtime_url, platform_url, runtime, platform) = setup_fresh().await;
    let owner = Owner::Personal(UserId::new(uuid::Uuid::now_v7()));
    let first = boot(
        runtime_url.clone(),
        Some(platform_url.clone()),
        owner,
        false,
    )
    .await
    .expect("fresh core+Code automatic migration");
    first.engine.stop(first.handle);
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
            owner.stored_owner_id()
        ),
    )
    .await;
    let second = boot(runtime_url, Some(platform_url), owner, true)
        .await
        .expect("fresh second boot");
    second.engine.stop(second.handle);
    cleanup(&database, admin, &runtime, &platform).await;
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
async fn split_role_boot_refuses_mismatched_owner_rls_checksum() {
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
        "UPDATE public._sqlx_migrations SET checksum = decode('00', 'hex') WHERE version = 14",
    )
    .execute(&admin)
    .await
    .expect("corrupt owner-RLS checksum");
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
    sqlx::query("INSERT INTO public._sqlx_migrations (version, description, success, checksum, execution_time) VALUES (15, 'unrelated', true, decode('00', 'hex'), 0)")
        .execute(&admin).await.expect("insert unrelated ledger row");
    assert!(
        boot(runtime_url, Some(platform_url), owner, true)
            .await
            .is_err()
    );
    cleanup(&database, admin, &runtime, &platform).await;
}

//! Boot-time runtime grants (`PROXIMA_RUNTIME_GRANTS`): the platform role
//! grants a FRESH runtime role — no privileges at all — what it needs to serve
//! under owner RLS, and nothing that escalates.

use proxima::flavor::{FlavorBundle, NamedMigrator, PgSidecarRegistry};
use proxima::{
    AppInfo, EmbedConfig, EmbedError, FactWrite, FlavorApp, Proxima, ProximaBuilder, ProximaError,
    QueryRequest, ToolScope, company_owner,
};
use proxima_core::{
    AgentNoteV1, AuthPath, AuthzContext, FlavorRegistry, FlavorRegistryError, Owner, Role, UserId,
};
use proxima_pg_testkit::{admin_url, create_db, db_url, drop_db, unique_db_name};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{AssertSqlSafe, ConnectOptions, PgPool};
use std::str::FromStr;
use uuid::Uuid;

/// Composed schemas of the Code-flavor bundle.
const SCHEMAS: [&str; 2] = ["proxima_code", "proxima_core"];
/// Migration ledgers of core and the Code flavor.
const LEDGERS: [&str; 2] = [
    "public._sqlx_migrations",
    "public._sqlx_migrations_proxima_code",
];

struct CodeGrantsApp;

impl FlavorBundle for CodeGrantsApp {
    fn register(registry: &mut FlavorRegistry) -> Result<(), FlavorRegistryError> {
        proxima_code::CodeFlavor::register(registry)
    }

    fn register_pg_sidecars(registry: &mut PgSidecarRegistry) {
        proxima_code::CodeFlavor::register_pg_sidecars(registry);
    }

    fn migrators() -> Vec<NamedMigrator> {
        proxima_code::CodeFlavor::migrators()
    }
}

impl FlavorApp for CodeGrantsApp {
    fn app_info() -> AppInfo {
        AppInfo {
            id: "runtime-grants-test",
            title: "Runtime Grants Test",
            version: "1",
        }
    }
}

struct Fixture {
    database: String,
    admin: PgPool,
    runtime: String,
    platform: String,
    password: String,
    runtime_url: String,
    platform_url: String,
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

fn role_url(database: &str, role: &str, password: &str) -> String {
    PgConnectOptions::from_str(&db_url(database))
        .expect("database URL")
        .username(role)
        .password(password)
        .to_url_lossy()
        .to_string()
}

async fn role_pool(fixture: &Fixture, role: &str) -> PgPool {
    let options = PgConnectOptions::from_str(&db_url(&fixture.database))
        .expect("database URL")
        .username(role)
        .password(&fixture.password);
    PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .expect("role connection")
}

/// A fresh database; a platform role that creates (so owns) every object via
/// boot migrations; a runtime role granted NOTHING.
async fn setup() -> Fixture {
    let database = unique_db_name("proxima_runtime_grants");
    create_db(&database).await.expect("create database");
    let admin = PgPool::connect(&db_url(&database)).await.expect("admin");
    exec(
        &admin,
        "CREATE EXTENSION IF NOT EXISTS vector; CREATE EXTENSION IF NOT EXISTS btree_gin; CREATE EXTENSION IF NOT EXISTS pg_trgm",
    )
    .await;
    let suffix = Uuid::now_v7().simple().to_string();
    let platform = format!("rtg_platform_{suffix}");
    let runtime = format!("rtg_runtime_{suffix}");
    let password = format!("pw_{suffix}");
    // SQL-POLICY: fixed-fragment — generated role identifiers are quoted; the
    // password is a generated hex fixture value.
    exec(
        &admin,
        format!(
            "CREATE ROLE {} LOGIN PASSWORD '{password}' NOSUPERUSER NOBYPASSRLS NOINHERIT; \
             CREATE ROLE {} LOGIN PASSWORD '{password}' NOSUPERUSER NOBYPASSRLS NOINHERIT",
            ident(&platform),
            ident(&runtime),
        ),
    )
    .await;
    // SQL-POLICY: fixed-fragment — generated database and role identifiers are quoted.
    exec(
        &admin,
        format!(
            "GRANT CREATE ON DATABASE {} TO {p}; GRANT USAGE, CREATE ON SCHEMA public TO {p}",
            ident(&database),
            p = ident(&platform),
        ),
    )
    .await;
    Fixture {
        runtime_url: role_url(&database, &runtime, &password),
        platform_url: role_url(&database, &platform, &password),
        database,
        admin,
        runtime,
        platform,
        password,
    }
}

async fn cleanup(fixture: Fixture) {
    fixture.admin.close().await;
    drop_db(&fixture.database).await.expect("drop database");
    let control = PgPool::connect(&admin_url())
        .await
        .expect("control connection");
    // SQL-POLICY: fixed-fragment — uniquely generated fixture roles, quoted identifiers.
    exec(
        &control,
        format!(
            "DROP ROLE IF EXISTS {}; DROP ROLE IF EXISTS {}",
            ident(&fixture.runtime),
            ident(&fixture.platform)
        ),
    )
    .await;
    control.close().await;
}

/// Boot through the runtime facade, the path `PROXIMA_RUNTIME_GRANTS` takes.
async fn build(
    fixture: &Fixture,
    owner: Owner,
    runtime_grants: bool,
    skip_migrations: bool,
) -> Result<proxima::BuiltProxima, ProximaError> {
    let mut app = Proxima::<CodeGrantsApp>::app()
        .database_url(fixture.runtime_url.clone())
        .platform_database_url(fixture.platform_url.clone())
        .owner(owner)
        .tool_scope(ToolScope::All)
        .runtime_grants(runtime_grants);
    if skip_migrations {
        app = app.skip_migrations();
    }
    app.build().await
}

fn host_context(owner: Owner) -> AuthzContext {
    proxima_core::test_fixtures::authenticated_context(AuthzContext::for_subject_with_role(
        UserId::new(Uuid::now_v7()),
        [(owner, Role::admin())],
        AuthPath::HostBearer,
    ))
    .narrowed_to_owner(owner)
    .expect("trusted host resolved this exact owner")
}

async fn has(admin: &PgPool, probe: &str, role: &str, object: &str, privilege: &str) -> bool {
    // SQL-POLICY: fixed-fragment — `probe` is a closed privilege function name.
    sqlx::query_scalar(AssertSqlSafe(format!("SELECT {probe}($1, $2, $3)")))
        .bind(role)
        .bind(object)
        .bind(privilege)
        .fetch_one(admin)
        .await
        .expect("privilege probe")
}

async fn assert_runtime_privileges(fixture: &Fixture) {
    let (admin, runtime) = (&fixture.admin, fixture.runtime.as_str());
    for schema in SCHEMAS {
        assert!(has(admin, "has_schema_privilege", runtime, schema, "USAGE").await);
        assert!(
            !has(admin, "has_schema_privilege", runtime, schema, "CREATE").await,
            "runtime must not CREATE in {schema}"
        );
    }
    for ledger in LEDGERS {
        assert!(has(admin, "has_table_privilege", runtime, ledger, "SELECT").await);
        for privilege in ["INSERT", "UPDATE", "DELETE", "TRUNCATE"] {
            assert!(
                !has(admin, "has_table_privilege", runtime, ledger, privilege).await,
                "runtime must not {privilege} {ledger}"
            );
        }
    }
    let (missing_dml, escalating): (i64, i64) = sqlx::query_as(
        "SELECT
           count(*) FILTER (WHERE NOT (has_table_privilege($1, c.oid, 'SELECT')
                                   AND has_table_privilege($1, c.oid, 'INSERT')
                                   AND has_table_privilege($1, c.oid, 'UPDATE')
                                   AND has_table_privilege($1, c.oid, 'DELETE'))),
           count(*) FILTER (WHERE has_table_privilege($1, c.oid, 'TRUNCATE')
                               OR has_table_privilege($1, c.oid, 'REFERENCES')
                               OR has_table_privilege($1, c.oid, 'TRIGGER'))
           FROM pg_class AS c JOIN pg_namespace AS n ON n.oid = c.relnamespace
          WHERE n.nspname = ANY($2::text[]) AND c.relkind IN ('r', 'p')",
    )
    .bind(runtime)
    .bind(SCHEMAS.as_slice())
    .fetch_one(admin)
    .await
    .expect("table privilege census");
    assert_eq!(missing_dml, 0, "every composed table needs runtime DML");
    assert_eq!(escalating, 0, "no TRUNCATE/REFERENCES/TRIGGER for runtime");
    let (memberships, owned): (i64, i64) = sqlx::query_as(
        "SELECT
           (SELECT count(*) FROM pg_auth_members WHERE member = $1::regrole),
           (SELECT count(*) FROM pg_class WHERE relowner = $1::regrole)",
    )
    .bind(runtime)
    .fetch_one(admin)
    .await
    .expect("membership and ownership census");
    assert_eq!((memberships, owned), (0, 0));
}

/// An owner-scoped write and read through the booted engine, under enforced
/// owner RLS, followed by the storage runtime-role guard itself.
async fn assert_runtime_serves(fixture: &Fixture, booted: &proxima::BuiltProxima, owner: Owner) {
    let authz = host_context(owner);
    let note = AgentNoteV1 {
        note_id: Uuid::now_v7(),
        title: "runtime grants".into(),
        body: "granted at boot".into(),
        tags: Vec::new(),
        idempotency_key: Some("runtime-grants-boot".into()),
    };
    let outcome = booted
        .engine
        .ingest_fact(&authz, FactWrite::new(owner, "test/runtime-grants", &note))
        .await
        .expect("runtime DML under owner RLS");
    let response = booted
        .engine
        .query(&authz, &QueryRequest::readable())
        .await
        .expect("runtime read under owner RLS");
    assert!(
        response
            .memories
            .iter()
            .any(|memory| memory.id == outcome.memory_id)
    );
    let runtime_pool = role_pool(fixture, &fixture.runtime).await;
    assert!(
        proxima_storage_pg::owner_rls_enforced(&runtime_pool, &SCHEMAS)
            .await
            .expect("RLS epoch probe")
    );
    proxima_storage_pg::assert_runtime_rls(&runtime_pool, &SCHEMAS)
        .await
        .expect("granted runtime role passes the runtime RLS guard");
    runtime_pool.close().await;
}

/// Schema-scoped default privileges cover objects the platform creates later.
async fn assert_default_privileges(fixture: &Fixture, platform_pool: &PgPool) {
    exec(
        platform_pool,
        "CREATE TABLE proxima_code.runtime_grants_default_probe (id int); \
         CREATE SEQUENCE proxima_code.runtime_grants_default_seq",
    )
    .await;
    let (admin, runtime) = (&fixture.admin, fixture.runtime.as_str());
    let table = "proxima_code.runtime_grants_default_probe";
    assert!(has(admin, "has_table_privilege", runtime, table, "INSERT").await);
    let sequence = "proxima_code.runtime_grants_default_seq";
    assert!(has(admin, "has_sequence_privilege", runtime, sequence, "USAGE").await);
}

#[tokio::test]
async fn runtime_grants_boot_grants_a_fresh_runtime_role() {
    let fixture = setup().await;
    let owner = company_owner(Uuid::now_v7());
    let platform_pool = role_pool(&fixture, &fixture.platform).await;
    // A platform-owned host table in `public` must stay out of runtime reach.
    exec(
        &platform_pool,
        "CREATE TABLE public.runtime_grants_host_probe (id int)",
    )
    .await;

    // (1) Without the step the fresh runtime role cannot serve. This first
    // boot still migrates everything as the platform role.
    let refused = build(&fixture, owner, false, false)
        .await
        .expect_err("a runtime role holding no grants must not boot");
    // The first read that needs a privilege fails; which one is storage's
    // business (today: the projection census, blind without schema USAGE).
    assert!(matches!(refused, ProximaError::Storage(_)), "{refused}");
    assert!(
        !has(
            &fixture.admin,
            "has_schema_privilege",
            &fixture.runtime,
            "proxima_core",
            "USAGE"
        )
        .await
    );

    // (2) With it, boot succeeds and the runtime serves an owner-scoped
    // write and read under enforced owner RLS.
    let booted = build(&fixture, owner, true, false)
        .await
        .expect("runtime-grants boot");
    assert_runtime_serves(&fixture, &booted, owner).await;
    booted.shutdown();

    // (3) Idempotent: again, and together with skip_migrations.
    build(&fixture, owner, true, false)
        .await
        .expect("second runtime-grants boot")
        .shutdown();
    build(&fixture, owner, true, true)
        .await
        .expect("runtime grants with skip_migrations")
        .shutdown();

    // (4) What runtime holds, and what it never holds.
    assert_runtime_privileges(&fixture).await;
    for privilege in ["SELECT", "INSERT"] {
        assert!(
            !has(
                &fixture.admin,
                "has_table_privilege",
                &fixture.runtime,
                "public.runtime_grants_host_probe",
                privilege
            )
            .await,
            "host table in public must not be granted {privilege}"
        );
    }
    assert_default_privileges(&fixture, &platform_pool).await;
    platform_pool.close().await;
    cleanup(fixture).await;
}

/// (5) Refused before any SQL: the endpoint below is unreachable, so reaching
/// it would surface as a storage error, not a config error.
#[tokio::test]
async fn runtime_grants_refuse_without_split_roles() {
    let unreachable = |user: &str| format!("postgres://{user}:pw@127.0.0.1:1/runtime_grants");
    let owner = company_owner(Uuid::now_v7());
    let boot = |database_url: String, platform_database_url: Option<String>| {
        ProximaBuilder::new(
            EmbedConfig {
                database_url,
                platform_database_url,
                s3: None,
            },
            owner,
        )
        .bundle::<proxima_code::CodeFlavor>()
        .runtime_grants()
        .boot()
    };

    let same = boot(unreachable("rtg_same"), Some(unreachable("rtg_same")))
        .await
        .expect_err("runtime == platform user must be refused");
    assert!(
        matches!(&same, EmbedError::Config(message) if message.contains("split roles")),
        "{same}"
    );
    let missing = boot(unreachable("rtg_runtime"), None)
        .await
        .expect_err("runtime grants need a platform URL");
    assert!(
        matches!(&missing, EmbedError::Config(message)
            if message.contains("PROXIMA_PLATFORM_DATABASE_URL")),
        "{missing}"
    );

    // The runtime facade forwards the flag to the same refusal.
    let facade = Proxima::<CodeGrantsApp>::app()
        .database_url(unreachable("rtg_same"))
        .platform_database_url(unreachable("rtg_same"))
        .owner(owner)
        .tool_scope(ToolScope::All)
        .runtime_grants(true)
        .build()
        .await
        .expect_err("facade refuses runtime grants without split roles");
    assert!(
        matches!(&facade, ProximaError::Config(message) if message.contains("split roles")),
        "{facade}"
    );
}

use async_trait::async_trait;
use proxima_core::{
    AuthPath, Authenticator, AuthzContext, Credentials, GroupId, OwnerRef, OwnerRoles, Role, UserId,
};
use proxima_pg_testkit::{create_db, db_url, drop_db, unique_db_name};
use proxima_storage_pg::{PgPlatformScope, assert_runtime_rls, begin_owner_transaction};
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::borrow::Cow;
use std::str::FromStr;
use std::sync::OnceLock;
use tokio::sync::Mutex;
use tokio::time::{Duration, timeout};

static DDL_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

const STAGED_CORE_OWNER_RLS: &str = include_str!("../migrations/0014_v015_owner_rls.sql");
const STAGED_CODE_OWNER_RLS: &str =
    include_str!("../../../flavors/code/migrations/20260922000020_v015_owner_rls.sql");

struct SyntheticVerifier {
    roles: OwnerRoles,
}

#[path = "rls_guard_pg/memory_paging.rs"]
mod memory_paging;

#[path = "rls_guard_pg/paging_work.rs"]
mod paging_work;

#[path = "rls_guard_pg/trigger_scope.rs"]
mod trigger_scope;

#[async_trait]
impl Authenticator for SyntheticVerifier {
    async fn authenticate(
        &self,
        _credentials: &Credentials,
    ) -> Result<AuthzContext, proxima_core::AuthError> {
        Ok(AuthzContext::server_resolved(
            self.roles.clone(),
            AuthPath::HostBearer,
        ))
    }
}

fn quoted_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

async fn execute(pool: &PgPool, statement: &str) {
    let _lock = DDL_LOCK.get_or_init(|| Mutex::new(())).lock().await;
    // SQL-POLICY: fixed-fragment
    sqlx::query(sqlx::AssertSqlSafe(statement.to_owned()))
        .execute(pool)
        .await
        .unwrap();
}

fn assert_rls_refusal(error: &sqlx::Error) {
    let database = error.as_database_error().expect("database refusal");
    assert!(
        matches!(database.code().as_deref(), Some("42501" | "55000")),
        "{error}"
    );
}

async fn assert_unscoped_owner_access_denied(connection: &mut sqlx::PgConnection) {
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM proxima_core.owners")
        .fetch_one(&mut *connection)
        .await
        .unwrap();
    assert_eq!(count, 0);
    assert_eq!(
        sqlx::query("UPDATE proxima_core.owners SET owner_id = owner_id")
            .execute(&mut *connection)
            .await
            .unwrap()
            .rows_affected(),
        0
    );
    assert_eq!(
        sqlx::query("DELETE FROM proxima_core.owners")
            .execute(&mut *connection)
            .await
            .unwrap()
            .rows_affected(),
        0
    );
    let error = sqlx::query("INSERT INTO proxima_core.owners(owner_id, kind) VALUES ($1, 'group')")
        .bind(uuid::Uuid::now_v7())
        .execute(&mut *connection)
        .await
        .expect_err("missing scope refuses INSERT");
    assert_rls_refusal(&error);
}

async fn runtime_pool(database: &str, role: &str, password: &str) -> PgPool {
    let options = PgConnectOptions::from_str(&db_url(database))
        .unwrap()
        .username(role)
        .password(password);
    PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options)
        .await
        .unwrap()
}

#[expect(
    clippy::too_many_lines,
    reason = "isolated disposable database fixture"
)]
async fn setup() -> (String, PgPool, PgPool, String, String, String, String) {
    let database = unique_db_name("proxima_rls_guard");
    create_db(&database).await.unwrap();
    let admin = PgPool::connect(&db_url(&database)).await.unwrap();
    let suffix = uuid::Uuid::now_v7().simple().to_string();
    let owner = format!("rls_owner_{suffix}");
    let runtime = format!("rls_runtime_{suffix}");
    let password = format!("pw_{suffix}");
    // SQL-POLICY: fixed-fragment
    execute(
        &admin,
        &format!(
            "CREATE ROLE {} LOGIN PASSWORD '{}' NOSUPERUSER NOBYPASSRLS",
            quoted_identifier(&owner),
            password
        ),
    )
    .await;
    // SQL-POLICY: fixed-fragment
    execute(
        &admin,
        &format!(
            "CREATE ROLE {} LOGIN PASSWORD '{}' NOSUPERUSER NOBYPASSRLS",
            quoted_identifier(&runtime),
            password
        ),
    )
    .await;
    let schema = format!("proxima_rls_{suffix}");
    // SQL-POLICY: fixed-fragment
    execute(
        &admin,
        &format!(
            "CREATE SCHEMA {} AUTHORIZATION {}",
            quoted_identifier(&schema),
            quoted_identifier(&owner)
        ),
    )
    .await;
    // SQL-POLICY: fixed-fragment
    execute(
        &admin,
        &format!(
            "GRANT USAGE ON SCHEMA {} TO {}",
            quoted_identifier(&schema),
            quoted_identifier(&runtime)
        ),
    )
    .await;
    for table in ["memory", "sidecar"] {
        execute(
            &admin,
            &format!(
                "CREATE TABLE {}.{} (id uuid PRIMARY KEY, owner_id uuid NOT NULL)",
                quoted_identifier(&schema),
                quoted_identifier(table)
            ),
        )
        .await;
        execute(
            &admin,
            &format!(
                "ALTER TABLE {}.{} OWNER TO {}",
                quoted_identifier(&schema),
                quoted_identifier(table),
                quoted_identifier(&owner)
            ),
        )
        .await;
        execute(
            &admin,
            &format!(
                "GRANT SELECT, INSERT, UPDATE, DELETE ON {}.{} TO {}",
                quoted_identifier(&schema),
                quoted_identifier(table),
                quoted_identifier(&runtime)
            ),
        )
        .await;
        execute(
            &admin,
            &format!(
                "ALTER TABLE {}.{} ENABLE ROW LEVEL SECURITY",
                quoted_identifier(&schema),
                quoted_identifier(table)
            ),
        )
        .await;
        execute(
            &admin,
            &format!(
                "ALTER TABLE {}.{} FORCE ROW LEVEL SECURITY",
                quoted_identifier(&schema),
                quoted_identifier(table)
            ),
        )
        .await;
        execute(&admin, &format!(
            "CREATE POLICY proxima_owner_read ON {}.{} FOR SELECT TO PUBLIC USING (owner_id = ANY(COALESCE(NULLIF(current_setting('app.owner', true), '')::uuid[], '{{}}'::uuid[])))",
            quoted_identifier(&schema), quoted_identifier(table)
        )).await;
        execute(&admin, &format!(
            "CREATE POLICY proxima_owner_write ON {}.{} FOR ALL TO PUBLIC USING (owner_id = ANY(COALESCE(NULLIF(current_setting('app.owner', true), '')::uuid[], '{{}}'::uuid[]))) WITH CHECK (owner_id = ANY(COALESCE(NULLIF(current_setting('app.owner', true), '')::uuid[], '{{}}'::uuid[])))",
            quoted_identifier(&schema), quoted_identifier(table)
        )).await;
        execute(&admin, &format!(
            "CREATE POLICY proxima_platform ON {}.{} FOR ALL TO {} USING (current_setting('app.proxima_scope', true) = 'platform') WITH CHECK (current_setting('app.proxima_scope', true) = 'platform')",
            quoted_identifier(&schema), quoted_identifier(table), quoted_identifier(&owner)
        )).await;
    }
    let runtime_pool = runtime_pool(&database, &runtime, &password).await;
    (
        database,
        admin,
        runtime_pool,
        owner,
        runtime,
        schema,
        password,
    )
}

async fn apply_owner_rls_fixtures(pool: &PgPool) {
    let mut connection = pool.acquire().await.unwrap().detach();
    let mut transaction = proxima_storage_pg::begin_migration_transaction(&mut connection)
        .await
        .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(STAGED_CORE_OWNER_RLS.to_owned()))
        .execute(transaction.as_mut())
        .await
        .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(STAGED_CODE_OWNER_RLS.to_owned()))
        .execute(transaction.as_mut())
        .await
        .unwrap();
    transaction.commit().await.unwrap();
}

#[expect(
    clippy::uninlined_format_args,
    clippy::too_many_lines,
    reason = "full migrated RLS fixture owns all setup steps"
)]
async fn setup_full_schema() -> (String, PgPool, PgPool, PgPool, String, String, String) {
    let database = unique_db_name("proxima_rls_full");
    create_db(&database).await.unwrap();
    let admin = PgPool::connect(&db_url(&database)).await.unwrap();
    proxima_storage_pg::test_fixtures::core_migrator_before_owner_rls()
        .run(&admin)
        .await
        .unwrap();
    let mut code = sqlx::migrate!("../../flavors/code/migrations");
    code.migrations = Cow::Owned(
        code.iter()
            .filter(|migration| !migration.description.contains("owner rls"))
            .cloned()
            .collect(),
    );
    code.dangerous_set_table_name("public._sqlx_migrations_proxima_code");
    code.run(&admin).await.unwrap();
    let suffix = uuid::Uuid::now_v7().simple().to_string();
    let platform = format!("rls_platform_{suffix}");
    let runtime = format!("rls_runtime_full_{suffix}");
    let password = format!("pw_{suffix}");
    for role in [&platform, &runtime] {
        execute(
            &admin,
            &format!(
                "CREATE ROLE {} LOGIN PASSWORD '{}' NOSUPERUSER NOBYPASSRLS",
                quoted_identifier(role),
                password
            ),
        )
        .await;
    }
    execute(
        &admin,
        &format!(
            "DO $$ DECLARE f record; BEGIN FOR f IN SELECT n.nspname, p.proname, pg_get_function_identity_arguments(p.oid) AS args FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace WHERE n.nspname IN ('proxima_core','proxima_code') LOOP EXECUTE format('ALTER FUNCTION %I.%I(%s) OWNER TO %I', f.nspname, f.proname, f.args, '{}'); END LOOP; END $$",
            platform
        ),
    )
    .await;
    execute(
        &admin,
        &format!(
            "DO $$ DECLARE r record; BEGIN FOR r IN SELECT n.nspname, c.relname FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname IN ('proxima_core','proxima_code') AND c.relkind IN ('r','p') LOOP EXECUTE format('ALTER TABLE %I.%I OWNER TO %I', r.nspname, r.relname, '{}'); END LOOP; END $$",
            platform
        ),
    )
    .await;
    execute(
        &admin,
        &format!(
            "GRANT USAGE ON SCHEMA proxima_core, proxima_code TO {}, {}",
            quoted_identifier(&runtime),
            quoted_identifier(&platform)
        ),
    )
    .await;
    execute(
        &admin,
        &format!(
            "GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA proxima_core, proxima_code TO {}",
            quoted_identifier(&runtime)
        ),
    )
    .await;
    execute(
        &admin,
        &format!(
            "ALTER SCHEMA proxima_core OWNER TO {}",
            quoted_identifier(&platform)
        ),
    )
    .await;
    execute(
        &admin,
        &format!(
            "ALTER SCHEMA proxima_code OWNER TO {}",
            quoted_identifier(&platform)
        ),
    )
    .await;
    let platform_connection = runtime_pool(&database, &platform, &password).await;
    let scope_grant: bool =
        sqlx::query_scalar("SELECT has_parameter_privilege($1, 'app.proxima_scope', 'SET')")
            .bind(&platform)
            .fetch_one(&admin)
            .await
            .unwrap();
    assert!(
        !scope_grant,
        "owner RLS must apply without a parameter grant"
    );
    apply_owner_rls_fixtures(&platform_connection).await;
    platform_connection.close().await;
    let platform_connection = runtime_pool(&database, &platform, &password).await;
    let runtime_connection = runtime_pool(&database, &runtime, &password).await;
    (
        database,
        admin,
        runtime_connection,
        platform_connection,
        platform,
        runtime,
        password,
    )
}

async fn cleanup(database: &str, admin: PgPool, owner: &str, runtime: &str) {
    admin.close().await;
    drop_db(database).await.unwrap();
    let control = PgPool::connect(&proxima_pg_testkit::admin_url())
        .await
        .unwrap();
    execute(
        &control,
        // SQL-POLICY: fixed-fragment
        &format!("DROP ROLE IF EXISTS {}", quoted_identifier(runtime)),
    )
    .await;
    execute(
        &control,
        // SQL-POLICY: fixed-fragment
        &format!("DROP ROLE IF EXISTS {}", quoted_identifier(owner)),
    )
    .await;
    control.close().await;
}

async fn restore_owner_read_policy(admin: &PgPool, schema: &str, table: &str) {
    execute(
        admin,
        &format!(
            "CREATE POLICY proxima_owner_read ON {}.{} FOR SELECT TO PUBLIC USING (owner_id = ANY(COALESCE(NULLIF(current_setting('app.owner', true), '')::uuid[], '{{}}'::uuid[])))",
            quoted_identifier(schema), quoted_identifier(table)
        ),
    )
    .await;
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "structural negative controls share one disposable fixture"
)]
async fn runtime_guard_accepts_complete_schema_and_refuses_structural_breaks() {
    let (database, admin, runtime, owner, runtime_role, schema, password) = setup().await;
    assert_runtime_rls(&runtime, &[&schema]).await.unwrap();

    let owner_pool = runtime_pool(&database, &owner, &password).await;
    assert!(assert_runtime_rls(&owner_pool, &[&schema]).await.is_err());
    owner_pool.close().await;
    execute(
        &admin,
        &format!(
            "GRANT {} TO {}",
            quoted_identifier(&owner),
            quoted_identifier(&runtime_role)
        ),
    )
    .await;
    assert!(assert_runtime_rls(&runtime, &[&schema]).await.is_err());
    execute(
        &admin,
        &format!(
            "REVOKE {} FROM {}",
            quoted_identifier(&owner),
            quoted_identifier(&runtime_role)
        ),
    )
    .await;
    execute(
        &admin,
        // SQL-POLICY: fixed-fragment
        &format!("ALTER ROLE {} BYPASSRLS", quoted_identifier(&runtime_role)),
    )
    .await;
    assert!(assert_runtime_rls(&runtime, &[&schema]).await.is_err());
    execute(
        &admin,
        &format!(
            "ALTER ROLE {} NOBYPASSRLS",
            quoted_identifier(&runtime_role)
        ),
    )
    .await;
    execute(
        &admin,
        // SQL-POLICY: fixed-fragment
        &format!("ALTER ROLE {} SUPERUSER", quoted_identifier(&runtime_role)),
    )
    .await;
    assert!(assert_runtime_rls(&runtime, &[&schema]).await.is_err());
    execute(
        &admin,
        &format!(
            "ALTER ROLE {} NOSUPERUSER",
            quoted_identifier(&runtime_role)
        ),
    )
    .await;

    // SQL-POLICY: fixed-fragment
    let count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT count(*) FROM {}.memory",
        quoted_identifier(&schema)
    )))
    .fetch_one(&runtime)
    .await
    .unwrap();
    assert_eq!(count, 0);
    // SQL-POLICY: fixed-fragment
    let missing_scope = sqlx::query(sqlx::AssertSqlSafe(format!("INSERT INTO {}.memory (id, owner_id) VALUES ('00000000-0000-0000-0000-000000000001', '00000000-0000-0000-0000-000000000002')", quoted_identifier(&schema))))
        .execute(&runtime).await;
    assert_rls_refusal(&missing_scope.expect_err("missing app.owner must refuse writes"));

    execute(
        &admin,
        &format!(
            "DROP POLICY proxima_owner_read ON {}.memory",
            quoted_identifier(&schema)
        ),
    )
    .await;
    assert!(assert_runtime_rls(&runtime, &[&schema]).await.is_err());
    restore_owner_read_policy(&admin, &schema, "memory").await;

    execute(
        &admin,
        &format!(
            "ALTER TABLE {}.sidecar NO FORCE ROW LEVEL SECURITY",
            quoted_identifier(&schema)
        ),
    )
    .await;
    assert!(assert_runtime_rls(&runtime, &[&schema]).await.is_err());
    execute(
        &admin,
        &format!(
            "ALTER TABLE {}.sidecar FORCE ROW LEVEL SECURITY",
            quoted_identifier(&schema)
        ),
    )
    .await;
    execute(
        &admin,
        &format!(
            "CREATE TABLE {}.unprotected (id int)",
            quoted_identifier(&schema)
        ),
    )
    .await;
    assert!(assert_runtime_rls(&runtime, &[&schema]).await.is_err());

    let mut transaction = runtime.begin().await.unwrap();
    sqlx::query("SET LOCAL row_security = off")
        .execute(&mut *transaction)
        .await
        .unwrap();
    // SQL-POLICY: fixed-fragment
    let bypass = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT count(*) FROM {}.memory",
        quoted_identifier(&schema)
    )))
    .execute(&mut *transaction)
    .await;
    assert_rls_refusal(&bypass.expect_err("forced RLS refuses runtime row_security bypass"));
    transaction.rollback().await.unwrap();
    runtime.close().await;
    cleanup(&database, admin, &owner, &runtime_role).await;
}

#[tokio::test]
async fn runtime_guard_rejects_empty_and_unknown_schema_inventory() {
    let (database, admin, runtime, owner, runtime_role, schema, _password) = setup().await;
    assert!(assert_runtime_rls(&runtime, &[]).await.is_err());
    assert!(
        assert_runtime_rls(&runtime, &["does_not_exist"])
            .await
            .is_err()
    );
    runtime.close().await;
    cleanup(&database, admin, &owner, &runtime_role).await;
    let _ = schema;
}

#[tokio::test]
async fn platform_scope_rejects_missing_force_and_policy() {
    let (database, admin, runtime, owner, runtime_role, schema, password) = setup().await;
    let platform = runtime_pool(&database, &owner, &password).await;
    PgPlatformScope::new(platform.clone(), &[&schema])
        .await
        .expect("complete owner-backed platform contract must be admitted");

    execute(
        &admin,
        &format!(
            "ALTER TABLE {}.memory NO FORCE ROW LEVEL SECURITY",
            quoted_identifier(&schema)
        ),
    )
    .await;
    assert!(
        PgPlatformScope::new(platform.clone(), &[&schema])
            .await
            .is_err(),
        "owner-backed platform scope must require FORCE RLS"
    );
    execute(
        &admin,
        &format!(
            "ALTER TABLE {}.memory FORCE ROW LEVEL SECURITY",
            quoted_identifier(&schema)
        ),
    )
    .await;

    execute(
        &admin,
        &format!(
            "DROP POLICY proxima_platform ON {}.memory",
            quoted_identifier(&schema)
        ),
    )
    .await;
    assert!(
        PgPlatformScope::new(platform.clone(), &[&schema])
            .await
            .is_err(),
        "platform scope must require the complete policy contract"
    );

    execute(
        &admin,
        &format!(
            "CREATE POLICY proxima_platform ON {}.memory FOR ALL TO {} USING (current_setting('app.proxima_scope', true) = 'platform') WITH CHECK (current_setting('app.proxima_scope', true) = 'platform')",
            quoted_identifier(&schema),
            quoted_identifier(&owner)
        ),
    )
    .await;
    PgPlatformScope::new(platform.clone(), &[&schema])
        .await
        .expect("repaired owner-backed platform contract must be admitted");

    platform.close().await;
    runtime.close().await;
    cleanup(&database, admin, &owner, &runtime_role).await;
}

#[tokio::test]
async fn information_schema_census_matches_catalog_guard_contract() {
    let (database, admin, runtime, platform, owner, runtime_role, _password) =
        setup_full_schema().await;
    let schemas = ["proxima_core", "proxima_code"];
    // The privileged inventory cannot omit a table because the runtime lacks a grant.
    let tables: Vec<(String, String)> = sqlx::query_as(
        "SELECT table_schema, table_name FROM information_schema.tables
         WHERE table_schema = ANY($1) AND table_type = 'BASE TABLE'",
    )
    .bind(&schemas[..])
    .fetch_all(&admin)
    .await
    .unwrap();
    assert!(
        tables
            .iter()
            .any(|(s, t)| s == "proxima_core" && t == "memory")
    );
    assert!(
        tables
            .iter()
            .any(|(s, t)| s == "proxima_code" && t == "code_chunk_v1")
    );
    let uncovered: Vec<String> = sqlx::query_scalar(
        "SELECT t.table_schema || '.' || t.table_name
           FROM information_schema.tables t
           JOIN pg_namespace n ON n.nspname = t.table_schema
           JOIN pg_class c ON c.relnamespace = n.oid AND c.relname = t.table_name
          WHERE t.table_schema = ANY($1) AND t.table_type = 'BASE TABLE'
            AND (NOT c.relrowsecurity OR NOT c.relforcerowsecurity
              OR (SELECT count(*) FROM pg_policy p WHERE p.polrelid = c.oid) <> 3
              OR NOT EXISTS (SELECT 1 FROM pg_policy p WHERE p.polrelid = c.oid
                 AND p.polname = 'proxima_owner_read' AND p.polcmd = 'r')
              OR NOT EXISTS (SELECT 1 FROM pg_policy p WHERE p.polrelid = c.oid
                 AND p.polname = 'proxima_owner_write' AND p.polcmd = '*')
              OR NOT EXISTS (SELECT 1 FROM pg_policy p WHERE p.polrelid = c.oid
                 AND p.polname = 'proxima_platform' AND p.polroles = ARRAY[c.relowner]))",
    )
    .bind(&schemas[..])
    .fetch_all(&admin)
    .await
    .unwrap();
    assert!(uncovered.is_empty(), "unprotected tables: {uncovered:?}");
    assert_runtime_rls(&runtime, &schemas).await.unwrap();
    runtime.close().await;
    platform.close().await;
    cleanup(&database, admin, &owner, &runtime_role).await;
}

#[tokio::test]
async fn authenticated_witness_binds_and_clears_transaction_scope() {
    let (database, admin, runtime, owner, runtime_role, schema, password) = setup().await;
    runtime.close().await;
    let runtime = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(
            PgConnectOptions::from_str(&db_url(&database))
                .unwrap()
                .username(&runtime_role)
                .password(&password),
        )
        .await
        .unwrap();
    let subject = UserId::new(uuid::Uuid::now_v7());
    let verifier = SyntheticVerifier {
        roles: OwnerRoles::for_subject(subject, []).expect("synthetic roles"),
    };
    let authz = proxima_core::authenticate(
        &verifier,
        &Credentials::Bearer("synthetic-test-credential".into()),
    )
    .await
    .unwrap();
    let scope = authz.owner_scope().expect("authenticated witness");

    let mut transaction = begin_owner_transaction(&runtime, scope).await.unwrap();
    let (scope_name, owner_setting): (String, String) =
        sqlx::query_as("SELECT current_setting('app.proxima_scope'), current_setting('app.owner')")
            .fetch_one(&mut *transaction)
            .await
            .unwrap();
    assert_eq!(scope_name, "owner");
    assert!(owner_setting.contains(&subject.into_inner().to_string()));
    transaction.rollback().await.unwrap();

    let mut next = runtime.begin().await.unwrap();
    let scope_after_rollback: Option<String> =
        sqlx::query_scalar("SELECT current_setting('app.proxima_scope', true)")
            .fetch_one(&mut *next)
            .await
            .unwrap();
    assert!(scope_after_rollback.is_none_or(|scope| scope.is_empty()));
    next.rollback().await.unwrap();
    let committed = begin_owner_transaction(&runtime, scope).await.unwrap();
    committed.commit().await.unwrap();
    let mut after_commit = runtime.begin().await.unwrap();
    let committed_scope: Option<String> =
        sqlx::query_scalar("SELECT current_setting('app.proxima_scope', true)")
            .fetch_one(&mut *after_commit)
            .await
            .unwrap();
    assert!(committed_scope.is_none_or(|scope| scope.is_empty()));
    after_commit.rollback().await.unwrap();

    let mut errored = begin_owner_transaction(&runtime, scope).await.unwrap();
    assert!(
        sqlx::query("SELECT * FROM definitely_missing_owner_rls_table")
            .fetch_one(&mut *errored)
            .await
            .is_err()
    );
    errored.rollback().await.unwrap();
    let mut after_error = runtime.begin().await.unwrap();
    let error_scope: Option<String> =
        sqlx::query_scalar("SELECT current_setting('app.proxima_scope', true)")
            .fetch_one(&mut *after_error)
            .await
            .unwrap();
    assert!(error_scope.is_none_or(|scope| scope.is_empty()));
    after_error.rollback().await.unwrap();

    let mut cancelled = begin_owner_transaction(&runtime, scope).await.unwrap();
    let cancellation = timeout(
        Duration::from_millis(1),
        sqlx::query("SELECT pg_sleep(1)").execute(&mut *cancelled),
    )
    .await;
    assert!(
        cancellation.is_err(),
        "the scoped statement must be cancellable"
    );
    drop(cancelled);
    let mut after_cancel = runtime.begin().await.unwrap();
    let cancelled_scope: Option<String> =
        sqlx::query_scalar("SELECT current_setting('app.proxima_scope', true)")
            .fetch_one(&mut *after_cancel)
            .await
            .unwrap();
    assert!(cancelled_scope.is_none_or(|scope| scope.is_empty()));
    after_cancel.rollback().await.unwrap();
    runtime.close().await;
    cleanup(&database, admin, &owner, &runtime_role).await;
    let _ = schema;
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    clippy::uninlined_format_args,
    clippy::similar_names,
    clippy::cast_possible_wrap,
    reason = "single end-to-end RLS acceptance scenario"
)]
async fn full_policy_isolation_uses_authenticated_scope_and_platform_role() {
    let (database, admin, runtime, platform, platform_role, runtime_role, password) =
        setup_full_schema().await;
    assert_runtime_rls(&runtime, &["proxima_core", "proxima_code"])
        .await
        .unwrap();

    let subject = UserId::new(uuid::Uuid::now_v7());
    let owner_a = uuid::Uuid::now_v7();
    let owner_b = uuid::Uuid::now_v7();
    let group_a = GroupId::new(owner_a);
    let group_b = GroupId::new(owner_b);
    execute(
        &admin,
        &format!(
            "INSERT INTO proxima_core.owners(owner_id, kind) VALUES ('{}','group'),('{}','group')",
            owner_a, owner_b
        ),
    )
    .await;
    let mut owner_a_t = uuid::Uuid::nil();
    let mut owner_b_t = uuid::Uuid::nil();
    for (owner, handle, t, title) in [
        (owner_a, uuid::Uuid::now_v7(), uuid::Uuid::now_v7(), "A"),
        (owner_b, uuid::Uuid::now_v7(), uuid::Uuid::now_v7(), "B"),
    ] {
        if owner == owner_a {
            owner_a_t = t;
        } else {
            owner_b_t = t;
        }
        execute(
            &admin,
            &format!(
                "INSERT INTO proxima_core.memory_head(handle,schema_id,kind,owner_id,t) VALUES ('{}','core/test','fact','{}','{}')",
                handle, owner, t
            ),
        )
        .await;
        let mut transaction = admin.begin().await.unwrap();
        sqlx::query("INSERT INTO proxima_core.memory(handle,t,kind,owner_id,schema_id,sidecar_tables) VALUES ($1,$2,'fact',$3,'core/test',ARRAY['proxima_core.agent_note_v1'])")
            .bind(handle).bind(t).bind(owner).execute(&mut *transaction).await.unwrap();
        sqlx::query(
            "INSERT INTO proxima_core.agent_note_v1(t,note_id,title,body) VALUES ($1,$2,$3,$3)",
        )
        .bind(t)
        .bind(owner_a_t)
        .bind(title)
        .execute(&mut *transaction)
        .await
        .unwrap();
        transaction.commit().await.unwrap();
    }

    let verifier = SyntheticVerifier {
        roles: OwnerRoles::for_subject(
            subject,
            [
                (OwnerRef::Group(group_a), Role::editor()),
                (OwnerRef::Group(group_b), Role::viewer()),
            ],
        )
        .unwrap(),
    };
    let authz = proxima_core::authenticate(&verifier, &Credentials::Bearer("synthetic".into()))
        .await
        .unwrap();
    let narrowed = authz.narrowed_to_owner(OwnerRef::Group(group_a)).unwrap();
    let scope = narrowed.owner_scope().unwrap();
    let mut transaction = begin_owner_transaction(&runtime, scope).await.unwrap();
    let visible_sidecars: i64 =
        sqlx::query_scalar("SELECT count(*) FROM proxima_core.agent_note_v1")
            .fetch_one(&mut *transaction)
            .await
            .unwrap();
    assert_eq!(
        visible_sidecars, 1,
        "foreign sidecar must be redacted by parent ownership"
    );
    transaction.rollback().await.unwrap();

    let full_scope_verifier = SyntheticVerifier {
        roles: OwnerRoles::for_subject(
            subject,
            [
                (OwnerRef::Group(group_a), Role::editor()),
                (OwnerRef::Group(group_b), Role::editor()),
            ],
        )
        .unwrap(),
    };
    let full_scope_authz = proxima_core::authenticate(
        &full_scope_verifier,
        &Credentials::Bearer("cross-owner-pin".into()),
    )
    .await
    .unwrap();
    let mut cross_owner_tx =
        begin_owner_transaction(&runtime, full_scope_authz.owner_scope().unwrap())
            .await
            .unwrap();
    let derived_handle = uuid::Uuid::now_v7();
    let derived_t = uuid::Uuid::now_v7();
    let derived_content = uuid::Uuid::now_v7();
    sqlx::query("INSERT INTO proxima_core.content(content_id,owner_id,schema_id,content_hash) VALUES ($1,$2,'core/test',decode(repeat('00',32),'hex'))")
        .bind(derived_content).bind(owner_a)
        .execute(&mut *cross_owner_tx).await.unwrap();
    sqlx::query("INSERT INTO proxima_core.memory_head(handle,schema_id,kind,owner_id,t) VALUES ($1,'core/test','abstraction',$2,$3)")
        .bind(derived_handle).bind(owner_a).bind(derived_t)
        .execute(&mut *cross_owner_tx).await.unwrap();
    sqlx::query("INSERT INTO proxima_core.memory(handle,t,kind,owner_id,schema_id,content_id,origins) VALUES ($1,$2,'abstraction',$3,'core/test',$4,ARRAY[$5]::uuid[])")
        .bind(derived_handle).bind(derived_t).bind(owner_a).bind(derived_content).bind(owner_b_t)
        .execute(&mut *cross_owner_tx).await.unwrap();
    let restored_scope: String =
        sqlx::query_scalar("SELECT current_setting('app.proxima_scope', true)")
            .fetch_one(&mut *cross_owner_tx)
            .await
            .unwrap();
    assert_eq!(
        restored_scope, "owner",
        "platform trigger bridge must restore owner scope"
    );
    cross_owner_tx.commit().await.unwrap();

    let expected_trigger_functions = [
        "assert_erased_pin_target_insert",
        "memory_erase_witness",
        "cooled_erase_witness",
        "goal_erase_witness",
        "cooled_identity_seal",
        "goal_pin_target_checks",
        "wake_pin_target_checks",
        "memory_pin_checks",
        "cooled_forget_grounding",
        "pins_have_grounding_support",
    ];
    let installed: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
          WHERE n.nspname = 'proxima_core' AND p.proname = ANY($1::text[])",
    )
    .bind(expected_trigger_functions.to_vec())
    .fetch_one(&admin)
    .await
    .unwrap();
    assert_eq!(installed, expected_trigger_functions.len() as i64);
    let unsafe_definers: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace JOIN pg_roles r ON r.oid = p.proowner WHERE n.nspname='proxima_core' AND p.proname = ANY($1::text[]) AND (r.rolsuper OR r.rolbypassrls)",
    )
    .bind(expected_trigger_functions.to_vec())
    .fetch_one(&admin)
    .await
    .unwrap();
    assert_eq!(
        unsafe_definers, 0,
        "RLS trigger definers must be non-superuser/non-BYPASSRLS"
    );

    let new_handle = uuid::Uuid::now_v7();
    let new_t = uuid::Uuid::now_v7();
    let mut writer = begin_owner_transaction(&runtime, scope).await.unwrap();
    sqlx::query("INSERT INTO proxima_core.memory_head(handle,schema_id,kind,owner_id,t) VALUES ($1,'core/agent-note-v1','fact',$2,$3)")
        .bind(new_handle).bind(owner_a).bind(new_t)
        .execute(&mut *writer).await.unwrap();
    sqlx::query("INSERT INTO proxima_core.memory(handle,t,kind,owner_id,schema_id,sidecar_tables) VALUES ($1,$2,'fact',$3,'core/agent-note-v1',ARRAY['proxima_core.agent_note_v1'])")
        .bind(new_handle).bind(new_t).bind(owner_a)
        .execute(&mut *writer).await.unwrap();
    sqlx::query("INSERT INTO proxima_core.agent_note_v1(t,note_id,title,body) VALUES ($1,$2,'runtime','runtime')")
        .bind(new_t).bind(uuid::Uuid::now_v7())
        .execute(&mut *writer).await.unwrap();
    writer.commit().await.unwrap();
    let mut verify_writer = begin_owner_transaction(&runtime, scope).await.unwrap();
    let own_sidecars: i64 = sqlx::query_scalar("SELECT count(*) FROM proxima_core.agent_note_v1")
        .fetch_one(&mut *verify_writer)
        .await
        .unwrap();
    assert_eq!(own_sidecars, 2);
    verify_writer.rollback().await.unwrap();

    let viewer = SyntheticVerifier {
        roles: OwnerRoles::for_subject(subject, [(OwnerRef::Group(group_a), Role::viewer())])
            .unwrap(),
    };
    let viewer_authz = proxima_core::authenticate(&viewer, &Credentials::Bearer("viewer".into()))
        .await
        .unwrap();
    let mut viewer_tx = begin_owner_transaction(&runtime, viewer_authz.owner_scope().unwrap())
        .await
        .unwrap();
    let denied = sqlx::query("INSERT INTO proxima_core.agent_note_v1(t,note_id,title,body) VALUES ($1,$2,'viewer','viewer')")
        .bind(owner_a_t)
        .bind(uuid::Uuid::now_v7())
        .execute(&mut *viewer_tx)
        .await;
    assert_rls_refusal(&denied.expect_err("viewer write ceiling must refuse INSERT"));
    viewer_tx.rollback().await.unwrap();

    let mixed = SyntheticVerifier {
        roles: OwnerRoles::for_subject(
            subject,
            [
                (OwnerRef::Group(group_a), Role::viewer()),
                (OwnerRef::Group(group_b), Role::editor()),
            ],
        )
        .unwrap(),
    };
    let mixed_authz = proxima_core::authenticate(&mixed, &Credentials::Bearer("mixed".into()))
        .await
        .unwrap();
    let mut mixed_tx = begin_owner_transaction(&runtime, mixed_authz.owner_scope().unwrap())
        .await
        .unwrap();
    let cross_owner_write = sqlx::query("INSERT INTO proxima_core.agent_note_v1(t,note_id,title,body) VALUES ($1,$2,'mixed','mixed')")
        .bind(owner_a_t)
        .bind(uuid::Uuid::now_v7())
        .execute(&mut *mixed_tx)
        .await;
    assert_rls_refusal(&cross_owner_write.expect_err("writable B must not authorize sidecar A"));
    mixed_tx.rollback().await.unwrap();

    let mut unscoped_platform = platform.begin().await.unwrap();
    assert_unscoped_owner_access_denied(&mut unscoped_platform).await;
    unscoped_platform.rollback().await.unwrap();

    let mut forged_platform = runtime.begin().await.unwrap();
    sqlx::query("SET LOCAL app.proxima_scope = 'platform'")
        .execute(&mut *forged_platform)
        .await
        .unwrap();
    assert_unscoped_owner_access_denied(&mut forged_platform).await;
    forged_platform.rollback().await.unwrap();

    let mut foreign_insert = begin_owner_transaction(&runtime, scope).await.unwrap();
    let foreign_t = uuid::Uuid::now_v7();
    let error = sqlx::query("INSERT INTO proxima_core.memory_head(handle,schema_id,kind,owner_id,t) VALUES ($1,'core/agent-note-v1','fact',$2,$1)")
        .bind(foreign_t).bind(owner_b).execute(&mut *foreign_insert).await
        .expect_err("owner A cannot insert owner B's head");
    assert_rls_refusal(&error);
    foreign_insert.rollback().await.unwrap();

    let mut platform_tx = platform.begin().await.unwrap();
    sqlx::query("SET LOCAL app.proxima_scope = 'platform'")
        .execute(&mut *platform_tx)
        .await
        .unwrap();
    let platform_scope: i64 = sqlx::query_scalar("SELECT count(*) FROM proxima_core.owners")
        .fetch_one(&mut *platform_tx)
        .await
        .unwrap();
    assert_eq!(platform_scope, 2);
    platform_tx.rollback().await.unwrap();

    let mut empty_scope_tx = runtime.begin().await.unwrap();
    sqlx::query("SET LOCAL app.proxima_scope = 'owner'")
        .execute(&mut *empty_scope_tx)
        .await
        .unwrap();
    sqlx::query("SET LOCAL app.owner = '{}'")
        .execute(&mut *empty_scope_tx)
        .await
        .unwrap();
    let metadata_rows: i64 = sqlx::query_scalar("SELECT count(*) FROM proxima_core.flavor_surface")
        .fetch_one(&mut *empty_scope_tx)
        .await
        .unwrap();
    assert_eq!(metadata_rows, 0, "empty owner array must hide metadata too");
    empty_scope_tx.rollback().await.unwrap();

    let mut bypass_tx = runtime.begin().await.unwrap();
    sqlx::query("SET LOCAL row_security = off")
        .execute(&mut *bypass_tx)
        .await
        .unwrap();
    assert_rls_refusal(
        &sqlx::query("SELECT count(*) FROM proxima_core.owners")
            .execute(&mut *bypass_tx)
            .await
            .expect_err("runtime cannot disable row security"),
    );
    bypass_tx.rollback().await.unwrap();

    runtime.close().await;
    platform.close().await;
    cleanup(&database, admin, &platform_role, &runtime_role).await;
    let _ = password;
}

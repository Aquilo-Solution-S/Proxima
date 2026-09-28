use proxima_pg_testkit::{
    DbGuard, SplitRoleDb, create_db, db_url, split_role_urls_for, unique_db_name,
};
use sqlx::{AssertSqlSafe, Connection, PgConnection};

async fn admin_database() -> (DbGuard, PgConnection) {
    let name = unique_db_name("proxima_test_split_roles");
    create_db(&name).await.expect("create cutover database");
    let guard = DbGuard::adopt(name);
    let admin = PgConnection::connect(&db_url(guard.name()))
        .await
        .expect("connect as admin");
    (guard, admin)
}

async fn assert_ledger_access(
    ledger: &str,
    runtime: &mut PgConnection,
    platform: &mut PgConnection,
) {
    let platform_owns: bool = sqlx::query_scalar(
        "SELECT pg_get_userbyid(relowner) = current_user
           FROM pg_class WHERE oid = $1::regclass",
    )
    .bind(ledger)
    .fetch_one(&mut *platform)
    .await
    .expect("ledger ownership");
    assert!(platform_owns, "platform must own {ledger}");

    let preserved = format!(
        "SELECT version = 17 AND description = 'before split'
                AND installed_on = '2020-01-01 00:00:00+00'::timestamptz
                AND success AND checksum = decode('abcd', 'hex')
                AND execution_time = 42 FROM {ledger}"
    );
    // SQL-POLICY: fixed-fragment — ledger is one of the closed, quoted fixture table names below.
    let rows: Vec<bool> = sqlx::query_scalar(AssertSqlSafe(preserved))
        .fetch_all(&mut *runtime)
        .await
        .expect("runtime reads existing ledger");
    assert_eq!(rows, [true], "cutover must preserve {ledger}");

    let insert = format!(
        "INSERT INTO {ledger} (version, description, success, checksum, execution_time)
         VALUES (18, 'after split', true, decode('cdef', 'hex'), 43)"
    );
    // SQL-POLICY: fixed-fragment — ledger is one of the closed, quoted fixture table names below.
    sqlx::query(AssertSqlSafe(insert))
        .execute(&mut *platform)
        .await
        .expect("platform records the next migration");

    // SQL-POLICY: fixed-fragment — closed DML forms and quoted fixture table names only.
    for statement in [
        format!("INSERT INTO {ledger} SELECT * FROM {ledger} WHERE false"),
        format!("UPDATE {ledger} SET description = 'runtime mutation'"),
        format!("DELETE FROM {ledger}"),
        format!("TRUNCATE {ledger}"),
    ] {
        // SQL-POLICY: fixed-fragment — closed DML forms and quoted fixture table names only.
        let error = sqlx::query(AssertSqlSafe(statement))
            .execute(&mut *runtime)
            .await
            .expect_err("runtime ledger writes must be refused");
        assert_eq!(
            error
                .as_database_error()
                .and_then(sqlx::error::DatabaseError::code),
            Some("42501".into()),
            "runtime must have read-only access to {ledger}: {error}"
        );
    }
    let count = format!("SELECT count(*) FROM {ledger}");
    // SQL-POLICY: fixed-fragment — ledger is one of the closed, quoted fixture table names below.
    let count: i64 = sqlx::query_scalar(AssertSqlSafe(count))
        .fetch_one(&mut *runtime)
        .await
        .expect("runtime reads both migrations");
    assert_eq!(count, 2, "runtime writes must not alter {ledger}");
}

#[tokio::test]
async fn split_roles_transfer_all_existing_public_migration_ledgers() {
    // Establish the shared fixture roles before seeding the old runtime grants.
    let _roles = SplitRoleDb::create("proxima_test_split_role_seed", &[])
        .await
        .expect("provision reusable fixture roles");
    let (guard, mut admin) = admin_database().await;
    sqlx::raw_sql(
        r#"CREATE TABLE public._sqlx_migrations_custom (
               version bigint PRIMARY KEY, description text NOT NULL,
               installed_on timestamptz NOT NULL DEFAULT now(), success boolean NOT NULL,
               checksum bytea NOT NULL, execution_time bigint NOT NULL
           );
           INSERT INTO public._sqlx_migrations_custom
               VALUES (17, 'before split', '2020-01-01 00:00:00+00', true, decode('abcd', 'hex'), 42);
           CREATE TABLE public."_sqlx_migrations_custom""quoted"
               (LIKE public._sqlx_migrations_custom INCLUDING ALL);
           INSERT INTO public."_sqlx_migrations_custom""quoted"
               SELECT * FROM public._sqlx_migrations_custom;
           CREATE TABLE public._sqlx_migrations
               (LIKE public._sqlx_migrations_custom INCLUDING ALL);
           INSERT INTO public._sqlx_migrations SELECT * FROM public._sqlx_migrations_custom;
           CREATE TABLE public._sqlx_migrations_proxima_code
               (LIKE public._sqlx_migrations_custom INCLUDING ALL);
           INSERT INTO public._sqlx_migrations_proxima_code SELECT * FROM public._sqlx_migrations_custom;
           GRANT SELECT, INSERT, UPDATE, DELETE, TRUNCATE ON
               public._sqlx_migrations_custom, public."_sqlx_migrations_custom""quoted",
               public._sqlx_migrations, public._sqlx_migrations_proxima_code
               TO proxima_test_runtime"#,
    )
    .execute(&mut admin)
    .await
    .expect("seed admin-owned ledgers with existing runtime grants");
    admin.close().await.expect("close admin");

    let (runtime_url, platform_url) = split_role_urls_for(guard.name(), &[])
        .await
        .expect("cut over existing flavor ledgers");
    let mut runtime = PgConnection::connect(&runtime_url).await.expect("runtime");
    let mut platform = PgConnection::connect(&platform_url)
        .await
        .expect("platform");
    for ledger in [
        "public._sqlx_migrations",
        "public._sqlx_migrations_proxima_code",
        "public._sqlx_migrations_custom",
        r#"public."_sqlx_migrations_custom""quoted""#,
    ] {
        assert_ledger_access(ledger, &mut runtime, &mut platform).await;
    }
    runtime.close().await.expect("close runtime");
    platform.close().await.expect("close platform");
}

#[tokio::test]
async fn split_roles_only_transfer_public_tables_with_the_literal_ledger_prefix() {
    let (guard, mut admin) = admin_database().await;
    sqlx::raw_sql(
        "CREATE TABLE public.xsqlx_migrations_lookalike (id integer);
         CREATE TABLE public._sqlxXmigrations_lookalike (id integer);
         CREATE TABLE public._sqlx_migration (id integer);
         CREATE VIEW public._sqlx_migrations_view AS SELECT 1 AS id;
         CREATE SEQUENCE public._sqlx_migrations_sequence;
         CREATE SCHEMA other_flavor;
         CREATE TABLE other_flavor._sqlx_migrations_custom (id integer);
         CREATE TABLE public._sqlx_migrations_partitioned (version bigint)
             PARTITION BY RANGE (version)",
    )
    .execute(&mut admin)
    .await
    .expect("seed discovery boundaries");
    split_role_urls_for(guard.name(), &[])
        .await
        .expect("provision around non-ledger objects");

    for relation in [
        "public.xsqlx_migrations_lookalike",
        "public._sqlxXmigrations_lookalike",
        "public._sqlx_migration",
        "public._sqlx_migrations_view",
        "public._sqlx_migrations_sequence",
        "other_flavor._sqlx_migrations_custom",
    ] {
        let admin_owns: bool = sqlx::query_scalar(
            "SELECT pg_get_userbyid(relowner) = current_user
               FROM pg_class WHERE oid = $1::regclass",
        )
        .bind(relation)
        .fetch_one(&mut admin)
        .await
        .expect("non-ledger ownership");
        assert!(admin_owns, "fixture must leave {relation} with admin");
    }
    let platform_owns_partitioned: bool = sqlx::query_scalar(
        "SELECT pg_get_userbyid(relowner) = 'proxima_test_platform'
           FROM pg_class WHERE oid = 'public._sqlx_migrations_partitioned'::regclass",
    )
    .fetch_one(&mut admin)
    .await
    .expect("partitioned ledger ownership");
    assert!(platform_owns_partitioned);
    admin.close().await.expect("close admin");
}

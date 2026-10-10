//! `SplitRoleDb::from_template`: the clone half of a migrated split-role
//! template. The clone is owned by a guard, arrives with the split roles
//! prepared, and keeps their enforcement: the runtime role owns nothing.

use std::process::Command;

use proxima_pg_testkit::{
    SplitRoleDb, TemplateFamily, TemplateLease, admin_url, db_url, drop_db, ensure_template_in,
    split_role_urls,
};
use sqlx::{Connection, PgConnection, PgPool};
use uuid::Uuid;

const CHILD_VAR: &str = "PROXIMA_PG_TESTKIT_CHILD";
const TEMPLATE_VAR: &str = "PROXIMA_PG_TESTKIT_TEMPLATE";

fn family() -> TemplateFamily {
    let id = &Uuid::now_v7().simple().to_string()[20..];
    TemplateFamily::new(&format!("tfsplit{id}_")).expect("test family")
}

async fn exists(name: &str) -> bool {
    let mut conn = PgConnection::connect(&admin_url()).await.expect("admin");
    let found = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname = $1)",
    )
    .bind(name)
    .fetch_one(&mut conn)
    .await
    .expect("exists");
    conn.close().await.expect("close");
    found
}

/// A split-role template with one platform-owned table in a host schema, built
/// the way `HostTemplate` builds: roles on the staging database, then DDL as
/// the platform role. The lease is held until the test is done with it.
async fn host_template(family: &TemplateFamily) -> TemplateLease {
    ensure_template_in(family, 1, |pool| async move {
        let staging: String = sqlx::query_scalar("SELECT current_database()")
            .fetch_one(&pool)
            .await?;
        pool.close().await;
        let (_, platform_url) = split_role_urls(&staging).await?;
        let platform = PgPool::connect(&platform_url).await?;
        sqlx::raw_sql(
            "CREATE SCHEMA hostapp;
             CREATE TABLE hostapp.notes (id integer PRIMARY KEY, body text NOT NULL)",
        )
        .execute(&platform)
        .await?;
        platform.close().await;
        Ok(())
    })
    .await
    .expect("build the host template")
}

/// Give the lease up and drop the template.
async fn cleanup(template: TemplateLease) {
    let name = template.name().to_owned();
    template.release().await.expect("release the lease");
    drop_db(&name).await.expect("clean up");
}

fn refused_with_42501(result: Result<sqlx::postgres::PgQueryResult, sqlx::Error>) -> bool {
    result.is_err_and(|error| {
        error
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::code)
            .as_deref()
            == Some("42501")
    })
}

#[tokio::test]
async fn a_clone_holds_the_template_and_enforces_the_split_roles() {
    let family = family();
    let template = host_template(&family).await;
    let db = SplitRoleDb::from_template("proxima_split_clone", &template, &["hostapp"])
        .await
        .expect("clone");

    assert!(
        db.name().starts_with("proxima_split_clone_"),
        "{}",
        db.name()
    );
    let mut runtime = PgConnection::connect(db.runtime_url())
        .await
        .expect("runtime");
    let mut platform = PgConnection::connect(db.platform_url())
        .await
        .expect("platform");
    let attributes: (String, bool, bool) = sqlx::query_as(
        "SELECT current_user::text, rolsuper, rolbypassrls FROM pg_roles WHERE rolname = current_user",
    )
    .fetch_one(&mut runtime)
    .await
    .expect("runtime attributes");
    assert_eq!(
        attributes,
        ("proxima_test_runtime".to_owned(), false, false)
    );
    let table_owner: String = sqlx::query_scalar(
        "SELECT pg_get_userbyid(relowner)::text FROM pg_class WHERE oid = 'hostapp.notes'::regclass",
    )
    .fetch_one(&mut platform)
    .await
    .expect("table owner");
    assert_eq!(
        table_owner, "proxima_test_platform",
        "the template carries the final state"
    );

    // DML through the platform's default privileges; no DDL, owning nothing.
    sqlx::query("INSERT INTO hostapp.notes VALUES (1, 'kept')")
        .execute(&mut runtime)
        .await
        .expect("runtime writes rows");
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM hostapp.notes")
        .fetch_one(&mut runtime)
        .await
        .expect("runtime reads rows");
    assert_eq!(rows, 1);
    assert!(refused_with_42501(
        sqlx::query("ALTER TABLE hostapp.notes ADD COLUMN extra integer")
            .execute(&mut runtime)
            .await
    ));
    assert!(refused_with_42501(
        sqlx::query("CREATE TABLE hostapp.other (id integer)")
            .execute(&mut runtime)
            .await
    ));
    runtime.close().await.expect("close");
    platform.close().await.expect("close");

    let name = db.name().to_owned();
    drop(db);
    cleanup(template).await;
    assert!(!exists(&name).await, "a passing drop removes the clone");
}

#[tokio::test]
async fn clones_of_one_template_are_distinct_and_independent() {
    let family = family();
    let template = host_template(&family).await;
    let first = SplitRoleDb::from_template("proxima_split_clone", &template, &["hostapp"])
        .await
        .expect("first");
    let second = SplitRoleDb::from_template("proxima_split_clone", &template, &["hostapp"])
        .await
        .expect("second");
    assert_ne!(first.name(), second.name());
    let pool = PgPool::connect(first.runtime_url()).await.expect("runtime");
    sqlx::query("INSERT INTO hostapp.notes VALUES (1, 'only in the first')")
        .execute(&pool)
        .await
        .expect("insert");
    let other = PgPool::connect(second.runtime_url())
        .await
        .expect("runtime");
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM hostapp.notes")
        .fetch_one(&other)
        .await
        .expect("count");
    pool.close().await;
    other.close().await;
    cleanup(template).await;
    assert_eq!(rows, 0, "a clone is a copy of the template");
}

#[tokio::test]
async fn a_bad_schema_name_fails_the_clone_and_leaves_no_database() {
    let family = family();
    let template = host_template(&family).await;
    let error = SplitRoleDb::from_template("proxima_split_refused", &template, &["Bad-Schema"])
        .await
        .expect_err("not a plain lowercase identifier");
    let leftovers: Vec<String> = {
        let mut admin = PgConnection::connect(&admin_url()).await.expect("admin");
        sqlx::query_scalar(
            "SELECT datname::text FROM pg_database WHERE datname LIKE 'proxima\\_split\\_refused\\_%'",
        )
        .fetch_all(&mut admin)
        .await
        .expect("leftovers")
    };
    cleanup(template).await;
    assert!(matches!(error, sqlx::Error::Protocol(_)), "{error:?}");
    assert!(
        leftovers.is_empty(),
        "the failed clone is dropped: {leftovers:?}"
    );
}

#[tokio::test]
async fn a_panicking_test_keeps_the_clone() {
    let family = family();
    let template = host_template(&family).await;
    let db = SplitRoleDb::from_template("proxima_split_kept", &template, &["hostapp"])
        .await
        .expect("clone");
    let name = db.name().to_owned();
    let joined = std::thread::spawn(move || {
        let _db = db;
        panic!("intentional");
    })
    .join();
    let kept = exists(&name).await;
    drop_db(&name).await.expect("clean up the kept clone");
    cleanup(template).await;
    assert!(joined.is_err());
    assert!(
        kept,
        "a clone dropped while unwinding is kept for inspection"
    );
}

/// Re-executed by [`a_kept_clone_is_announced_with_a_redacted_url`]: clone
/// the template of the family in `PROXIMA_PG_TESTKIT_TEMPLATE` (already built
/// by the parent, so this only leases it), say which clone, and panic.
#[tokio::test]
async fn child() {
    let Ok(prefix) = std::env::var(TEMPLATE_VAR) else {
        return;
    };
    assert_eq!(std::env::var(CHILD_VAR).as_deref(), Ok("panic"));
    let template = host_template(&TemplateFamily::new(&prefix).expect("family")).await;
    let db = SplitRoleDb::from_template("proxima_split_announced", &template, &["hostapp"])
        .await
        .expect("clone");
    println!("DATABASE:{}", db.name());
    let _held = db;
    panic!("intentional");
}

#[tokio::test]
async fn a_kept_clone_is_announced_with_a_redacted_url() {
    let family = family();
    let template = host_template(&family).await;
    let output = Command::new(std::env::current_exe().expect("test binary"))
        .args(["--exact", "child", "--nocapture", "--test-threads=1"])
        .env(CHILD_VAR, "panic")
        .env(TEMPLATE_VAR, family.prefix())
        .env("RUST_BACKTRACE", "0")
        .output()
        .expect("re-execute the test binary");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    // The harness prints `test child ... ` before the child's own output.
    let name = stdout
        .lines()
        .find_map(|line| line.split_once("DATABASE:"))
        .map(|(_, name)| name.trim().to_owned())
        .expect("the child announces its clone");
    let kept = exists(&name).await;
    drop_db(&name).await.expect("clean up the kept clone");
    cleanup(template).await;

    assert!(!output.status.success(), "the child panics");
    assert!(kept);
    assert!(
        stderr.contains(&format!("keeping test database `{name}`")),
        "{stderr}"
    );
    let admin = url::Url::parse(&admin_url()).expect("admin URL");
    let expected = db_url(&name);
    assert!(stderr.contains("psql 'postgres"), "{stderr}");
    assert!(stderr.contains(&name), "{expected}: {stderr}");
    if let Some(password) = admin.password() {
        assert!(stderr.contains(":****@"), "{stderr}");
        assert!(
            !stderr.contains(&format!(":{password}@")),
            "the password is redacted: {stderr}"
        );
    }
}

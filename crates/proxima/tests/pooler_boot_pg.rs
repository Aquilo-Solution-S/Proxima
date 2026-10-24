//! Boot migrates on `PROXIMA_MIGRATION_DATABASE_URL` and serves on the
//! platform and runtime URLs (#493).
//!
//! With `PROXIMA_TEST_PGBOUNCER_URL` set (the `PgBouncer` CI lane), the
//! serving URLs go through that transaction-mode pooler and the migration
//! URL reaches Postgres directly, the deployment docs/operate.md describes.
//! Without it every URL is direct and the test still pins which URL boot
//! migrates on.

use std::error::Error;
use std::time::Duration;

use proxima::flavor::{FlavorBundle, FlavorRegistry, FlavorRegistryError, NamedMigrator};
use proxima::{
    AppInfo, AuthPath, AuthzContext, BuiltProxima, FactWrite, FlavorApp, GroupId, OwnerId,
    PgPoolConfig, Proxima, Role, ServedSurface, UserId,
};
use proxima_core::AgentNoteV1;
use proxima_pg_testkit::{
    create_db, db_url, drop_db, split_role_urls, test_pooler_url, through_test_pooler,
    unique_db_name,
};
use uuid::Uuid;

type TestResult<T> = Result<T, Box<dyn Error>>;

struct CoreApp;

impl FlavorBundle for CoreApp {
    fn register(_registry: &mut FlavorRegistry) -> Result<(), FlavorRegistryError> {
        Ok(())
    }

    fn migrators() -> Vec<NamedMigrator> {
        Vec::new()
    }
}

impl FlavorApp for CoreApp {
    fn app_info() -> AppInfo {
        AppInfo {
            id: "pooler-boot-test",
            title: "Pooler Boot Test",
            version: "1",
        }
    }
}

fn host_context(owner: OwnerId) -> AuthzContext {
    proxima_core::test_fixtures::authenticated_context(AuthzContext::for_subject_with_role(
        UserId::new(Uuid::now_v7()),
        [(owner, Role::admin())],
        AuthPath::HostBearer,
    ))
    .narrowed_to_owner(owner)
    .expect("trusted host resolved this exact owner")
}

struct Urls {
    runtime: String,
    platform: String,
    migration: String,
}

async fn boot(urls: &Urls, owner: OwnerId, skip_migrations: bool) -> TestResult<BuiltProxima> {
    let app = Proxima::<CoreApp>::app()
        .database_url(&urls.runtime)
        .store_number(0)
        .platform_database_url(&urls.platform)
        .migration_database_url(&urls.migration)
        .owner(owner)
        .served_surface(ServedSurface::All);
    let app = if skip_migrations {
        app.skip_migrations()
    } else {
        app
    };
    Ok(app.build().await?)
}

async fn write_one(runtime: &BuiltProxima, owner: OwnerId, text: &str) -> TestResult<()> {
    let note = AgentNoteV1 {
        note_id: Uuid::now_v7(),
        title: text.to_owned(),
        body: "Served through the pooler.".to_owned(),
        tags: Vec::new(),
        idempotency_key: None,
    };
    runtime
        .host()
        .engine()
        .ingest_fact(
            &host_context(owner),
            FactWrite::new(owner, "test/pooler-boot", &note),
        )
        .await?;
    Ok(())
}

async fn stop(runtime: BuiltProxima) {
    let pool = runtime.host().pool_for_tests().clone();
    runtime.shutdown().await;
    pool.close().await;
}

#[tokio::test]
async fn boot_migrates_direct_and_serves_through_the_pooler() -> TestResult<()> {
    let database = unique_db_name("proxima_pooler_boot");
    create_db(&database).await?;
    let result = tokio::time::timeout(
        Duration::from_mins(2),
        migrate_direct_serve_pooled(&database),
    )
    .await;
    drop_db(&database).await?;
    result?
}

async fn migrate_direct_serve_pooled(database: &str) -> TestResult<()> {
    let (runtime_url, platform_url) = split_role_urls(database).await?;
    let urls = Urls {
        runtime: through_test_pooler(&runtime_url),
        platform: through_test_pooler(&platform_url),
        migration: platform_url.clone(),
    };
    if test_pooler_url().is_some() {
        assert_ne!(
            urls.platform, urls.migration,
            "the lane routes the platform URL"
        );
    }
    let owner = OwnerId::from(GroupId::new(Uuid::now_v7()));

    // First boot migrates on the direct URL, then serves a write.
    let first = boot(&urls, owner, false).await?;
    write_one(&first, owner, "migrated direct").await?;
    stop(first).await;

    // A GitOps-style boot runs only the preflight, on the same URL.
    let second = boot(&urls, owner, true).await?;
    write_one(&second, owner, "preflight direct").await?;
    stop(second).await;

    let admin = sqlx::PgPool::connect(&db_url(database)).await?;
    let written: i64 =
        sqlx::query_scalar("SELECT count(*) FROM proxima_core.memory WHERE owner_id = $1")
            .bind(owner.into_inner())
            .fetch_one(&admin)
            .await?;
    // No session advisory lock of the migration run outlived it.
    let advisory: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_locks
          WHERE locktype = 'advisory'
            AND database = (SELECT oid FROM pg_database WHERE datname = current_database())",
    )
    .fetch_one(&admin)
    .await?;
    admin.close().await;
    assert_eq!(written, 2, "both boots served a write");
    assert_eq!(advisory, 0, "the migration lock left with its connection");
    Ok(())
}

/// Boot migrates on the migration URL, not the platform URL: pointed at a
/// port nothing listens on, boot fails before any schema exists, though the
/// platform URL works.
#[tokio::test]
async fn boot_migrates_on_the_migration_url_not_the_platform_url() -> TestResult<()> {
    let database = unique_db_name("proxima_migration_url");
    create_db(&database).await?;
    let result = tokio::time::timeout(
        Duration::from_mins(2),
        migration_url_is_the_one_used(&database),
    )
    .await;
    drop_db(&database).await?;
    result?
}

async fn migration_url_is_the_one_used(database: &str) -> TestResult<()> {
    let (runtime_url, platform_url) = split_role_urls(database).await?;
    let mut unreachable = reqwest::Url::parse(&platform_url)?;
    unreachable
        .set_host(Some("127.0.0.1"))
        .expect("a postgres URL takes a host");
    unreachable
        .set_port(Some(1))
        .expect("a postgres URL takes a port");
    let urls = Urls {
        runtime: runtime_url,
        platform: platform_url,
        migration: unreachable.to_string(),
    };
    let owner = OwnerId::from(GroupId::new(Uuid::now_v7()));
    let booted = Proxima::<CoreApp>::app()
        .database_url(&urls.runtime)
        .store_number(0)
        .platform_database_url(&urls.platform)
        .migration_database_url(&urls.migration)
        .pg_pool_config(PgPoolConfig {
            acquire_timeout: Duration::from_millis(300),
            ..PgPoolConfig::default()
        })
        .owner(owner)
        .served_surface(ServedSurface::All)
        .build()
        .await;
    assert!(
        booted.is_err(),
        "boot connects its migrations to the migration URL"
    );
    drop(booted);
    let admin = sqlx::PgPool::connect(&db_url(database)).await?;
    let migrated: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_namespace WHERE nspname = 'proxima_core')",
    )
    .fetch_one(&admin)
    .await?;
    admin.close().await;
    assert!(!migrated, "nothing migrated over the platform URL");
    Ok(())
}

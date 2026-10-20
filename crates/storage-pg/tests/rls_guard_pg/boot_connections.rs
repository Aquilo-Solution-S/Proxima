//! A serving boot validates its catalog on one connection per pool.
//!
//! Boot checks every composed table's row security, one statement per
//! table, then the definers and pgvector. A pooled connection goes back to
//! its pool in a spawned task, so a statement that acquires right after the
//! previous one released often finds the idle queue empty and opens a NEW
//! connection. Run statement by statement through the pool, the checks grew
//! a pool toward its maximum during boot. On a server near
//! `max_connections`, each of those connects was refused, retried until the
//! pool's acquire timeout, and the boot failed with `PoolTimedOut` in the
//! middle of a catalog check.
//!
//! The test caps the database's non-superuser connections at four: the
//! runtime pool's one held connection, and up to three on the platform
//! pool, whose first connection the caller opened and whose two
//! validation acquires (the scope's checks, then the boot's one platform
//! transaction) may each race a release. A connect over the cap is
//! refused, so a boot that spreads its checks over fresh connections fails
//! here. The census then pins the runtime side to exactly one connection.

use std::time::Duration;

use proxima_pg_testkit::{DbGuard, admin_url, split_role_urls};
use proxima_storage_pg::test_fixtures::create_core_db;
use proxima_storage_pg::{PgMigrationStorage, PgPlatformScope, PgPoolConfig, PgStorage, PgTuning};
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

/// The database's connection cap; see the module comment.
const CAP: i64 = 4;

/// Short enough that a refused connect fails the boot inside the test, long
/// enough for a loaded runner to answer a held connection's statements.
const ACQUIRE: Duration = Duration::from_secs(3);

/// Non-superuser backends on `database`, by role, read from the
/// maintenance database so the probe never counts against the cap.
async fn backends(admin: &sqlx::PgPool, database: &str) -> Vec<(String, i64)> {
    sqlx::query_as(
        "SELECT a.usename::text, count(*)::bigint
           FROM pg_stat_activity AS a JOIN pg_roles AS r ON r.oid = a.usesysid
          WHERE a.datname = $1 AND NOT r.rolsuper
          GROUP BY 1 ORDER BY 1",
    )
    .bind(database)
    .fetch_all(admin)
    .await
    .expect("backend census")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_serving_boot_validates_on_one_connection_per_pool() {
    let name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_core_db(&name).await.expect("PG required for tests");
    let _guard = DbGuard::adopt(name.clone());
    let (runtime_url, platform_url) = split_role_urls(&name).await.expect("split roles");
    let migration = PgMigrationStorage::connect_with_config(
        &platform_url,
        PgPoolConfig::default(),
        PgTuning::default(),
    )
    .await
    .expect("migration connection");
    migration.run_migrations().await.expect("migrations");
    migration
        .initialize_store_number(1)
        .await
        .expect("store number");
    drop(migration);

    let admin = sqlx::PgPool::connect(&admin_url())
        .await
        .expect("admin connection");
    // The migration pool's connections close in the background once it is
    // dropped; the cap must not count them.
    for _ in 0..500 {
        if backends(&admin, &name).await.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        backends(&admin, &name).await.is_empty(),
        "the migration pool's connections closed"
    );
    // SQL-POLICY: fixed-fragment — the database name is this test's own uuid-v7 name.
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "ALTER DATABASE \"{name}\" CONNECTION LIMIT {CAP}"
    )))
    .execute(&admin)
    .await
    .expect("cap the database's connections");

    let platform_pool = PgPoolOptions::new()
        .max_connections(10)
        .acquire_timeout(ACQUIRE)
        .connect(&platform_url)
        .await
        .expect("platform connection");
    let scope = PgPlatformScope::new(platform_pool, &["proxima_core"])
        .await
        .expect("the platform scope validates within the cap");
    let storage = PgStorage::connect_with_config(
        &runtime_url,
        PgPoolConfig {
            acquire_timeout: ACQUIRE,
            ..PgPoolConfig::default()
        },
        PgTuning::default(),
        scope,
        1,
    )
    .await
    .expect("the serving boot validates within the cap");

    let census = backends(&admin, &name).await;
    let runtime = census
        .iter()
        .find(|(role, _)| role.ends_with("_runtime"))
        .map_or(0, |(_, count)| *count);
    assert_eq!(
        runtime, 1,
        "the runtime pool validated on one held connection: {census:?}"
    );
    drop(storage);
}

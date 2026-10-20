//! A serving pool through a transaction-mode `PgBouncer` (#493).
//!
//! Runs only when `PROXIMA_TEST_PGBOUNCER_URL` names a `PgBouncer` in
//! transaction mode in front of the `PROXIMA_TEST_PG_URL` server, configured
//! as the CI `PgBouncer` lane is (`.github/workflows/ci.yml`):
//! `max_prepared_statements` above zero, `track_extra_parameters` including
//! `statement_timeout`, and `extra_float_digits` NOT in
//! `ignore_startup_parameters`. With the variable set, the storage fixtures
//! also route their serving pools through the pooler, so the export/erase
//! fence and maintenance lease suites run through it in that lane.

use std::time::Duration;

use proxima_pg_testkit::{db_url, drop_db, split_role_urls, test_pooler_url, through_test_pooler};
use proxima_storage_pg::PgPoolConfig;
use proxima_storage_pg::test_fixtures::create_core_db;
use uuid::Uuid;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// A fresh database and its runtime role's URL through the pooler; `None`
/// (a visible skip) when no pooler is configured.
async fn pooled_runtime() -> TestResult<Option<(String, String)>> {
    if test_pooler_url().is_none() {
        eprintln!("skipped: PROXIMA_TEST_PGBOUNCER_URL is not set");
        return Ok(None);
    }
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_core_db(&db_name).await?;
    let (runtime_url, _platform_url) = split_role_urls(&db_name).await?;
    Ok(Some((db_name, through_test_pooler(&runtime_url))))
}

/// The serving pool policy connects through the pooler: no
/// `extra_float_digits` startup parameter, and the statement timeout rides
/// the startup `options` the pooler tracks. Prepared statements work
/// across the pooler's shared server connections.
#[tokio::test]
async fn a_serving_pool_connects_through_a_transaction_pooler() -> TestResult {
    let Some((db_name, pooled)) = pooled_runtime().await? else {
        return Ok(());
    };
    let result: TestResult = async {
        let config = PgPoolConfig {
            max_connections: 2,
            statement_timeout: Duration::from_millis(7250),
            ..PgPoolConfig::default()
        };
        let pool = config.connect_lazy(&pooled)?;
        let timeout: String = sqlx::query_scalar("SHOW statement_timeout")
            .fetch_one(&pool)
            .await?;
        assert_eq!(timeout, "7250ms", "the pooler carried the timeout");
        for n in 0..50_i64 {
            let (next, third): (i64, f64) = sqlx::query_as("SELECT $1::int8 + 1, $2::float8 / 3")
                .bind(n)
                .bind(1.0_f64)
                .fetch_one(&pool)
                .await?;
            assert_eq!(next, n + 1);
            assert!((third - 1.0 / 3.0).abs() < f64::EPSILON);
        }
        let shortest: String = sqlx::query_scalar("SELECT (0.1::float8)::text")
            .fetch_one(&pool)
            .await?;
        assert_eq!(
            shortest, "0.1",
            "Postgres prints the shortest exact float text"
        );
        pool.close().await;
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}

/// The lane's pooler is strict: a bare sqlx connect, which sends
/// `extra_float_digits` at startup, is refused. This is what every Proxima
/// pool met before the serving policy stopped sending it, and what keeps
/// the test above honest.
#[tokio::test]
async fn the_lane_pooler_refuses_the_default_sqlx_startup() -> TestResult {
    let Some((db_name, pooled)) = pooled_runtime().await? else {
        return Ok(());
    };
    let refused = sqlx::PgPool::connect(&pooled).await;
    let _ = drop_db(&db_name).await;
    let error = refused.expect_err("the pooler refuses extra_float_digits");
    assert!(
        error.to_string().contains("extra_float_digits"),
        "refused for the startup parameter: {error}"
    );
    Ok(())
}

/// A session advisory lock taken through the pooler outlives its client:
/// it stays on the shared server connection after the client has gone,
/// for whichever client is handed that connection next. This is why
/// serving code takes no session-scoped lock (`no_session_sql.rs`).
#[tokio::test]
async fn a_session_lock_through_the_pooler_outlives_its_client() -> TestResult {
    let Some((db_name, pooled)) = pooled_runtime().await? else {
        return Ok(());
    };
    let result: TestResult = async {
        let key = 493_493_i64;
        let client = PgPoolConfig {
            max_connections: 1,
            ..PgPoolConfig::default()
        }
        .connect_lazy(&pooled)?;
        let taken: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
            .bind(key)
            .fetch_one(&client)
            .await?;
        assert!(taken);
        client.close().await;

        let admin = sqlx::PgPool::connect(&db_url(&db_name)).await?;
        let held: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_locks
              WHERE locktype = 'advisory' AND granted
                AND database = (SELECT oid FROM pg_database WHERE datname = current_database())
                AND ((classid::bigint << 32) | objid::bigint) = $1",
        )
        .bind(key)
        .fetch_one(&admin)
        .await?;
        admin.close().await;
        assert_eq!(held, 1, "the lock outlived the client that took it");
        Ok(())
    }
    .await;
    // Drops the database with its pooled server connections, and the lock.
    let _ = drop_db(&db_name).await;
    result
}

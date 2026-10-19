//! A failed object purge gives way to the rest of the queue (#482).
//!
//! Each failed delete records its attempt and backs the debt off, and the
//! drain selects only debts that are due, so keys that keep failing never
//! hold back the keys behind them. In-process stores; no object store needed.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use proxima_core::{ColdObjectStore, ObjectStoreId, PurgeDeleted, StorageError};
use proxima_pg_testkit::{db_url, drop_db};
use proxima_storage_pg::test_fixtures::create_core_db;
use proxima_storage_pg::{ColdPurgeRetryOptions, ColdPurgeRetryOutcome, PgStorage};
use sqlx::PgPool;
use uuid::Uuid;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const STORE: &str = "s3:test/backoff";

/// Holds `objects`; refuses every key in `failing` while `refusing` is set.
struct FlakyStore {
    id: ObjectStoreId,
    objects: Mutex<BTreeSet<String>>,
    failing: BTreeSet<String>,
    refusing: AtomicBool,
}

impl FlakyStore {
    fn new(objects: impl IntoIterator<Item = String>, failing: BTreeSet<String>) -> Self {
        Self {
            id: ObjectStoreId::parse(STORE).expect("test store id"),
            objects: Mutex::new(objects.into_iter().collect()),
            failing,
            refusing: AtomicBool::new(true),
        }
    }

    fn held(&self) -> usize {
        self.objects.lock().expect("objects").len()
    }
}

#[async_trait::async_trait]
impl ColdObjectStore for FlakyStore {
    fn store_id(&self) -> &ObjectStoreId {
        &self.id
    }

    async fn delete(&self, key: &str) -> Result<PurgeDeleted, StorageError> {
        if self.refusing.load(Ordering::SeqCst) && self.failing.contains(key) {
            return Err(StorageError::Unavailable(format!(
                "access denied for {key}\nretry later"
            )));
        }
        let removed = self.objects.lock().expect("objects").remove(key);
        Ok(PurgeDeleted::from_versions(u64::from(removed)))
    }
}

async fn fresh() -> TestResult<(String, PgStorage)> {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_core_db(&db_name).await?;
    let pg = PgStorage::connect_for_tests(db_url(&db_name)).await?;
    Ok((db_name, pg))
}

/// Owe `keys` in order, oldest first.
async fn owe(pool: &PgPool, keys: &[String]) -> TestResult {
    sqlx::query(
        "INSERT INTO proxima_core.cold_purge_pending (object_key, owner_id, object_store, enqueued_at)
         SELECT key, NULL, $2, now() - interval '1 hour' + ord * interval '1 millisecond'
           FROM unnest($1::text[]) WITH ORDINALITY AS t(key, ord)",
    )
    .bind(keys)
    .bind(STORE)
    .execute(pool)
    .await?;
    Ok(())
}

async fn debt_count(pool: &PgPool) -> TestResult<i64> {
    Ok(
        sqlx::query_scalar("SELECT count(*) FROM proxima_core.cold_purge_pending")
            .fetch_one(pool)
            .await?,
    )
}

/// What a maintenance pass does: drain batches until nothing is due.
async fn pass(pg: &PgStorage, batch: i64) -> TestResult<(u64, ColdPurgeRetryOutcome)> {
    let mut purged = 0;
    for _ in 0..100 {
        let outcome = pg
            .retry_cold_object_purges(ColdPurgeRetryOptions::default().batch_size(batch))
            .await?;
        purged += outcome.purged + outcome.absent;
        if outcome.due == 0 {
            return Ok((purged, outcome));
        }
    }
    Err("a pass must end once nothing is due".into())
}

fn keys(range: std::ops::Range<usize>) -> Vec<String> {
    range.map(|i| format!("objects/k{i:03}")).collect()
}

/// Acceptance: 150 debts, the oldest 100 failing. One pass purges the 50 and
/// defers the 100; an immediate retry selects nothing; once the 100 are due
/// again and the store recovers, the next pass empties the queue.
#[tokio::test]
async fn failing_keys_give_way_to_the_rest_of_the_queue() -> TestResult {
    let (db_name, pg) = fresh().await?;
    let result: TestResult = async {
        let all = keys(0..150);
        let failing: BTreeSet<String> = keys(0..100).into_iter().collect();
        let store = Arc::new(FlakyStore::new(all.clone(), failing));
        let pg = pg.with_cold(store.clone());
        let pool = pg.pool_for_tests();
        owe(pool, &all).await?;

        let (purged, last) = pass(&pg, 100).await?;
        assert_eq!(purged, 50, "every key that can be deleted is");
        assert_eq!((last.due, last.deferred), (0, 100));
        assert_eq!(last.remaining(), 100);
        assert_eq!(store.held(), 100);

        let again = pg
            .retry_cold_object_purges(ColdPurgeRetryOptions::default().batch_size(100))
            .await?;
        assert_eq!(again.selected, 0, "a backed-off debt is not retried early");

        sqlx::query(
            "UPDATE proxima_core.cold_purge_pending
                SET next_attempt_at = now() - interval '1 second'",
        )
        .execute(pool)
        .await?;
        store.refusing.store(false, Ordering::SeqCst);
        let (purged, last) = pass(&pg, 100).await?;
        assert_eq!(purged, 100);
        assert_eq!(last.remaining(), 0);
        assert_eq!(debt_count(pool).await?, 0);
        assert_eq!(store.held(), 0);
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}

/// The same queue, one call at a time: a batch of 100 failures purges
/// nothing, and the very next call reaches the 50 behind them.
#[tokio::test]
async fn the_next_call_reaches_past_a_failed_batch() -> TestResult {
    let (db_name, pg) = fresh().await?;
    let result: TestResult = async {
        let all = keys(0..150);
        let failing: BTreeSet<String> = keys(0..100).into_iter().collect();
        let store = Arc::new(FlakyStore::new(all.clone(), failing));
        let pg = pg.with_cold(store.clone());
        owe(pg.pool_for_tests(), &all).await?;
        let options = ColdPurgeRetryOptions::default().batch_size(100);

        let first = pg.retry_cold_object_purges(options).await?;
        assert_eq!(
            (
                first.selected,
                first.purged,
                first.failed,
                first.deferred,
                first.due
            ),
            (100, 0, 100, 100, 50)
        );
        let second = pg.retry_cold_object_purges(options).await?;
        assert_eq!(
            (second.selected, second.purged, second.failed),
            (50, 50, 0),
            "no head-of-line block"
        );
        assert_eq!((second.due, second.deferred), (0, 100));
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}

/// The delay doubles per failure from the base, and a debt that keeps
/// failing is named with its owner and last error once it reaches the
/// threshold. A re-enqueue makes it due again but keeps its attempts.
#[tokio::test]
async fn a_debt_that_keeps_failing_is_named_stuck() -> TestResult {
    let (db_name, pg) = fresh().await?;
    let result: TestResult = async {
        let key = "objects/stuck".to_owned();
        let store = Arc::new(FlakyStore::new(
            [key.clone()],
            [key.clone()].into_iter().collect(),
        ));
        let pg = pg.with_cold(store.clone());
        let pool = pg.pool_for_tests();
        let owner = Uuid::now_v7();
        sqlx::query("INSERT INTO proxima_core.owners (owner_id, kind) VALUES ($1, 'personal')")
            .bind(owner)
            .execute(pool)
            .await?;
        sqlx::query(
            "INSERT INTO proxima_core.cold_purge_pending (object_key, owner_id, object_store)
             VALUES ($1, $2, $3)",
        )
        .bind(&key)
        .bind(owner)
        .bind(STORE)
        .execute(pool)
        .await?;

        let delays = async |pool: &PgPool| -> TestResult<(i32, i64)> {
            Ok(sqlx::query_as(
                "SELECT attempts,
                        round(extract(epoch FROM next_attempt_at - last_attempt_at))::bigint
                   FROM proxima_core.cold_purge_pending",
            )
            .fetch_one(pool)
            .await?)
        };
        let options = ColdPurgeRetryOptions::default()
            .backoff_base(Duration::from_secs(10))
            .backoff_max(Duration::from_secs(25))
            .stuck_after(3);
        let make_due = async |pool: &PgPool| -> TestResult {
            sqlx::query(
                "UPDATE proxima_core.cold_purge_pending
                    SET next_attempt_at = now() - interval '1 second'",
            )
            .execute(pool)
            .await?;
            Ok(())
        };
        let mut seen = Vec::new();
        for _ in 0..3 {
            let outcome = pg.retry_cold_object_purges(options).await?;
            assert_eq!(outcome.failed, 1);
            seen.push(delays(pool).await?);
            make_due(pool).await?;
        }
        assert_eq!(
            seen,
            vec![(1, 10), (2, 20), (3, 25)],
            "base, doubled, then capped"
        );
        let outcome = pg.retry_cold_object_purges(options.dry_run(true)).await?;
        assert_eq!(outcome.stuck_count, 1);
        let stuck = &outcome.stuck[0];
        assert_eq!(stuck.object_key, key);
        assert_eq!(stuck.owner_id.map(proxima_core::OwnerId::into_inner), Some(owner));
        assert_eq!(stuck.attempts, 3);
        let error = stuck.last_error.as_deref().expect("last error kept");
        assert!(error.contains("access denied"), "{error}");
        assert!(!error.contains('\n'), "control characters are not stored: {error:?}");

        // A new erase owing the same key makes it due now; the attempts stay.
        sqlx::query(
            "UPDATE proxima_core.cold_purge_pending SET next_attempt_at = now() + interval '1 hour'",
        )
        .execute(pool)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.cold_purge_pending (object_key, owner_id, object_store)
             VALUES ($1, $2, $3)
             ON CONFLICT (object_store, object_key) DO UPDATE
                SET enqueued_at = now(), next_attempt_at = now()",
        )
        .bind(&key)
        .bind(owner)
        .bind(STORE)
        .execute(pool)
        .await?;
        let renewed = pg.retry_cold_object_purges(options.dry_run(true)).await?;
        assert_eq!((renewed.due, renewed.stuck_count), (1, 1));
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}

/// A key that holds nothing clears its debt and is counted apart; a dry run
/// selects only due debts and writes nothing.
#[tokio::test]
async fn absent_keys_clear_and_a_dry_run_writes_nothing() -> TestResult {
    let (db_name, pg) = fresh().await?;
    let result: TestResult = async {
        let held = keys(0..2);
        let store = Arc::new(FlakyStore::new(held.clone(), BTreeSet::new()));
        let pg = pg.with_cold(store.clone());
        let pool = pg.pool_for_tests();
        let owed = keys(0..5);
        owe(pool, &owed).await?;
        sqlx::query(
            "UPDATE proxima_core.cold_purge_pending
                SET next_attempt_at = now() + interval '1 hour', attempts = 1
              WHERE object_key = $1",
        )
        .bind(&owed[4])
        .execute(pool)
        .await?;
        let snapshot = async |pool: &PgPool| -> TestResult<Vec<(String, i32, String)>> {
            Ok(sqlx::query_as(
                "SELECT object_key, attempts, next_attempt_at::text
                   FROM proxima_core.cold_purge_pending ORDER BY object_key",
            )
            .fetch_all(pool)
            .await?)
        };
        let before = snapshot(pool).await?;
        let dry = pg
            .retry_cold_object_purges(ColdPurgeRetryOptions::default().dry_run(true))
            .await?;
        assert_eq!((dry.selected, dry.due, dry.deferred), (4, 4, 1));
        assert!(dry.dry_run);
        assert_eq!(snapshot(pool).await?, before, "a dry run writes nothing");
        assert_eq!(store.held(), 2);

        let drained = pg
            .retry_cold_object_purges(ColdPurgeRetryOptions::default())
            .await?;
        assert_eq!(
            (
                drained.selected,
                drained.purged,
                drained.absent,
                drained.failed
            ),
            (4, 2, 2, 0)
        );
        assert_eq!((drained.due, drained.deferred), (0, 1));
        assert_eq!(debt_count(pool).await?, 1);
        assert_eq!(store.held(), 0);
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}

/// A batch size or stuck threshold of zero is refused.
#[tokio::test]
async fn non_positive_options_are_refused() -> TestResult {
    let (db_name, pg) = fresh().await?;
    let result: TestResult = async {
        let pg = pg.with_cold(Arc::new(FlakyStore::new([], BTreeSet::new())));
        for options in [
            ColdPurgeRetryOptions::default().batch_size(0),
            ColdPurgeRetryOptions::default().stuck_after(0),
        ] {
            let error = pg
                .retry_cold_object_purges(options)
                .await
                .expect_err("refused");
            assert!(
                matches!(error, StorageError::ConstraintViolation(_)),
                "{error:?}"
            );
        }
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}

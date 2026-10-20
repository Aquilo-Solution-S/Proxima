//! Maintenance passes are serialized by a lease row, not a session lock.
//!
//! Each family (storage, embedding) has one live lease at a time; a second
//! taker skips. The holder renews it, releases it at the end, and a lease
//! nobody renews expires so another process can take it. Nothing is tied to
//! one database session, so the same holds behind a transaction pooler.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use proxima_core::{ColdObjectStore, ObjectStoreId, PurgeDeleted, StorageError};
use proxima_pg_testkit::{db_url, drop_db};
use proxima_storage_pg::test_fixtures::create_core_db;
use proxima_storage_pg::{ColdPurgeRetryOptions, PgStorage};
use sqlx::PgPool;
use uuid::Uuid;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const STORE: &str = "s3:test/lease";

async fn fresh() -> TestResult<(String, PgStorage)> {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_core_db(&db_name).await?;
    let pg = PgStorage::connect_for_tests(db_url(&db_name)).await?;
    Ok((db_name, pg))
}

async fn lease_rows(admin: &PgPool) -> TestResult<Vec<String>> {
    Ok(
        sqlx::query_scalar("SELECT name FROM proxima_core.maintenance_lease ORDER BY name")
            .fetch_all(admin)
            .await?,
    )
}

/// Make `name`'s lease look abandoned: its holder stopped renewing a while ago.
async fn expire(admin: &PgPool, name: &str) -> TestResult {
    let expired = sqlx::query(
        "UPDATE proxima_core.maintenance_lease
            SET expires_at = clock_timestamp() - interval '1 second'
          WHERE name = $1",
    )
    .bind(name)
    .execute(admin)
    .await?
    .rows_affected();
    assert_eq!(expired, 1, "{name} lease present");
    Ok(())
}

/// One live lease per family: a second taker of the same family gets `None`,
/// the other family is independent, and release frees it at once.
#[tokio::test]
async fn one_live_lease_per_family_and_release_frees_it() -> TestResult {
    let (db_name, pg) = fresh().await?;
    let result: TestResult = async {
        let other = PgStorage::connect_for_tests(db_url(&db_name)).await?;
        let storage = pg
            .try_storage_maintenance_lock()
            .await?
            .expect("free lease");
        assert!(
            other.try_storage_maintenance_lock().await?.is_none(),
            "a second storage pass skips"
        );
        assert!(
            pg.try_storage_maintenance_lock().await?.is_none(),
            "the same process is no exception"
        );
        let embedding = other
            .try_embedding_maintenance_lock()
            .await?
            .expect("the embedding family is independent");
        assert_eq!(
            lease_rows(pg.pool_for_tests()).await?,
            ["embedding", "storage"]
        );
        storage.renew().await?;

        storage.release().await?;
        let taken = other
            .try_storage_maintenance_lock()
            .await?
            .expect("a released lease is free immediately");
        taken.release().await?;
        embedding.release().await?;
        assert!(lease_rows(pg.pool_for_tests()).await?.is_empty());
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}

/// A holder that stops renewing (it crashed) blocks others only until its
/// lease expires; once taken over, the old holder's renew reports the loss.
#[tokio::test]
async fn an_expired_lease_is_taken_over_and_its_old_holder_learns_it() -> TestResult {
    let (db_name, pg) = fresh().await?;
    let result: TestResult = async {
        let other = PgStorage::connect_for_tests(db_url(&db_name)).await?;
        let stale = pg
            .try_storage_maintenance_lock()
            .await?
            .expect("free lease");
        assert!(other.try_storage_maintenance_lock().await?.is_none());

        expire(pg.pool_for_tests(), "storage").await?;
        let error = stale
            .renew()
            .await
            .expect_err("an expired lease is not renewed");
        assert!(matches!(error, StorageError::Unavailable(_)), "{error:?}");

        let fresh_holder = other
            .try_storage_maintenance_lock()
            .await?
            .expect("an expired lease is free");
        fresh_holder.renew().await?;
        assert!(
            stale.renew().await.is_err(),
            "the old holder stays out after the takeover"
        );
        stale.release().await?;
        assert_eq!(
            lease_rows(pg.pool_for_tests()).await?,
            ["storage"],
            "the old holder's release leaves the new holder's lease alone"
        );
        fresh_holder.release().await?;
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}

/// Dropping a guard without `release` still frees the lease (best effort,
/// in the background), so a pass that returns early does not hold the
/// family for a full lease term.
#[tokio::test]
async fn a_dropped_guard_frees_its_lease() -> TestResult {
    let (db_name, pg) = fresh().await?;
    let result: TestResult = async {
        drop(
            pg.try_embedding_maintenance_lock()
                .await?
                .expect("free lease"),
        );
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            if lease_rows(pg.pool_for_tests()).await?.is_empty() {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the dropped guard deleted its lease"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        pg.try_embedding_maintenance_lock()
            .await?
            .expect("free again")
            .release()
            .await?;
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}

/// Deletes anything, and during its first delete another process takes the
/// storage lease over (the drain's own lease having expired meanwhile).
struct StealingStore {
    id: ObjectStoreId,
    admin: PgPool,
    stolen: AtomicBool,
    deletes: Mutex<u32>,
}

#[async_trait::async_trait]
impl ColdObjectStore for StealingStore {
    fn store_id(&self) -> &ObjectStoreId {
        &self.id
    }

    async fn delete(&self, _key: &str) -> Result<PurgeDeleted, StorageError> {
        *self.deletes.lock().expect("deletes") += 1;
        if !self.stolen.swap(true, Ordering::SeqCst) {
            sqlx::query(
                "UPDATE proxima_core.maintenance_lease
                    SET holder = gen_random_uuid(), expires_at = clock_timestamp() + interval '1 minute'
                  WHERE name = 'storage'",
            )
            .execute(&self.admin)
            .await
            .map_err(|error| StorageError::Unavailable(error.to_string()))?;
        }
        Ok(PurgeDeleted::from_versions(1))
    }
}

/// The drain confirms its lease before every batch: once another pass holds
/// the lease, it stops after the batch in flight instead of running beside
/// that pass.
#[tokio::test]
async fn a_drain_stops_at_the_next_batch_once_its_lease_is_lost() -> TestResult {
    let (db_name, pg) = fresh().await?;
    let result: TestResult = async {
        let store = Arc::new(StealingStore {
            id: ObjectStoreId::parse(STORE).expect("test store id"),
            admin: pg.pool_for_tests().clone(),
            stolen: AtomicBool::new(false),
            deletes: Mutex::new(0),
        });
        let pg = pg.with_cold(store.clone());
        sqlx::query(
            "INSERT INTO proxima_core.cold_purge_pending (object_key, owner_id, object_store, enqueued_at)
             SELECT 'objects/' || n, NULL, $1, now() - interval '1 hour'
               FROM generate_series(1, 5) AS n",
        )
        .bind(STORE)
        .execute(pg.pool_for_tests())
        .await?;

        let error = pg
            .run_object_purge_pass(ColdPurgeRetryOptions::default().batch_size(1), 10)
            .await
            .expect_err("the drain stops once the lease is gone");
        assert!(matches!(error, StorageError::Unavailable(_)), "{error:?}");
        assert_eq!(*store.deletes.lock().expect("deletes"), 1, "one batch ran");
        let left: i64 = sqlx::query_scalar("SELECT count(*) FROM proxima_core.cold_purge_pending")
            .fetch_one(pg.pool_for_tests())
            .await?;
        assert_eq!(left, 4);
        assert_eq!(
            lease_rows(pg.pool_for_tests()).await?,
            ["storage"],
            "the stopped drain leaves the new holder's lease alone"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}

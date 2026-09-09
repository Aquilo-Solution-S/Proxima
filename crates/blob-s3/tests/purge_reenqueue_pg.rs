//! Real purge/PUT race with an explicitly modelled producer enqueue.
//! The producer SQL mirrors `resolve_erased_owner_row`; this is not a full
//! stage/owner-erase API interleaving test.

use std::error::Error;
use std::sync::Arc;
use std::time::Duration;

use aws_sdk_s3::Client;
use aws_sdk_s3::config::Region;
use aws_sdk_s3::primitives::ByteStream;
use proxima_blob_s3::{CitedBlobStore, S3RuntimeConfig};
use proxima_core::{ColdObjectStore, StorageError};
use proxima_pg_testkit::{create_db, db_url, drop_db, unique_db_name};
use proxima_storage_pg::{ColdPurgeRetryOptions, ColdPurgeRetryOutcome, PgStorage};
use tokio::sync::Notify;
use uuid::Uuid;

type TestResult<T> = Result<T, Box<dyn Error>>;
const OLD_BODY: &[u8] = b"old object whose purge response is delayed";
const NEW_BODY: &[u8] = b"new object with a renewed durable purge obligation";

struct PauseAfterDelete {
    inner: Arc<dyn ColdObjectStore>,
    deleted: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait::async_trait]
impl ColdObjectStore for PauseAfterDelete {
    fn backend(&self) -> &str {
        self.inner.backend()
    }

    async fn put(&self, key: &str, bytes: &[u8]) -> Result<(), StorageError> {
        self.inner.put(key, bytes).await
    }

    async fn get(&self, key: &str) -> Result<Vec<u8>, StorageError> {
        self.inner.get(key).await
    }

    async fn delete(&self, key: &str) -> Result<(), StorageError> {
        self.inner.delete(key).await?;
        self.deleted.notify_one();
        self.release.notified().await;
        Ok(())
    }
}

struct AbortOnDrop(tokio::task::AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct Observation {
    backend_after_old_purge: Option<String>,
    versions_after_old_purge: usize,
    body_after_old_purge: Vec<u8>,
    outcome: ColdPurgeRetryOutcome,
    subsequent_retry: ColdPurgeRetryOutcome,
    versions_after_retry: usize,
    backend_after_retry: Option<String>,
}

#[tokio::test]
async fn old_purge_completion_preserves_a_reenqueued_object() -> TestResult<()> {
    assert_reenqueue_survives(false).await
}

#[tokio::test]
async fn old_purge_completion_preserves_reenqueue_with_unchanged_timestamp() -> TestResult<()> {
    assert_reenqueue_survives(true).await
}

async fn assert_reenqueue_survives(preserve_timestamp: bool) -> TestResult<()> {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset");
        return Ok(());
    }
    let database = unique_db_name("proxima_reenqueue_purge");
    create_db(&database).await?;
    let pg = PgStorage::connect(&db_url(&database)).await?;
    pg.run_migrations().await?;
    let config = S3RuntimeConfig {
        force_path_style: true,
        ..S3RuntimeConfig::from_env()?
    };
    let client = s3_client(&config).await;
    let store = CitedBlobStore::new(pg.pool_for_tests().clone(), config.clone())?;
    let key = format!("objects/{}", Uuid::now_v7());
    let result = tokio::time::timeout(
        Duration::from_secs(30),
        run_race(
            &pg,
            &store,
            &client,
            &config.bucket,
            &key,
            preserve_timestamp,
        ),
    )
    .await;
    // Cleanup follows captured observations and precedes the final red/green
    // assertion. Delete only this fixture's unique key and database.
    let cleanup =
        tokio::time::timeout(Duration::from_secs(10), store.cold_store().delete(&key)).await;
    pg.pool_for_tests().close().await;
    drop_db(&database).await?;
    cleanup??;
    let observed = result??;
    eprintln!(
        "producer=conditional S3 PUT + modelled SQL enqueue; preserve_timestamp={preserve_timestamp}; remaining versions={}; debt={:?}; old retry={:?}; subsequent retry={:?}",
        observed.versions_after_old_purge,
        observed.backend_after_old_purge,
        observed.outcome,
        observed.subsequent_retry,
    );
    assert_eq!(observed.versions_after_old_purge, 1);
    assert_eq!(observed.body_after_old_purge, NEW_BODY);
    assert_eq!(
        observed.backend_after_old_purge.as_deref(),
        Some(config.bucket.as_str()),
        "an earlier physical deletion must not acknowledge a newly enqueued object version",
    );
    assert_eq!(
        (
            observed.outcome.purged,
            observed.outcome.failed,
            observed.outcome.remaining
        ),
        (0, 1, 1),
        "the stale completion must report the renewed debt as pending",
    );
    assert_eq!(
        (
            observed.subsequent_retry.selected,
            observed.subsequent_retry.purged,
            observed.subsequent_retry.failed,
            observed.subsequent_retry.remaining
        ),
        (1, 1, 0, 0),
    );
    assert_eq!(observed.versions_after_retry, 0);
    assert_eq!(observed.backend_after_retry, None);
    Ok(())
}

async fn run_race(
    pg: &PgStorage,
    store: &CitedBlobStore,
    client: &Client,
    bucket: &str,
    key: &str,
    preserve_timestamp: bool,
) -> TestResult<Observation> {
    put_absent(client, bucket, key, OLD_BODY).await?;
    let initial_enqueue = enqueue_modelled_producer(pg.pool_for_tests(), key, bucket, None).await?;
    let deleted = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let purging = pg.clone().with_cold(Arc::new(PauseAfterDelete {
        inner: Arc::new(store.cold_store()),
        deleted: Arc::clone(&deleted),
        release: Arc::clone(&release),
    }));
    let purge_task = tokio::spawn(async move {
        purging
            .retry_cold_object_purges(ColdPurgeRetryOptions {
                batch_size: 1,
                dry_run: false,
            })
            .await
    });
    let _abort = AbortOnDrop(purge_task.abort_handle());
    deleted.notified().await;
    if version_count(client, bucket, key).await? != 0 {
        return Err("the old version must be physically gone before the producer resumes".into());
    }
    // This real conditional PUT can now succeed, as a late stage's PUT would
    // after the prior canonical object has been removed.
    put_absent(client, bucket, key, NEW_BODY).await?;
    // The second case deliberately keeps the original timestamp while still
    // writing a new row version. This adversarial test control proves that
    // comparing only enqueued_at cannot distinguish every renewed debt.
    let preserved_time = preserve_timestamp.then_some(initial_enqueue);
    let renewed_enqueue =
        enqueue_modelled_producer(pg.pool_for_tests(), key, bucket, preserved_time).await?;
    if preserve_timestamp && renewed_enqueue != initial_enqueue {
        return Err("the adversarial enqueue must preserve the original timestamp".into());
    }
    if !preserve_timestamp && renewed_enqueue <= initial_enqueue {
        return Err("the second enqueue must record a newer obligation".into());
    }
    release.notify_one();
    let outcome = purge_task.await??;
    let backend_after_old_purge = sqlx::query_scalar(
        "SELECT backend FROM proxima_core.cold_purge_pending WHERE object_key = $1",
    )
    .bind(key)
    .fetch_optional(pg.pool_for_tests())
    .await?;
    let versions_after_old_purge = version_count(client, bucket, key).await?;
    let body_after_old_purge = client
        .get_object()
        .bucket(bucket)
        .key(key)
        .send()
        .await?
        .body
        .collect()
        .await?
        .into_bytes()
        .to_vec();
    // Keep the stale completion's observations intact, then use the real
    // unpaused S3 adapter for the next attempt at the renewed obligation.
    let subsequent_retry = pg
        .clone()
        .with_cold(Arc::new(store.cold_store()))
        .retry_cold_object_purges(ColdPurgeRetryOptions {
            batch_size: 1,
            dry_run: false,
        })
        .await?;
    let versions_after_retry = version_count(client, bucket, key).await?;
    let backend_after_retry = sqlx::query_scalar(
        "SELECT backend FROM proxima_core.cold_purge_pending WHERE object_key = $1",
    )
    .bind(key)
    .fetch_optional(pg.pool_for_tests())
    .await?;
    Ok(Observation {
        backend_after_old_purge,
        versions_after_old_purge,
        body_after_old_purge,
        outcome,
        subsequent_retry,
        versions_after_retry,
        backend_after_retry,
    })
}

async fn enqueue_modelled_producer(
    pool: &sqlx::PgPool,
    key: &str,
    bucket: &str,
    preserved_time: Option<time::OffsetDateTime>,
) -> TestResult<time::OffsetDateTime> {
    let mut tx = pool.begin().await?;
    proxima_storage_pg::access::owner_columns::lock_object_keys_tx(&mut tx, &[key.to_owned()])
        .await?;
    // Match the production missing-owner stage enqueue under its key fence.
    // Explicit SQL is the producer model, not a call to the stage API. The
    // optional preserved timestamp is an adversarial test-only control.
    let enqueued_at = sqlx::query_scalar(
        "INSERT INTO proxima_core.cold_purge_pending (object_key, owner_id, backend)
         VALUES ($1, NULL, $2)
         ON CONFLICT (object_key) DO UPDATE
            SET enqueued_at = COALESCE($3, now()), backend = EXCLUDED.backend
         RETURNING enqueued_at",
    )
    .bind(key)
    .bind(bucket)
    .bind(preserved_time)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(enqueued_at)
}

async fn put_absent(
    client: &Client,
    bucket: &str,
    key: &str,
    body: &'static [u8],
) -> TestResult<()> {
    client
        .put_object()
        .bucket(bucket)
        .key(key)
        .if_none_match("*")
        .body(ByteStream::from_static(body))
        .send()
        .await?;
    Ok(())
}

async fn version_count(client: &Client, bucket: &str, key: &str) -> TestResult<usize> {
    let page = client
        .list_object_versions()
        .bucket(bucket)
        .prefix(key)
        .send()
        .await?;
    if page.is_truncated() == Some(true) {
        return Err("fixture only creates one current version at a time".into());
    }
    Ok(page
        .versions()
        .iter()
        .filter(|version| version.key() == Some(key))
        .count()
        + page
            .delete_markers()
            .iter()
            .filter(|marker| marker.key() == Some(key))
            .count())
}

async fn s3_client(config: &S3RuntimeConfig) -> Client {
    let mut loader = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .region(Region::new(config.region.clone()));
    if let Some(endpoint) = &config.endpoint_url {
        loader = loader.endpoint_url(endpoint);
    }
    Client::from_conf(
        aws_sdk_s3::config::Builder::from(&loader.load().await)
            .force_path_style(true)
            .build(),
    )
}

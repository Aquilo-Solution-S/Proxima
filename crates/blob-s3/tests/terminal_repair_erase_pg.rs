use std::error::Error;
use std::sync::Arc;
use std::time::Duration;

use aws_sdk_s3::Client;
use aws_sdk_s3::config::Region;
use aws_sdk_s3::primitives::ByteStream;
use proxima_blob_s3::{
    BlobError, CitedBlobStore, CitedBlobUploadAbortTs, CitedBlobUploadCompleteTs,
    CitedBlobUploadPrepareTs, S3RuntimeConfig,
};
use proxima_core::owner_inverse::{
    EraseAuthorization, OwnerEraseOutcome, OwnerEraseTarget, OwnerSurfaces,
};
use proxima_core::storage_ports::OwnerInversePort;
use proxima_core::test_fixtures::owner_fixture;
use proxima_core::{
    AuthPath, AuthzContext, ColdObjectStore, FlavorRegistry, OwnerRef, StorageError,
};
use proxima_pg_testkit::{create_db, db_url, drop_db, unique_db_name};
use proxima_storage_pg::{ColdPurgeRetryOptions, PgStorage};

type TestResult<T> = Result<T, Box<dyn Error>>;
const BODY: &[u8] = b"terminal repair must not lose canonical erase debt";

/// A failed post-commit provider delete must leave the real erase's debt
/// inspectable. The later explicit retry uses the actual S3 adapter.
struct DeferPurge(Arc<dyn ColdObjectStore>);

#[async_trait::async_trait]
impl ColdObjectStore for DeferPurge {
    async fn put(&self, key: &str, bytes: &[u8]) -> Result<(), StorageError> {
        self.0.put(key, bytes).await
    }

    async fn get(&self, key: &str) -> Result<Vec<u8>, StorageError> {
        self.0.get(key).await
    }

    async fn delete(&self, _key: &str) -> Result<(), StorageError> {
        Err(StorageError::Unavailable(
            "test defers post-commit deletion".into(),
        ))
    }

    fn backend(&self) -> &str {
        self.0.backend()
    }
}

struct AbortTasks(Vec<tokio::task::AbortHandle>);

impl Drop for AbortTasks {
    fn drop(&mut self) {
        for task in &self.0 {
            task.abort();
        }
    }
}

enum Probe {
    Abort,
    Repair,
}

struct Observation {
    bucket: String,
    canonical_debt: Option<String>,
    canonical_versions_after_drain: usize,
    pending_versions_after_drain: usize,
    erase_waited_for_owner: bool,
}

/// Abort wins before locator publication; an owner erase then arrives while
/// terminal locator repair is paused. The canonical bytes must become durable
/// erase debt, whether the erase precedes repair or waits for its transaction.
#[tokio::test]
async fn terminal_locator_repair_preserves_owner_erase_debt() -> TestResult<()> {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset");
        return Ok(());
    }
    let database = unique_db_name("proxima_terminal_repair");
    create_db(&database).await?;
    let result = tokio::time::timeout(Duration::from_secs(40), run_race(&database)).await;
    // Final contract assertions happen after cleanup, including on the red
    // baseline where canonical bytes survived an apparently completed erase.
    drop_db(&database).await?;
    let observed = result??;
    eprintln!(
        "erase waited for owner fence: {}; canonical debt: {:?}; canonical versions after drain: {}",
        observed.erase_waited_for_owner,
        observed.canonical_debt,
        observed.canonical_versions_after_drain,
    );
    assert_eq!(
        observed.canonical_debt.as_deref(),
        Some(observed.bucket.as_str()),
        "erasing the terminal upload must owe its canonical object to the correct bucket",
    );
    assert_eq!(observed.canonical_versions_after_drain, 0);
    assert_eq!(observed.pending_versions_after_drain, 0);
    Ok(())
}

#[allow(clippy::too_many_lines)] // two barriers pin the actual abort/stage/erase APIs
async fn run_race(database: &str) -> TestResult<Observation> {
    let mut tasks = AbortTasks(Vec::new());
    let pg = PgStorage::connect(&db_url(database)).await?;
    pg.run_migrations().await?;
    let pool = pg.pool_for_tests();
    let config = S3RuntimeConfig {
        force_path_style: true,
        ..S3RuntimeConfig::from_env()?
    };
    let client = s3_client(&config).await;
    let store = CitedBlobStore::new(pool.clone(), config.clone())?;
    let owner = owner_fixture();
    let ctx = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
    let prepared = store
        .prepare_upload(
            &ctx,
            CitedBlobUploadPrepareTs {
                owner,
                filename: "terminal-repair.pdf".into(),
                mime: "application/pdf".into(),
                byte_len: BODY.len() as u64,
            },
        )
        .await?;
    let pending = format!("pending/{}", prepared.upload_id);
    let canonical = format!("objects/{}", prepared.upload_id);
    client
        .put_object()
        .bucket(&config.bucket)
        .key(&pending)
        .body(ByteStream::from_static(BODY))
        .send()
        .await?;

    sqlx::raw_sql(
        "CREATE SEQUENCE abort_probe;
         CREATE SEQUENCE locator_probe;
         CREATE FUNCTION block_abort() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
             PERFORM nextval('abort_probe');
             PERFORM pg_advisory_xact_lock(hashtextextended(current_database() || ':abort', 0));
             RETURN NEW;
         END $$;
         CREATE TRIGGER block_abort BEFORE UPDATE OF status ON proxima_core.blob_uploads
             FOR EACH ROW WHEN (OLD.status = 'pending' AND NEW.status = 'aborted')
             EXECUTE FUNCTION block_abort();
         CREATE FUNCTION block_terminal_locator() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
             IF nextval('locator_probe') = 2 THEN
                 PERFORM pg_advisory_xact_lock(hashtextextended(current_database() || ':repair', 0));
             END IF;
             RETURN NULL;
         END $$;
         CREATE TRIGGER block_terminal_locator BEFORE UPDATE OF object_key
             ON proxima_core.blob_uploads FOR EACH STATEMENT
             EXECUTE FUNCTION block_terminal_locator();",
    )
    .execute(pool)
    .await?;
    let mut abort_barrier = pool.begin().await?;
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended(current_database() || ':abort', 0))",
    )
    .execute(&mut *abort_barrier)
    .await?;
    let mut repair_barrier = pool.begin().await?;
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended(current_database() || ':repair', 0))",
    )
    .execute(&mut *repair_barrier)
    .await?;

    let abort_store = store.clone();
    let abort_ctx = ctx.clone();
    let abort_id = prepared.upload_id.clone();
    let abort_task = tokio::spawn(async move {
        abort_store
            .abort_upload(
                &abort_ctx,
                CitedBlobUploadAbortTs {
                    owner,
                    upload_id: abort_id,
                },
            )
            .await
    });
    tasks.0.push(abort_task.abort_handle());
    wait_for_probe(pool, Probe::Abort).await?;
    let stage_store = store.clone();
    let stage_task = tokio::spawn(async move {
        stage_store
            .stage_upload(
                &ctx,
                CitedBlobUploadCompleteTs {
                    owner,
                    upload_id: prepared.upload_id,
                },
            )
            .await
    });
    tasks.0.push(stage_task.abort_handle());
    // Provider work must finish while abort still owns the upload row/fences.
    loop {
        if client
            .head_object()
            .bucket(&config.bucket)
            .key(&canonical)
            .send()
            .await
            .is_ok()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    abort_barrier.commit().await?;
    assert!(abort_task.await??.aborted);
    wait_for_probe(pool, Probe::Repair).await?;

    // Inject one provider-unavailable phase, so a successful in-band delete
    // cannot remove the debt before it is inspected. Never use PgStorage's
    // default MemoryColdStore here: it accepts deletes without reaching S3.
    let erase_pg = pg
        .clone()
        .with_cold(Arc::new(DeferPurge(Arc::new(store.cold_store()))));
    let erase_task = tokio::spawn(async move {
        let OwnerRef::Personal(user_id) = owner else {
            panic!("fixture must be personal");
        };
        let auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalOwner {
            user_id,
            drop_event_id: "terminal-repair-race".into(),
        });
        let surfaces =
            OwnerSurfaces::for_registry(&FlavorRegistry::new().freeze_or_panic_for_tests());
        OwnerInversePort::erase_personal_owner(&erase_pg, &auth, user_id, &surfaces).await
    });
    tasks.0.push(erase_task.abort_handle());
    // Both versions have a deterministic rendezvous: the broken implementation
    // permits erase to commit, while an atomic repair makes erase wait on the
    // owner fence. Do not await erase while withholding that transaction's gate.
    let erase_waited_for_owner = loop {
        if erase_task.is_finished() {
            break false;
        }
        if owner_fence_waiting(pool, owner).await? {
            break true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    repair_barrier.commit().await?;
    let erased = erase_task.await??;
    assert!(
        matches!(
            erased,
            OwnerEraseOutcome::Completed {
                cold_object_purge_pending: true,
                ..
            }
        ),
        "the injected provider failure must leave durable erase debt: {erased:?}"
    );
    let stage = stage_task.await?;
    assert!(
        matches!(stage, Err(BlobError::State(ref message)) if message == "upload is aborted" || message == "upload not found for Owner"),
        "a terminal/erased upload is not a successful stage: {stage:?}",
    );
    let rows: (i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM proxima_core.blob_uploads),
                (SELECT count(*) FROM proxima_core.blob),
                (SELECT count(*) FROM proxima_core.memory)",
    )
    .fetch_one(pool)
    .await?;
    assert_eq!(rows, (0, 0, 0));
    let canonical_debt: Option<String> = sqlx::query_scalar(
        "SELECT backend FROM proxima_core.cold_purge_pending WHERE object_key = $1",
    )
    .bind(&canonical)
    .fetch_optional(pool)
    .await?;
    let retained = client
        .get_object()
        .bucket(&config.bucket)
        .key(&canonical)
        .send()
        .await?
        .body
        .collect()
        .await?
        .into_bytes();
    assert_eq!(
        retained.as_ref(),
        BODY,
        "debt precedes physical destruction"
    );
    let drained = pg
        .clone()
        .with_cold(Arc::new(store.cold_store()))
        .retry_cold_object_purges(ColdPurgeRetryOptions {
            batch_size: 10,
            dry_run: false,
        })
        .await?;
    assert_eq!(
        (drained.selected, drained.purged, drained.failed),
        (1, 1, 0)
    );
    let canonical_left = versions(&client, &config.bucket, &canonical).await?;
    let pending_left = versions(&client, &config.bucket, &pending).await?;
    // Remove only this test's leftover canonical versions on the red baseline;
    // retain the observation, so cleanup cannot turn the regression green.
    for version in &canonical_left {
        client
            .delete_object()
            .bucket(&config.bucket)
            .key(&canonical)
            .version_id(version)
            .send()
            .await?;
    }
    pool.close().await;
    Ok(Observation {
        bucket: config.bucket,
        canonical_debt,
        canonical_versions_after_drain: canonical_left.len(),
        pending_versions_after_drain: pending_left.len(),
        erase_waited_for_owner,
    })
}

async fn wait_for_probe(pool: &sqlx::PgPool, probe: Probe) -> TestResult<()> {
    loop {
        let reached: bool = match probe {
            Probe::Abort => {
                sqlx::query_scalar("SELECT is_called FROM abort_probe")
                    .fetch_one(pool)
                    .await?
            }
            Probe::Repair => {
                sqlx::query_scalar("SELECT is_called AND last_value >= 2 FROM locator_probe")
                    .fetch_one(pool)
                    .await?
            }
        };
        if reached {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn owner_fence_waiting(pool: &sqlx::PgPool, owner: OwnerRef) -> TestResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1 FROM pg_locks
              WHERE locktype = 'advisory' AND NOT granted AND mode = 'ExclusiveLock'
                AND classid::bigint = ((hashtextextended(
                    'proxima-owner-fence:personal:' || $1::text, 0
                ) >> 32) & 4294967295)
                AND objid::bigint = (hashtextextended(
                    'proxima-owner-fence:personal:' || $1::text, 0
                ) & 4294967295)
         )",
    )
    .bind(owner.stored_owner_id())
    .fetch_one(pool)
    .await?)
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

async fn versions(client: &Client, bucket: &str, key: &str) -> TestResult<Vec<String>> {
    let page = client
        .list_object_versions()
        .bucket(bucket)
        .prefix(key)
        .send()
        .await?;
    assert_ne!(
        page.is_truncated(),
        Some(true),
        "this fixture writes only one version"
    );
    Ok(page
        .versions()
        .iter()
        .map(|v| (v.key(), v.version_id()))
        .chain(
            page.delete_markers()
                .iter()
                .map(|v| (v.key(), v.version_id())),
        )
        .filter(|(found, _)| *found == Some(key))
        .map(|(_, version)| version.expect("version identity").to_owned())
        .collect())
}

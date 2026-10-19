//! Erase and abort owe every key an upload implies, whatever its row records.
//!
//! An upload names two object keys by construction: the presigned transfer
//! key `pending/<id>` and the canonical `objects/<id>`. The row records only
//! one of them, and a crash between the canonical PUT and the locator update,
//! or a transfer-key delete that failed, leaves bytes under the other. Each
//! test here builds that state against the real store, on a versioned and an
//! unversioned bucket, and proves that no version and no delete marker
//! survives the erase or abort plus a drain.

use super::*;
use proxima_core::owner_inverse::{EraseAuthorization, OwnerEraseOutcome, OwnerEraseTarget};
use proxima_storage_pg::{ColdPurgeRetryOptions, ColdPurgeRetryOutcome};

/// A provider that refuses every delete, so a test can inspect the debt an
/// erase or abort left before the real store drains it.
struct DeferPurge(Arc<dyn ColdObjectStore>);

#[async_trait::async_trait]
impl ColdObjectStore for DeferPurge {
    async fn delete(&self, _key: &str) -> Result<proxima_core::PurgeDeleted, StorageError> {
        Err(StorageError::Unavailable("test defers deletion".into()))
    }

    fn store_id(&self) -> &proxima_core::ObjectStoreId {
        self.0.store_id()
    }
}

/// `base`'s bucket, or its `-unversioned` sibling created with versioning
/// left off.
async fn bucket(versioned: bool) -> S3RuntimeConfig {
    let base = s3_config_for_dev();
    let config = if versioned {
        S3RuntimeConfig {
            bucket: format!("{}-versioned", base.bucket),
            ..base
        }
    } else {
        S3RuntimeConfig {
            bucket: format!("{}-unversioned", base.bucket),
            ..base
        }
    };
    let client = config.client();
    let _ = client.create_bucket().bucket(&config.bucket).send().await;
    if versioned {
        client
            .put_bucket_versioning()
            .bucket(&config.bucket)
            .versioning_configuration(
                aws_sdk_s3::types::VersioningConfiguration::builder()
                    .status(aws_sdk_s3::types::BucketVersioningStatus::Enabled)
                    .build(),
            )
            .send()
            .await
            .expect("enable versioning");
    }
    config
}

/// Versions plus delete markers stored under exactly `key`.
async fn versions_left(config: &S3RuntimeConfig, key: &str) -> usize {
    let listed = config
        .client()
        .list_object_versions()
        .bucket(&config.bucket)
        .prefix(key)
        .send()
        .await
        .expect("list object versions");
    let versions = listed
        .versions()
        .iter()
        .filter(|v| v.key() == Some(key))
        .count();
    let markers = listed
        .delete_markers()
        .iter()
        .filter(|m| m.key() == Some(key))
        .count();
    versions + markers
}

struct Fixture {
    pg: PgStorage,
    db_name: String,
    pool: sqlx::PgPool,
    config: S3RuntimeConfig,
    store: CitedBlobStore,
    user: UserId,
    owner: OwnerId,
    ctx: AuthzContext,
}

async fn fixture(versioned: bool) -> Fixture {
    let (pg, db_name) = fresh_storage().await;
    let pool = pg.pool_for_tests().clone();
    let config = bucket(versioned).await;
    let store = CitedBlobStore::new(
        pg.clone_pool_for_backend(),
        config.clone(),
        pg.platform_scope_for_host(),
    )
    .expect("valid S3 config");
    let user = UserId::new(Uuid::now_v7());
    let owner = OwnerId::from(user);
    let ctx = proxima_core::test_fixtures::authenticated_context(AuthzContext::single_owner(
        &owner,
        AuthPath::HostBearer,
    ));
    Fixture {
        pg,
        db_name,
        pool,
        config,
        store,
        user,
        owner,
        ctx,
    }
}

impl Fixture {
    /// Same endpoint and bucket, so the same store; every request is refused.
    fn refusing_store(&self) -> CitedBlobStore {
        let refusing = CitedBlobStore::new(
            self.pg.clone_pool_for_backend(),
            S3RuntimeConfig {
                key_pair: proxima_blob_s3::S3KeyPair::new(
                    std::env::var("PROXIMA_S3_ACCESS_KEY_ID").expect("access key"),
                    "not-the-secret".to_owned(),
                ),
                ..self.config.clone()
            },
            self.pg.platform_scope_for_host(),
        )
        .expect("valid S3 config");
        assert_eq!(refusing.store_id(), self.store.store_id());
        refusing
    }

    fn deferred(&self) -> PgStorage {
        self.pg
            .clone()
            .with_cold(Arc::new(DeferPurge(Arc::new(self.store.cold_store()))))
    }

    /// Make every debt due and drain it through the real store.
    async fn drain(&self) -> ColdPurgeRetryOutcome {
        sqlx::query("UPDATE proxima_core.cold_purge_pending SET next_attempt_at = now()")
            .execute(&self.pool)
            .await
            .expect("make debts due");
        self.pg
            .clone()
            .with_cold(Arc::new(self.store.cold_store()))
            .retry_cold_object_purges(ColdPurgeRetryOptions::default())
            .await
            .expect("drain")
    }

    async fn owed_keys(&self) -> Vec<String> {
        sqlx::query_scalar(
            "SELECT object_key FROM proxima_core.cold_purge_pending ORDER BY object_key",
        )
        .fetch_all(&self.pool)
        .await
        .expect("read debts")
    }

    async fn erase_owner_deferred(&self) {
        let auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalOwner {
            user_id: self.user,
            drop_event_id: "implied-keys".into(),
        });
        let outcome = OwnerInversePort::erase_personal_owner(
            &self.deferred(),
            &auth,
            self.user,
            &transfer_surfaces(),
        )
        .await
        .expect("owner erase");
        assert!(
            matches!(
                outcome,
                OwnerEraseOutcome::Completed {
                    cold_object_purge_pending: true,
                    ..
                }
            ),
            "{outcome:?}"
        );
    }

    /// A source-scoped erase whose post-commit purge is refused.
    async fn erase_source_with_deferred_purge(&self, source: &str) -> OwnerEraseOutcome {
        let source_id = proxima_core::SourceId::new(source);
        let auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalSourceScope {
            user_id: self.user,
            source_id: source_id.clone(),
            drop_event_id: "implied-keys-source".into(),
        });
        OwnerInversePort::erase_personal_source_scope(
            &self.deferred(),
            &auth,
            self.user,
            &source_id,
            &transfer_surfaces(),
        )
        .await
        .expect("source erase")
    }

    async fn erase_source_deferred(&self, source: &str) {
        let outcome = self.erase_source_with_deferred_purge(source).await;
        assert!(
            matches!(
                outcome,
                OwnerEraseOutcome::Completed {
                    cold_object_purge_pending: true,
                    ..
                }
            ),
            "{outcome:?}"
        );
    }

    async fn erase_series(&self, memory: proxima_core::MemoryId) {
        let context = proxima_storage_pg::PgHostStateEraseContext::for_surfaces_for_tests(
            transfer_surfaces(),
        )
        .expect("no host lifecycle tables");
        let mut tx = self.pool.begin().await.expect("begin series erase");
        let (erased, plan) = proxima_storage_pg::verbs::forget::erase_memory_series(
            &mut tx,
            &proxima_storage_pg::core_pg_sidecars(),
            &context,
            &self.owner,
            &[memory.into_inner()],
        )
        .await
        .expect("series erase");
        tx.commit().await.expect("commit series erase");
        assert_eq!(erased, 1);
        assert_eq!(plan.object_keys().len(), 2, "{:?}", plan.object_keys());
    }

    async fn finish(self) {
        drop(self.pool);
        drop_db(&self.db_name).await.expect("drop db");
    }
}

/// Acceptance: the crash-window row. The canonical PUT landed but the row
/// still records the transfer key; an owner erase owes both, and a drain
/// leaves no version of either.
async fn crash_window_row_is_fully_erased(versioned: bool) {
    let f = fixture(versioned).await;
    let body: &'static [u8] = b"crash-window-bytes";
    let (upload_id, pending_key) = prepare_and_put_upload(
        &f.pool,
        &f.store,
        &f.config,
        &f.ctx,
        f.owner,
        "crash.pdf",
        "application/pdf",
        body,
    )
    .await;
    let canonical_key = format!("objects/{upload_id}");
    put_object_via_sdk(&f.config, &canonical_key, body).await;
    let recorded: String =
        sqlx::query_scalar("SELECT object_key FROM proxima_core.blob_uploads WHERE upload_id = $1")
            .bind(Uuid::parse_str(&upload_id).expect("upload id"))
            .fetch_one(&f.pool)
            .await
            .expect("row");
    assert_eq!(
        recorded, pending_key,
        "the row still names the transfer key"
    );

    f.erase_owner_deferred().await;
    assert_eq!(
        f.owed_keys().await,
        vec![canonical_key.clone(), pending_key.clone()]
    );
    let drained = f.drain().await;
    assert_eq!(
        (
            drained.purged,
            drained.absent,
            drained.failed,
            drained.remaining()
        ),
        (2, 0, 0, 0)
    );
    assert_eq!(versions_left(&f.config, &canonical_key).await, 0);
    assert_eq!(versions_left(&f.config, &pending_key).await, 0);
    f.finish().await;
}

#[tokio::test]
async fn owner_erase_owes_the_canonical_object_of_a_crash_window_row_versioned() {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset");
        return;
    }
    crash_window_row_is_fully_erased(true).await;
}

#[tokio::test]
async fn owner_erase_owes_the_canonical_object_of_a_crash_window_row_unversioned() {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset");
        return;
    }
    crash_window_row_is_fully_erased(false).await;
}

#[derive(Clone, Copy, Debug)]
enum Scope {
    Owner,
    Source,
    Series,
}

/// Acceptance: a completed upload whose transfer-key delete failed. Every
/// erase scope owes `pending/<id>` although the row records `objects/<id>`.
/// A second upload whose transfer key really is gone clears as absent.
async fn leftover_transfer_key_is_erased(versioned: bool, scope: Scope) {
    let f = fixture(versioned).await;
    let completed = upload_one_object(
        &f.pg,
        &f.store,
        &f.config,
        &f.pool,
        &f.ctx,
        f.owner,
        b"completed-with-a-leftover",
    )
    .await
    .expect("upload");
    let pending_key = completed.object_key.replacen("objects/", "pending/", 1);
    assert_eq!(
        versions_left(&f.config, &pending_key).await,
        0,
        "completion retired the transfer key"
    );
    // Model a failed delete: bytes under the transfer key again.
    put_object_via_sdk(&f.config, &pending_key, b"completed-with-a-leftover").await;
    // A source erase selects by the Fact's source. An upload completion
    // attributes none, and Memory is append-only, so the fixture sets one
    // with triggers suspended for this transaction alone.
    let mut attribute = f.pool.begin().await.expect("begin attribution");
    sqlx::query("SET LOCAL session_replication_role = replica")
        .execute(&mut *attribute)
        .await
        .expect("suspend triggers");
    sqlx::query(
        "UPDATE proxima_core.memory SET source_id = 'implied-src', ingest_key = 'implied-key'
          WHERE t = $1",
    )
    .bind(completed.memory_id.into_inner())
    .execute(&mut *attribute)
    .await
    .expect("attribute the Fact to a source");
    attribute.commit().await.expect("commit attribution");

    match scope {
        Scope::Owner => f.erase_owner_deferred().await,
        Scope::Source => f.erase_source_deferred("implied-src").await,
        Scope::Series => f.erase_series(completed.memory_id).await,
    }
    assert_eq!(
        f.owed_keys().await,
        vec![completed.object_key.clone(), pending_key.clone()],
        "{scope:?}"
    );
    let drained = f.drain().await;
    assert_eq!(
        (
            drained.purged,
            drained.absent,
            drained.failed,
            drained.remaining()
        ),
        (2, 0, 0, 0),
        "{scope:?}"
    );
    assert_eq!(versions_left(&f.config, &completed.object_key).await, 0);
    assert_eq!(versions_left(&f.config, &pending_key).await, 0, "{scope:?}");
    f.finish().await;
}

#[tokio::test]
async fn every_erase_scope_owes_a_leftover_transfer_key_versioned() {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset");
        return;
    }
    for scope in [Scope::Owner, Scope::Source, Scope::Series] {
        leftover_transfer_key_is_erased(true, scope).await;
    }
}

#[tokio::test]
async fn every_erase_scope_owes_a_leftover_transfer_key_unversioned() {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset");
        return;
    }
    for scope in [Scope::Owner, Scope::Source, Scope::Series] {
        leftover_transfer_key_is_erased(false, scope).await;
    }
}

/// An erase of a completed upload whose transfer key is really gone owes it
/// anyway, and the drain counts it as absent.
#[tokio::test]
async fn a_key_that_never_existed_clears_as_absent() {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset");
        return;
    }
    let f = fixture(false).await;
    upload_one_object(
        &f.pg,
        &f.store,
        &f.config,
        &f.pool,
        &f.ctx,
        f.owner,
        b"plain-completed",
    )
    .await
    .expect("upload");
    f.erase_owner_deferred().await;
    let drained = f.drain().await;
    assert_eq!(
        (
            drained.purged,
            drained.absent,
            drained.failed,
            drained.remaining()
        ),
        (1, 1, 0, 0)
    );
    f.finish().await;
}

/// Acceptance: an abort whose own purge failed leaves both keys owed, and a
/// later drain through the real store removes the canonical object the stage
/// published. The plain abort path drains them itself.
async fn abort_owes_the_canonical_object(versioned: bool) {
    let f = fixture(versioned).await;
    let refusing = f.refusing_store();

    let mut canonical_keys = Vec::new();
    for (purging, label) in [(&refusing, "refused"), (&f.store, "plain")] {
        let body: &'static [u8] = b"staged-then-aborted";
        let (upload_id, pending_key) = prepare_and_put_upload(
            &f.pool,
            &f.store,
            &f.config,
            &f.ctx,
            f.owner,
            "aborted.pdf",
            "application/pdf",
            body,
        )
        .await;
        let staged = f
            .store
            .stage_upload(
                &f.ctx,
                CitedBlobUploadCompleteTs {
                    owner: f.owner,
                    upload_id: upload_id.clone(),
                },
            )
            .await
            .expect("stage");
        let canonical_key = staged.payload.object_key;
        let aborted = purging
            .abort_upload(
                &f.ctx,
                CitedBlobUploadAbortTs {
                    owner: f.owner,
                    upload_id,
                },
            )
            .await
            .expect("abort");
        assert!(aborted.aborted, "{label}");
        if label == "refused" {
            assert_eq!(
                f.owed_keys().await,
                vec![canonical_key.clone(), pending_key.clone()],
                "a refused purge leaves both keys owed"
            );
            assert!(
                head(&f.config.client(), &f.config.bucket, &canonical_key)
                    .await
                    .is_some()
            );
        } else {
            assert_eq!(versions_left(&f.config, &canonical_key).await, 0, "{label}");
        }
        canonical_keys.push((canonical_key, pending_key));
    }
    let drained = f.drain().await;
    // The refused abort's canonical object is removed; its transfer key was
    // already retired by the stage, so it clears as absent.
    assert_eq!(
        (
            drained.purged,
            drained.absent,
            drained.failed,
            drained.remaining()
        ),
        (1, 1, 0, 0)
    );
    for (canonical_key, pending_key) in &canonical_keys {
        assert_eq!(versions_left(&f.config, canonical_key).await, 0);
        assert_eq!(versions_left(&f.config, pending_key).await, 0);
    }
    f.finish().await;
}

#[tokio::test]
async fn an_abort_owes_the_canonical_object_versioned() {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset");
        return;
    }
    abort_owes_the_canonical_object(true).await;
}

#[tokio::test]
async fn an_abort_owes_the_canonical_object_unversioned() {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset");
        return;
    }
    abort_owes_the_canonical_object(false).await;
}

/// Acceptance: a source-scoped erase over an aborted staged upload. The
/// stage published the canonical object, the abort's purge was refused, and
/// the source erase cannot reach the aborted row (it has no blob). The
/// abort's debt outlives the erase, and the drain leaves no version of
/// either key.
async fn source_erase_over_an_aborted_staged_upload(versioned: bool) {
    let f = fixture(versioned).await;
    let refusing = f.refusing_store();
    let (upload_id, pending_key) = prepare_and_put_upload(
        &f.pool,
        &f.store,
        &f.config,
        &f.ctx,
        f.owner,
        "staged-then-aborted.pdf",
        "application/pdf",
        b"staged-then-aborted",
    )
    .await;
    let canonical_key = f
        .store
        .stage_upload(
            &f.ctx,
            CitedBlobUploadCompleteTs {
                owner: f.owner,
                upload_id: upload_id.clone(),
            },
        )
        .await
        .expect("stage")
        .payload
        .object_key;
    let aborted = refusing
        .abort_upload(
            &f.ctx,
            CitedBlobUploadAbortTs {
                owner: f.owner,
                upload_id,
            },
        )
        .await
        .expect("abort");
    assert!(aborted.aborted);
    let owed = vec![canonical_key.clone(), pending_key.clone()];
    assert_eq!(
        f.owed_keys().await,
        owed,
        "a refused purge leaves both owed"
    );
    assert!(versions_left(&f.config, &canonical_key).await > 0);

    let erased = f.erase_source_with_deferred_purge("implied-src").await;
    assert!(
        matches!(erased, OwnerEraseOutcome::Completed { .. }),
        "{erased:?}"
    );
    assert_eq!(f.owed_keys().await, owed, "the erase drops no debt");

    let drained = f.drain().await;
    assert_eq!((drained.failed, drained.remaining()), (0, 0), "{drained:?}");
    assert_eq!(versions_left(&f.config, &canonical_key).await, 0);
    assert_eq!(versions_left(&f.config, &pending_key).await, 0);
    f.finish().await;
}

#[tokio::test]
async fn a_source_erase_over_an_aborted_staged_upload_leaves_nothing_versioned() {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset");
        return;
    }
    source_erase_over_an_aborted_staged_upload(true).await;
}

#[tokio::test]
async fn a_source_erase_over_an_aborted_staged_upload_leaves_nothing_unversioned() {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset");
        return;
    }
    source_erase_over_an_aborted_staged_upload(false).await;
}

/// An erase of a crash-window row serializes with a stage on the canonical
/// key the row only implies: while that key's fence is held, the erase
/// waits.
#[tokio::test]
async fn an_erase_waits_on_the_canonical_key_a_crash_window_row_implies() {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset");
        return;
    }
    let f = fixture(false).await;
    let (upload_id, _pending_key) = prepare_and_put_upload(
        &f.pool,
        &f.store,
        &f.config,
        &f.ctx,
        f.owner,
        "fenced.pdf",
        "application/pdf",
        b"fenced-bytes",
    )
    .await;
    let canonical_key = format!("objects/{upload_id}");
    let mut fence = f.pool.begin().await.expect("begin fence");
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('proxima-object-key:' || $1, 0))")
        .bind(&canonical_key)
        .execute(&mut *fence)
        .await
        .expect("hold the canonical key fence");

    let erase_pg = f.pg.clone().with_cold(Arc::new(f.store.cold_store()));
    let user = f.user;
    let erase = tokio::spawn(async move {
        let auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalOwner {
            user_id: user,
            drop_event_id: "fenced".into(),
        });
        OwnerInversePort::erase_personal_owner(&erase_pg, &auth, user, &transfer_surfaces()).await
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let waiting: bool = sqlx::query_scalar(
            "SELECT EXISTS (
                 SELECT 1 FROM pg_locks l JOIN pg_database d ON d.oid = l.database
                  WHERE d.datname = current_database()
                    AND l.locktype = 'advisory' AND NOT l.granted
             )",
        )
        .fetch_one(&f.pool)
        .await
        .expect("read lock waits");
        if waiting {
            break;
        }
        assert!(
            !erase.is_finished(),
            "the erase did not wait on the derived key"
        );
        assert!(
            Instant::now() < deadline,
            "the erase never reached the key fence"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    fence.rollback().await.expect("release fence");
    erase
        .await
        .expect("erase join")
        .expect("erase completes after the fence");
    assert!(f.owed_keys().await.is_empty());
    f.finish().await;
}

/// Expiry observed at stage owes both keys too: a crash-window row that
/// expires leaves neither its transfer bytes nor its canonical bytes.
#[tokio::test]
async fn expiry_owes_both_keys_of_a_crash_window_row() {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset");
        return;
    }
    for versioned in [true, false] {
        let f = fixture(versioned).await;
        let body: &'static [u8] = b"expiring-bytes";
        let (upload_id, pending_key) = prepare_and_put_upload(
            &f.pool,
            &f.store,
            &f.config,
            &f.ctx,
            f.owner,
            "expiring.pdf",
            "application/pdf",
            body,
        )
        .await;
        let canonical_key = format!("objects/{upload_id}");
        put_object_via_sdk(&f.config, &canonical_key, body).await;
        sqlx::query(
            "UPDATE proxima_core.blob_uploads SET expires_at = now() - interval '1 second'
              WHERE upload_id = $1",
        )
        .bind(Uuid::parse_str(&upload_id).expect("upload id"))
        .execute(&f.pool)
        .await
        .expect("expire the upload");
        let error = f
            .store
            .stage_upload(
                &f.ctx,
                CitedBlobUploadCompleteTs {
                    owner: f.owner,
                    upload_id,
                },
            )
            .await
            .expect_err("an expired upload does not stage");
        assert!(matches!(error, BlobError::State(_)), "{error:?}");
        assert!(
            f.owed_keys().await.is_empty(),
            "the expiry drained its debt"
        );
        assert_eq!(versions_left(&f.config, &canonical_key).await, 0);
        assert_eq!(versions_left(&f.config, &pending_key).await, 0);
        f.finish().await;
    }
}

//! An upload's lifecycle ends once: one outcome per upload.

use super::*;

/// Stages through the real store, then aborts the upload before handing the
/// staged payload back: the abort wins the window between staging and the
/// Fact transaction.
struct AbortAfterStagePort {
    inner: CitedBlobStore,
}

#[async_trait::async_trait]
impl CitedBlobPort for AbortAfterStagePort {
    async fn prepare_upload(
        &self,
        authz: &AuthzContext,
        owner: OwnerId,
        filename: &str,
        mime: &str,
        byte_len: u64,
    ) -> Result<CitedBlobUploadPrepared, StorageError> {
        CitedBlobPort::prepare_upload(&self.inner, authz, owner, filename, mime, byte_len).await
    }

    async fn stage_upload(
        &self,
        authz: &AuthzContext,
        owner: OwnerId,
        upload_id: &str,
    ) -> Result<CitedBlobStaged, StorageError> {
        let staged = CitedBlobPort::stage_upload(&self.inner, authz, owner, upload_id).await?;
        let aborted = CitedBlobPort::abort_upload(&self.inner, authz, owner, upload_id).await?;
        assert!(aborted.aborted, "a staged, unfinished upload aborts");
        Ok(staged)
    }

    async fn finish_upload(
        &self,
        authz: &AuthzContext,
        owner: OwnerId,
        upload_id: &str,
        cited_object_id: Uuid,
    ) -> Result<(), StorageError> {
        CitedBlobPort::finish_upload(&self.inner, authz, owner, upload_id, cited_object_id).await
    }

    async fn abort_upload(
        &self,
        authz: &AuthzContext,
        owner: OwnerId,
        upload_id: &str,
    ) -> Result<CitedBlobUploadAborted, StorageError> {
        CitedBlobPort::abort_upload(&self.inner, authz, owner, upload_id).await
    }

    async fn read_url(
        &self,
        authz: &AuthzContext,
        owner: OwnerId,
        cited_object_id: Uuid,
    ) -> Result<CitedBlobReadUrl, StorageError> {
        CitedBlobPort::read_url(&self.inner, authz, owner, cited_object_id).await
    }

    async fn find_held_blobs(
        &self,
        authz: &AuthzContext,
        owner: OwnerId,
        content_hashes: &[[u8; 32]],
    ) -> Result<Vec<CitedBlobHeld>, StorageError> {
        CitedBlobPort::find_held_blobs(&self.inner, authz, owner, content_hashes).await
    }
}

pub(super) fn lifecycle_store(pg: &PgStorage, config: &S3RuntimeConfig) -> CitedBlobStore {
    CitedBlobStore::new(
        pg.clone_pool_for_backend(),
        config.clone(),
        pg.platform_scope_for_host(),
    )
    .expect("valid S3 config")
}

pub(super) fn host_ctx(owner: OwnerId) -> AuthzContext {
    proxima_core::test_fixtures::authenticated_context(AuthzContext::single_owner(
        &owner,
        AuthPath::HostBearer,
    ))
}

/// #483 item 1: a Fact that cites an upload aborted after staging is refused
/// in its own transaction; nothing is written and the row stays aborted.
#[tokio::test]
async fn a_fact_citing_an_aborted_upload_is_a_conflict() {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset");
        return;
    }
    let (pg, db_name) = fresh_storage().await;
    let pool = pg.pool_for_tests().clone();
    let config = s3_config_for_dev();
    let store = lifecycle_store(&pg, &config);
    let owner = owner_fixture();
    let ctx = host_ctx(owner);
    let body: &'static [u8] = b"%PDF-1.7 aborted before its Fact";
    let upload_id = staged_upload(&pool, &store, &config, &ctx, owner, "late.pdf", body).await;

    let service = CitedBlobService::new(Arc::new(AbortAfterStagePort {
        inner: store.clone(),
    }) as Arc<dyn CitedBlobPort>);
    let error = Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests())
        .with_storage_ports(Arc::new(pg.clone()).storage_ports())
        .complete_upload_as_fact(&service, &ctx, owner, &upload_id, &[])
        .await
        .expect_err("a Fact may not cite an aborted upload");
    assert_eq!(error.code, ErrorCode::InvalidArgument, "{error:?}");
    assert!(error.message.contains("aborted"), "{error:?}");
    assert_eq!(upload_status(&pool, &upload_id).await, "aborted");
    assert_eq!(corpus_counts(&pool).await, (0, 0), "nothing was written");

    // The direct port says the same, as a typed conflict.
    let finish = store
        .finish_upload(&ctx, owner, &upload_id, Uuid::now_v7())
        .await
        .expect_err("finish after abort is refused");
    assert!(
        matches!(finish, BlobError::Storage(StorageError::Conflict(ref message)) if message.contains("aborted")),
        "{finish:?}"
    );
    assert_eq!(upload_status(&pool, &upload_id).await, "aborted");

    drop(pool);
    drop_db(&db_name).await.expect("drop db");
}

/// An abort after the Fact committed is a no-op that reports `aborted: false`.
#[tokio::test]
async fn abort_after_completion_reports_not_aborted() {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset");
        return;
    }
    let (pg, db_name) = fresh_storage().await;
    let pool = pg.pool_for_tests().clone();
    let config = s3_config_for_dev();
    let store = lifecycle_store(&pg, &config);
    let owner = owner_fixture();
    let ctx = host_ctx(owner);
    let body: &'static [u8] = b"%PDF-1.7 completed first";
    let upload_id = staged_upload(&pool, &store, &config, &ctx, owner, "first.pdf", body).await;
    complete_via_engine(&pg, &store, &ctx, owner, &upload_id)
        .await
        .expect("complete");
    let aborted = store
        .abort_upload(
            &ctx,
            CitedBlobUploadAbortTs {
                owner,
                upload_id: upload_id.clone(),
            },
        )
        .await
        .expect("abort after completion");
    assert!(!aborted.aborted);
    assert_eq!(upload_status(&pool, &upload_id).await, "completed");
    drop(pool);
    drop_db(&db_name).await.expect("drop db");
}

/// An expired, never-staged upload cannot be finished either.
#[tokio::test]
async fn finish_after_expiry_is_a_conflict() {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset");
        return;
    }
    let (pg, db_name) = fresh_storage().await;
    let pool = pg.pool_for_tests().clone();
    let config = s3_config_for_dev();
    let store = lifecycle_store(&pg, &config);
    let owner = owner_fixture();
    let ctx = host_ctx(owner);
    let body: &'static [u8] = b"%PDF-1.7 never staged";
    let upload_id = staged_upload(&pool, &store, &config, &ctx, owner, "never.pdf", body).await;
    sqlx::query(
        "UPDATE proxima_core.blob_uploads SET expires_at = now() - interval '1 hour'
          WHERE upload_id = $1",
    )
    .bind(Uuid::parse_str(&upload_id).expect("upload id"))
    .execute(&pool)
    .await
    .expect("age upload");
    let error = complete_via_engine(&pg, &store, &ctx, owner, &upload_id)
        .await
        .expect_err("an unstaged upload past its TTL expires");
    assert_eq!(error.code, ErrorCode::InvalidArgument, "{error:?}");
    assert_eq!(upload_status(&pool, &upload_id).await, "expired");
    let finish = store
        .finish_upload(&ctx, owner, &upload_id, Uuid::now_v7())
        .await
        .expect_err("finish after expiry is refused");
    assert!(
        matches!(finish, BlobError::Storage(StorageError::Conflict(ref message)) if message.contains("expired")),
        "{finish:?}"
    );
    assert_eq!(corpus_counts(&pool).await, (0, 0));
    drop(pool);
    drop_db(&db_name).await.expect("drop db");
}

/// A staged upload is not expirable: past its TTL it still finishes.
#[tokio::test]
async fn a_staged_upload_past_its_ttl_still_finishes() {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset");
        return;
    }
    let (pg, db_name) = fresh_storage().await;
    let pool = pg.pool_for_tests().clone();
    let config = s3_config_for_dev();
    let store = lifecycle_store(&pg, &config);
    let owner = owner_fixture();
    let ctx = host_ctx(owner);
    let body: &'static [u8] = b"%PDF-1.7 staged then slow";
    let upload_id = staged_upload(&pool, &store, &config, &ctx, owner, "slow.pdf", body).await;
    store
        .stage_upload(
            &ctx,
            CitedBlobUploadCompleteTs {
                owner,
                upload_id: upload_id.clone(),
            },
        )
        .await
        .expect("stage");
    sqlx::query(
        "UPDATE proxima_core.blob_uploads SET expires_at = now() - interval '1 hour'
          WHERE upload_id = $1",
    )
    .bind(Uuid::parse_str(&upload_id).expect("upload id"))
    .execute(&pool)
    .await
    .expect("age upload");
    complete_via_engine(&pg, &store, &ctx, owner, &upload_id)
        .await
        .expect("a staged upload finishes past its TTL");
    assert_eq!(upload_status(&pool, &upload_id).await, "completed");
    assert_eq!(corpus_counts(&pool).await, (1, 1));
    drop(pool);
    drop_db(&db_name).await.expect("drop db");
}

/// #483 item 2: the dedupe probe answers a hash only for bytes a completed
/// upload holds. A cited-object row with no upload behind it is not held, and
/// the answer appears when the upload completes.
#[tokio::test]
async fn find_held_blobs_answers_only_completed_uploads() {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset");
        return;
    }
    let (pg, db_name) = fresh_storage().await;
    let pool = pg.pool_for_tests().clone();
    let config = s3_config_for_dev();
    let store = lifecycle_store(&pg, &config);
    let owner = owner_fixture();
    let ctx = host_ctx(owner);

    let unbacked = [9_u8; 32];
    sqlx::query(
        "INSERT INTO proxima_core.owners (owner_id, kind) VALUES ($1, 'personal')
         ON CONFLICT (owner_id) DO NOTHING",
    )
    .bind(owner.into_inner())
    .execute(&pool)
    .await
    .expect("owner row");
    sqlx::query(
        "INSERT INTO proxima_core.blob (schema_id, owner_id, content_hash)
         VALUES ('core/uploaded-blob-v1', $1, $2)",
    )
    .bind(owner.into_inner())
    .bind(&unbacked[..])
    .execute(&pool)
    .await
    .expect("a cited object row with no upload");
    let held = store
        .find_held_blobs(&ctx, owner, &[unbacked])
        .await
        .expect("probe");
    assert!(held.is_empty(), "no completed upload holds {unbacked:?}");

    let body: &'static [u8] = b"%PDF-1.7 held once completed";
    let upload_id = staged_upload(&pool, &store, &config, &ctx, owner, "held.pdf", body).await;
    let digest = *blake3::hash(body).as_bytes();
    let staged = store
        .find_held_blobs(&ctx, owner, &[digest])
        .await
        .expect("probe");
    assert!(staged.is_empty(), "a staged upload holds nothing yet");
    complete_via_engine(&pg, &store, &ctx, owner, &upload_id)
        .await
        .expect("complete");
    let held = store
        .find_held_blobs(&ctx, owner, &[digest, unbacked])
        .await
        .expect("probe");
    assert_eq!(held.len(), 1, "{held:?}");
    assert_eq!(held[0].content_hash, digest);

    drop(pool);
    drop_db(&db_name).await.expect("drop db");
}

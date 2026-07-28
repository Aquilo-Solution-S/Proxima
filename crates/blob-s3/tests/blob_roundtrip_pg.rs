use aws_config::BehaviorVersion;
use aws_sdk_s3::Client;
use aws_sdk_s3::config::Region;
use aws_sdk_s3::primitives::ByteStream;
use proxima_blob_s3::{
    BlobError, CitedBlobReadUrlTs, CitedBlobStore, CitedBlobUploadAbortTs,
    CitedBlobUploadCompleteTs, CitedBlobUploadPrepareTs, S3RuntimeConfig,
};
use proxima_core::storage_ports::{CitedBlobPort, CitedObjectErasePort};
use proxima_core::test_fixtures::owner_fixture;
use proxima_core::{AuthPath, AuthzContext, OwnerRef, UserId};
use proxima_pg_testkit::{create_db, db_url, drop_db, unique_db_name};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

async fn fresh_pool() -> (sqlx::PgPool, String) {
    let db_name = unique_db_name("proxima_blob_s3_test");
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let pg = PgStorage::connect(&url)
        .await
        .unwrap_or_else(|err| panic!("PG required for tests but unavailable: {err}"));
    if let Err(err) = pg.run_migrations().await {
        let _ = drop_db(&db_name).await;
        panic!("migration failed: {err}");
    }
    (pg.pool_for_tests().clone(), db_name)
}

fn s3_config_for_dev() -> S3RuntimeConfig {
    S3RuntimeConfig {
        bucket: std::env::var("PROXIMA_S3_BUCKET")
            .expect("S3 target required: start docker-compose.dev.yml and set PROXIMA_S3_*"),
        region: std::env::var("PROXIMA_S3_REGION").expect("PROXIMA_S3_REGION"),
        endpoint_url: std::env::var("PROXIMA_S3_ENDPOINT_URL").ok(),
        force_path_style: true,
        upload_ttl_seconds: 900,
        read_ttl_seconds: 900,
        max_blob_bytes: None,
    }
}

async fn s3_client(config: &S3RuntimeConfig) -> Client {
    let mut loader =
        aws_config::defaults(BehaviorVersion::latest()).region(Region::new(config.region.clone()));
    if let Some(endpoint_url) = &config.endpoint_url {
        loader = loader.endpoint_url(endpoint_url);
    }
    let shared = loader.load().await;
    let mut builder = aws_sdk_s3::config::Builder::from(&shared);
    if config.force_path_style {
        builder = builder.force_path_style(true);
    }
    Client::from_conf(builder.build())
}

async fn put_object_via_sdk(config: &S3RuntimeConfig, key: &str, body: &'static [u8]) {
    let client = s3_client(config).await;
    client
        .put_object()
        .bucket(&config.bucket)
        .key(key)
        .body(ByteStream::from_static(body))
        .send()
        .await
        .expect("put object");
}

#[tokio::test]
async fn prepare_then_complete_then_read_roundtrip() {
    // Opt-in integration test: needs a reachable S3 target. Skip
    // (rather than panic) when PROXIMA_S3_* is unset so the default
    // `cargo test --workspace` is green without standing up an object store.
    // CI sets these (see .github/workflows/ci.yml), so the lane is not dark there.
    if !S3RuntimeConfig::present_in_env() {
        eprintln!(
            "skipped: PROXIMA_S3_* unset (set PROXIMA_S3_BUCKET/REGION + run the s3 service to enable)"
        );
        return;
    }

    let (pool, db_name) = fresh_pool().await;
    let config = s3_config_for_dev();
    let store = CitedBlobStore::new(pool.clone(), config.clone()).expect("valid S3 config");
    let body = b"test-bytes";
    let owner = owner_fixture();
    let ctx = AuthzContext::single_owner(&owner, AuthPath::System);

    let prepared = store
        .prepare_upload(
            &ctx,
            CitedBlobUploadPrepareTs {
                owner,
                filename: "test.pdf".into(),
                mime: "application/pdf".into(),
                byte_len: body.len() as u64,
            },
        )
        .await
        .expect("prepare");
    let upload_id = Uuid::parse_str(&prepared.upload_id).expect("upload id");

    let row: (String, String) = sqlx::query_as(
        "SELECT bucket, object_key FROM proxima_core.cited_object_uploads WHERE upload_id = $1",
    )
    .bind(upload_id)
    .fetch_one(&pool)
    .await
    .expect("upload row");
    assert_eq!(row.0, config.bucket);
    put_object_via_sdk(&config, &row.1, body).await;

    let completed = store
        .complete_upload(
            &ctx,
            CitedBlobUploadCompleteTs {
                owner,
                upload_id: prepared.upload_id,
            },
        )
        .await
        .expect("complete");
    let url = store
        .read_url(
            &ctx,
            CitedBlobReadUrlTs {
                owner,
                cited_object_id: completed.cited_object_id.clone(),
            },
        )
        .await
        .expect("read url");
    // `complete` moves the object from its pending key to the final
    // content-addressed key (doc 11 §Large artefact storage), so the
    // presigned GET must reference the completed blob's key, not the
    // pending upload's.
    let cited_object_id = Uuid::parse_str(&completed.cited_object_id).expect("cited object id");
    let final_key: (String,) = sqlx::query_as(
        "SELECT object_key FROM proxima_core.cited_uploaded_blob_v1 WHERE cited_object_id = $1",
    )
    .bind(cited_object_id)
    .fetch_one(&pool)
    .await
    .expect("completed blob row");
    assert!(
        url.read_url.contains(&final_key.0),
        "presigned GET references the final object key"
    );
    assert!(
        final_key.0.starts_with("objects/"),
        "completed blob moved out of pending/"
    );
    assert!(
        url.read_url
            .contains("response-content-disposition=attachment"),
        "presigned GET forces an attachment disposition (no inline stored-XSS)"
    );

    drop(pool);
    drop_db(&db_name).await.expect("drop db");
}

/// The same roundtrip through the core-defined [`CitedBlobPort`] — the
/// seam `core_upload` dispatches through. Exercised as a `dyn` object so
/// the trait surface (plain args in, core-owned outcomes out, hex hashes,
/// parsed expiry timestamps) is what is under test, not the inherent
/// methods the Tauri path uses.
#[tokio::test]
async fn port_level_prepare_complete_read_roundtrip() {
    use sha2::Digest;

    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset (run the s3 service to enable)");
        return;
    }

    let (pool, db_name) = fresh_pool().await;
    let config = s3_config_for_dev();
    let store = CitedBlobStore::new(pool.clone(), config.clone()).expect("valid S3 config");
    let port: &dyn CitedBlobPort = &store;
    let body = b"port-level-bytes";
    let owner = owner_fixture();
    let ctx = AuthzContext::single_owner(&owner, AuthPath::System);

    let prepared = port
        .prepare_upload(
            &ctx,
            owner,
            "port.pdf",
            "application/pdf",
            body.len() as u64,
        )
        .await
        .expect("port prepare");
    assert!(
        prepared.expires_at > time::OffsetDateTime::now_utc(),
        "expiry is a parsed future timestamp"
    );

    // Upload the bytes to the pending key exactly as an MCP client would
    // PUT to the presigned URL (the SDK PUT targets the same object).
    let pending: (String,) = sqlx::query_as(
        "SELECT object_key FROM proxima_core.cited_object_uploads WHERE upload_id = $1",
    )
    .bind(Uuid::parse_str(&prepared.upload_id).expect("upload id"))
    .fetch_one(&pool)
    .await
    .expect("upload row");
    put_object_via_sdk(&config, &pending.0, body).await;

    let completed = port
        .complete_upload(&ctx, owner, &prepared.upload_id)
        .await
        .expect("port complete");
    assert_eq!(completed.byte_len, body.len() as u64);
    assert_eq!(completed.mime, "application/pdf");
    assert_eq!(completed.filename, "port.pdf");
    assert!(!completed.idempotent_replay);
    // Hashes are hex on the port surface — 32 bytes each.
    assert_eq!(completed.content_hash.len(), 64);
    assert_eq!(completed.sha256.len(), 64);
    assert_eq!(completed.sha256, hex::encode(sha2::Sha256::digest(body)));

    // A second complete of the same upload is an idempotent replay.
    let replay = port
        .complete_upload(&ctx, owner, &prepared.upload_id)
        .await
        .expect("port complete replay");
    assert!(replay.idempotent_replay);
    assert_eq!(replay.cited_object_id, completed.cited_object_id);

    let read = port
        .read_url(
            &ctx,
            owner,
            Uuid::parse_str(&completed.cited_object_id).expect("cited object id"),
        )
        .await
        .expect("port read url");
    assert!(
        read.read_url
            .contains("response-content-disposition=attachment")
    );
    // The port never exposes bucket/object_key fields; the URL is the
    // only locator handed out.
    assert!(read.expires_at > time::OffsetDateTime::now_utc());

    // An abort after completion reports aborted == false and keeps the blob.
    let aborted = port
        .abort_upload(&ctx, owner, &prepared.upload_id)
        .await
        .expect("port abort");
    assert!(!aborted.aborted);

    drop(pool);
    drop_db(&db_name).await.expect("drop db");
}

/// Insert a `cited_objects` + `cited_uploaded_blob_v1` pair directly, the
/// way an inline `core/uploaded-blob-v1` citation lands one: the locator
/// columns hold whatever the caller asserted, under the caller's own
/// owner (`owner_fixture()` here, the nil personal owner).
async fn forge_uploaded_blob_row(
    pool: &sqlx::PgPool,
    bucket: &str,
    object_key: &str,
    content_hash_seed: u8,
) -> Uuid {
    let cited_object_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxima_core.cited_objects \
            (cited_object_id, schema_id, owner_kind, owner_id, content_hash) \
         VALUES ($1, 'core/uploaded-blob-v1', 'personal', $2, $3)",
    )
    .bind(cited_object_id)
    .bind(Uuid::nil())
    .bind(vec![content_hash_seed; 32])
    .execute(pool)
    .await
    .expect("forged cited_objects row");
    sqlx::query(
        "INSERT INTO proxima_core.cited_uploaded_blob_v1 \
            (cited_object_id, bucket, object_key, sha256, byte_len, mime, filename, etag) \
         VALUES ($1, $2, $3, $4, 1, 'application/pdf', 'forged.pdf', NULL)",
    )
    .bind(cited_object_id)
    .bind(bucket)
    .bind(object_key)
    .bind(vec![content_hash_seed; 32])
    .execute(pool)
    .await
    .expect("forged cited_uploaded_blob_v1 row");
    cited_object_id
}

/// `read_url` presigns only locators the store itself wrote. The locator
/// columns are client-writable (an inline `core/uploaded-blob-v1` citation
/// stores the caller's `bucket`/`object_key` verbatim) and presigning is
/// offline — without the gate, a forged row for the caller's own owner
/// yields a working GET, signed with the substrate's credentials, for any
/// object the substrate's role can read. Forged rows must answer exactly
/// like missing ones.
#[tokio::test]
async fn read_url_refuses_locators_the_store_did_not_write() {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset (run the s3 service to enable)");
        return;
    }

    let (pool, db_name) = fresh_pool().await;
    let config = s3_config_for_dev();
    let store = CitedBlobStore::new(pool.clone(), config.clone()).expect("valid S3 config");
    let owner = owner_fixture();
    let ctx = AuthzContext::single_owner(&owner, AuthPath::System);

    // The baseline answer for a cited object that does not exist at all.
    let missing = store
        .read_url(
            &ctx,
            CitedBlobReadUrlTs {
                owner,
                cited_object_id: Uuid::now_v7().to_string(),
            },
        )
        .await
        .expect_err("missing cited object");

    // (a) a bucket other than the store's; (b) the configured bucket, but a
    // key under some other owner's canonical prefix.
    let foreign_bucket = forge_uploaded_blob_row(
        &pool,
        "not-the-configured-bucket",
        &format!(
            "objects/{}/core/uploaded-blob-v1/{}",
            "ab".repeat(32),
            "cd".repeat(32)
        ),
        1,
    )
    .await;
    let foreign_prefix = forge_uploaded_blob_row(
        &pool,
        &config.bucket,
        &format!(
            "objects/{}/core/uploaded-blob-v1/{}",
            "ab".repeat(32),
            "ef".repeat(32)
        ),
        2,
    )
    .await;

    for (case, forged_id) in [
        ("foreign bucket", foreign_bucket),
        ("foreign owner prefix", foreign_prefix),
    ] {
        let err = store
            .read_url(
                &ctx,
                CitedBlobReadUrlTs {
                    owner,
                    cited_object_id: forged_id.to_string(),
                },
            )
            .await
            .expect_err(&format!("forged locator ({case}) must not presign"));
        assert_eq!(
            err.to_string(),
            missing.to_string(),
            "forged locator ({case}) must be indistinguishable from a missing row"
        );
    }

    drop(pool);
    drop_db(&db_name).await.expect("drop db");
}

/// Once the authz gate has passed for the caller's own owner, the SQL
/// owner predicates in the upload/blob lookups are the only boundary
/// keeping one owner's ids out of another owner's hands. Owner B, under
/// its own authority, must be refused on A's upload and cited-object ids
/// — and must not disturb A's rows in the attempt.
#[tokio::test]
async fn cross_owner_ids_are_refused_under_the_other_owners_authz() {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset (run the s3 service to enable)");
        return;
    }

    let (pool, db_name) = fresh_pool().await;
    let config = s3_config_for_dev();
    let store = CitedBlobStore::new(pool.clone(), config.clone()).expect("valid S3 config");
    let body = b"cross-owner-bytes";
    let owner_a = owner_fixture();
    let ctx_a = AuthzContext::single_owner(&owner_a, AuthPath::System);

    let prepared = store
        .prepare_upload(
            &ctx_a,
            CitedBlobUploadPrepareTs {
                owner: owner_a,
                filename: "a.pdf".into(),
                mime: "application/pdf".into(),
                byte_len: body.len() as u64,
            },
        )
        .await
        .expect("prepare");
    let upload_id = Uuid::parse_str(&prepared.upload_id).expect("upload id");
    let pending: (String,) = sqlx::query_as(
        "SELECT object_key FROM proxima_core.cited_object_uploads WHERE upload_id = $1",
    )
    .bind(upload_id)
    .fetch_one(&pool)
    .await
    .expect("upload row");
    put_object_via_sdk(&config, &pending.0, body).await;
    let completed = store
        .complete_upload(
            &ctx_a,
            CitedBlobUploadCompleteTs {
                owner: owner_a,
                upload_id: prepared.upload_id.clone(),
            },
        )
        .await
        .expect("complete");

    let owner_b = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    let ctx_b = AuthzContext::single_owner(&owner_b, AuthPath::System);

    let err = store
        .complete_upload(
            &ctx_b,
            CitedBlobUploadCompleteTs {
                owner: owner_b,
                upload_id: prepared.upload_id.clone(),
            },
        )
        .await
        .expect_err("A's upload id under B's owner must be refused");
    assert!(matches!(err, BlobError::State(_)), "got {err:?}");

    let err = store
        .abort_upload(
            &ctx_b,
            CitedBlobUploadAbortTs {
                owner: owner_b,
                upload_id: prepared.upload_id.clone(),
            },
        )
        .await
        .expect_err("A's upload id under B's owner must not abort");
    assert!(matches!(err, BlobError::State(_)), "got {err:?}");
    // B's attempts left A's upload row exactly as A completed it.
    let status: (String,) = sqlx::query_as(
        "SELECT status::text FROM proxima_core.cited_object_uploads WHERE upload_id = $1",
    )
    .bind(upload_id)
    .fetch_one(&pool)
    .await
    .expect("status");
    assert_eq!(status.0, "completed");

    let err = store
        .read_url(
            &ctx_b,
            CitedBlobReadUrlTs {
                owner: owner_b,
                cited_object_id: completed.cited_object_id.clone(),
            },
        )
        .await
        .expect_err("A's cited object id under B's owner must be refused");
    assert!(matches!(err, BlobError::State(_)), "got {err:?}");

    // Naming A's owner outright fails the authz gate before any SQL runs.
    let err = store
        .read_url(
            &ctx_b,
            CitedBlobReadUrlTs {
                owner: owner_a,
                cited_object_id: completed.cited_object_id,
            },
        )
        .await
        .expect_err("A's owner under B's authz must be denied");
    assert!(matches!(err, BlobError::Denied(_)), "got {err:?}");

    drop(pool);
    drop_db(&db_name).await.expect("drop db");
}

#[tokio::test]
async fn purge_owner_objects_removes_completed_blob() {
    // Owner erasure must physically remove the
    // owner's S3 objects in-band. Opt-in, needs an S3 target like the roundtrip above.
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset (run the s3 service to enable)");
        return;
    }

    let (pool, db_name) = fresh_pool().await;
    let config = s3_config_for_dev();
    let store = CitedBlobStore::new(pool.clone(), config.clone()).expect("valid S3 config");
    let body = b"ocr-scan-with-pii";
    let owner = owner_fixture();
    let ctx = AuthzContext::single_owner(&owner, AuthPath::System);

    // Upload + complete: leaves a canonical object at objects/<owner_hash>/<blake3>.
    let prepared = store
        .prepare_upload(
            &ctx,
            CitedBlobUploadPrepareTs {
                owner,
                filename: "scan.pdf".into(),
                mime: "application/pdf".into(),
                byte_len: body.len() as u64,
            },
        )
        .await
        .expect("prepare");
    let upload_id = Uuid::parse_str(&prepared.upload_id).expect("upload id");
    let pending: (String, String) = sqlx::query_as(
        "SELECT bucket, object_key FROM proxima_core.cited_object_uploads WHERE upload_id = $1",
    )
    .bind(upload_id)
    .fetch_one(&pool)
    .await
    .expect("upload row");
    put_object_via_sdk(&config, &pending.1, body).await;
    let completed = store
        .complete_upload(
            &ctx,
            CitedBlobUploadCompleteTs {
                owner,
                upload_id: prepared.upload_id,
            },
        )
        .await
        .expect("complete");
    let cited_object_id = Uuid::parse_str(&completed.cited_object_id).expect("cited object id");
    let final_key: (String,) = sqlx::query_as(
        "SELECT object_key FROM proxima_core.cited_uploaded_blob_v1 WHERE cited_object_id = $1",
    )
    .bind(cited_object_id)
    .fetch_one(&pool)
    .await
    .expect("completed blob row");

    let client = s3_client(&config).await;
    assert!(
        client
            .head_object()
            .bucket(&config.bucket)
            .key(&final_key.0)
            .send()
            .await
            .is_ok(),
        "object present in S3 before purge"
    );

    // In-band purge (the port the compliance-erase engine calls on owner erase).
    let deleted = store.purge_owner_objects(owner).await.expect("purge");
    assert!(
        deleted >= 1,
        "purge deleted at least the owner's blob object"
    );

    assert!(
        client
            .head_object()
            .bucket(&config.bucket)
            .key(&final_key.0)
            .send()
            .await
            .is_err(),
        "object physically removed from S3 by the owner purge"
    );

    drop(pool);
    drop_db(&db_name).await.expect("drop db");
}

/// On a VERSIONED bucket (the deployment our runbook recommends), a
/// key-only delete only writes a delete marker and leaves the PII object as a
/// recoverable noncurrent version. The purge must delete by `(key, version_id)`
/// so no version survives an Art. 17 owner erasure.
#[tokio::test]
async fn versioned_bucket_purge_removes_all_object_versions() {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset (run the s3 service to enable)");
        return;
    }

    let (pool, db_name) = fresh_pool().await;
    let base = s3_config_for_dev();
    let config = S3RuntimeConfig {
        bucket: format!("{}-versioned", base.bucket),
        ..base
    };
    let client = s3_client(&config).await;

    // Self-provision a versioned bucket (idempotent across runs).
    let _ = client.create_bucket().bucket(&config.bucket).send().await;
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

    let store = CitedBlobStore::new(pool.clone(), config.clone()).expect("valid S3 config");
    let owner = owner_fixture();
    let ctx = AuthzContext::single_owner(&owner, AuthPath::System);
    let body = b"versioned-pii-bytes";

    let prepared = store
        .prepare_upload(
            &ctx,
            CitedBlobUploadPrepareTs {
                owner,
                filename: "doc.pdf".into(),
                mime: "application/pdf".into(),
                byte_len: u64::try_from(body.len()).unwrap(),
            },
        )
        .await
        .expect("prepare");
    let pending: (String, String) = sqlx::query_as(
        "SELECT bucket, object_key FROM proxima_core.cited_object_uploads WHERE upload_id = $1",
    )
    .bind(Uuid::parse_str(&prepared.upload_id).expect("upload id"))
    .fetch_one(&pool)
    .await
    .expect("upload row");
    put_object_via_sdk(&config, &pending.1, body).await;
    let completed = store
        .complete_upload(
            &ctx,
            CitedBlobUploadCompleteTs {
                owner,
                upload_id: prepared.upload_id,
            },
        )
        .await
        .expect("complete");
    let cited_object_id = Uuid::parse_str(&completed.cited_object_id).expect("cited object id");
    let final_key: (String,) = sqlx::query_as(
        "SELECT object_key FROM proxima_core.cited_uploaded_blob_v1 WHERE cited_object_id = $1",
    )
    .bind(cited_object_id)
    .fetch_one(&pool)
    .await
    .expect("completed blob row");

    // Re-put the same content-addressed key to mint a second version, so a
    // key-only delete would demonstrably leave a noncurrent version behind.
    put_object_via_sdk(&config, &final_key.0, body).await;

    store.purge_owner_objects(owner).await.expect("purge");

    let versions = client
        .list_object_versions()
        .bucket(&config.bucket)
        .prefix(&final_key.0)
        .send()
        .await
        .expect("list object versions");
    assert!(
        versions.versions().is_empty() && versions.delete_markers().is_empty(),
        "versioned purge must remove every object version and delete marker; \
         found {} version(s) / {} marker(s)",
        versions.versions().len(),
        versions.delete_markers().len(),
    );

    drop(pool);
    drop_db(&db_name).await.expect("drop db");
}

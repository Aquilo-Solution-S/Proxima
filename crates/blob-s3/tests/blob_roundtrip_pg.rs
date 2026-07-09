use aws_config::BehaviorVersion;
use aws_sdk_s3::Client;
use aws_sdk_s3::config::Region;
use aws_sdk_s3::primitives::ByteStream;
use proxima_blob_s3::{
    CitedBlobReadUrlTs, CitedBlobStore, CitedBlobUploadCompleteTs, CitedBlobUploadPrepareTs,
    S3RuntimeConfig,
};
use proxima_core::storage_ports::CitedObjectErasePort;
use proxima_core::test_fixtures::owner_fixture;
use proxima_core::{AuthPath, AuthzContext};
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
            .expect("MinIO required: start docker-compose.dev.yml and set PROXIMA_S3_*"),
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
    // Opt-in integration test: needs a reachable S3/MinIO target. Skip
    // (rather than panic) when PROXIMA_S3_* is unset so the default
    // `cargo test --workspace` is green without standing up MinIO.
    if !S3RuntimeConfig::present_in_env() {
        eprintln!(
            "skipped: PROXIMA_S3_* unset (set PROXIMA_S3_BUCKET/REGION + run MinIO to enable)"
        );
        return;
    }

    let (pool, db_name) = fresh_pool().await;
    let config = s3_config_for_dev();
    let store = CitedBlobStore::new(pool.clone(), config.clone());
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

#[tokio::test]
async fn purge_owner_objects_removes_completed_blob() {
    // P1.7 (analysis 2026-07-05): owner erasure must physically remove the
    // owner's S3 objects in-band. Opt-in, needs MinIO like the roundtrip above.
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset (run MinIO to enable)");
        return;
    }

    let (pool, db_name) = fresh_pool().await;
    let config = s3_config_for_dev();
    let store = CitedBlobStore::new(pool.clone(), config.clone());
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

/// P1.7: on a VERSIONED bucket (the deployment our runbook recommends), a
/// key-only delete only writes a delete marker and leaves the PII object as a
/// recoverable noncurrent version. The purge must delete by `(key, version_id)`
/// so no version survives an Art. 17 owner erasure.
#[tokio::test]
async fn versioned_bucket_purge_removes_all_object_versions() {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset (run MinIO to enable)");
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

    let store = CitedBlobStore::new(pool.clone(), config.clone());
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

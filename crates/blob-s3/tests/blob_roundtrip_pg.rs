use aws_config::BehaviorVersion;
use aws_sdk_s3::Client;
use aws_sdk_s3::config::Region;
use aws_sdk_s3::primitives::ByteStream;
use proxima_blob_s3::{
    CitedBlobReadUrlTs, CitedBlobStore, CitedBlobUploadCompleteTs, CitedBlobUploadPrepareTs,
    S3RuntimeConfig,
};
use proxima_core::test_fixtures::owner_fixture;
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
    }
}

async fn put_object_via_sdk(config: &S3RuntimeConfig, key: &str, body: &'static [u8]) {
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
    let client = Client::from_conf(builder.build());
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

    let prepared = store
        .prepare_upload(CitedBlobUploadPrepareTs {
            owner,
            filename: "test.pdf".into(),
            mime: "application/pdf".into(),
            byte_len: body.len() as u64,
        })
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
        .complete_upload(CitedBlobUploadCompleteTs {
            owner,
            upload_id: prepared.upload_id,
        })
        .await
        .expect("complete");
    let url = store
        .read_url(CitedBlobReadUrlTs {
            owner,
            cited_object_id: completed.cited_object_id.clone(),
        })
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

    drop(pool);
    drop_db(&db_name).await.expect("drop db");
}

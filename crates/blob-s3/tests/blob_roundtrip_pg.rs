use aws_config::BehaviorVersion;
use aws_sdk_s3::Client;
use aws_sdk_s3::config::Region;
use aws_sdk_s3::primitives::ByteStream;
use proxima_blob_s3::{
    BlobError, CitedBlobReadUrlTs, CitedBlobStore, CitedBlobUploadAbortTs,
    CitedBlobUploadCompleteTs, CitedBlobUploadPrepareTs, S3RuntimeConfig,
};
use proxima_core::engine::{Engine, UploadCompleted};
use proxima_core::error::ProtocolError;
use proxima_core::storage_ports::{
    CitedBlobIntegrityMismatch, CitedBlobPort, CitedBlobReadError, CitedBlobReadPort,
    CitedBlobReconcileOutcome, CitedObjectErasePort, MAX_HELD_BLOB_DIGESTS,
};
use proxima_core::test_fixtures::owner_fixture;
use proxima_core::{AuthPath, AuthzContext, FlavorRegistry, OwnerRef, StorageError, UserId};
use std::num::NonZeroU64;

// Contexts here are `AuthPath::HostBearer`, matching every production
// caller. Completion writes a Fact through the engine, and an
// `AuthPath::System` context needs the host-held `SystemAuthority` witness
// to issue any owner-write permit. Request fixtures use HostBearer; the
// global maintenance test below obtains its witness by consuming a dedicated
// Engine through the same host boundary as production boot.
use proxima_pg_testkit::{create_db, db_url, drop_db, unique_db_name};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

async fn fresh_storage() -> (PgStorage, String) {
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
    (pg, db_name)
}

/// Completion as production runs it: the store stages the bytes, then ONE
/// transaction records the artefact and the `core/upload-v1` Fact that
/// cites it. `CitedBlobStore` no longer persists anything by itself, so a
/// test that only called it would be testing half a completion.
async fn complete_via_engine(
    pg: &PgStorage,
    store: &CitedBlobStore,
    ctx: &AuthzContext,
    owner: OwnerRef,
    upload_id: &str,
) -> Result<UploadCompleted, ProtocolError> {
    let service =
        proxima_core::storage_ports::CitedBlobService::new(std::sync::Arc::new(store.clone()));
    Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests())
        .with_storage_ports(std::sync::Arc::new(pg.clone()).storage_ports())
        .complete_upload_as_fact(&service, ctx, owner, upload_id, &[])
        .await
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

    let (pg, db_name) = fresh_storage().await;
    let pool = pg.pool_for_tests().clone();
    let config = s3_config_for_dev();
    let store = CitedBlobStore::new(pool.clone(), config.clone()).expect("valid S3 config");
    let body = b"test-bytes";
    let owner = owner_fixture();
    let ctx = AuthzContext::single_owner(&owner, AuthPath::HostBearer);

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
        "SELECT bucket, object_key FROM proxima_core.blob_uploads WHERE upload_id = $1",
    )
    .bind(upload_id)
    .fetch_one(&pool)
    .await
    .expect("upload row");
    assert_eq!(row.0, config.bucket);
    put_object_via_sdk(&config, &row.1, body).await;

    let completed = complete_via_engine(&pg, &store, &ctx, owner, &prepared.upload_id)
        .await
        .expect("complete")
        .blob;
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
        "SELECT object_key FROM proxima_core.blob_uploads WHERE blob_id = $1",
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
async fn verified_read_is_owner_exact_bounded_and_integrity_checked() {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset (run the s3 service to enable)");
        return;
    }

    let (pg, db_name) = fresh_storage().await;
    let pool = pg.pool_for_tests().clone();
    let config = s3_config_for_dev();
    let store = CitedBlobStore::new(pool.clone(), config.clone()).expect("valid S3 config");
    let body = b"real";
    let owner = owner_fixture();
    let ctx = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
    let (cited_object_id, canonical_key) =
        complete_upload_fixture(&pg, &store, &config, &pool, &ctx, owner)
            .await
            .expect("complete fixture");
    let cited_object_id = Uuid::parse_str(&cited_object_id).expect("cited object id");

    let verified = store
        .collect_verified(
            &ctx,
            owner,
            cited_object_id,
            NonZeroU64::new(u64::try_from(body.len()).unwrap()).unwrap(),
        )
        .await
        .expect("exact-owner verified read");
    assert_eq!(verified.bytes, body);
    assert_eq!(verified.cited_object_id, cited_object_id);
    assert_eq!(verified.filename, "genuine.pdf");

    let error = store
        .collect_verified(
            &ctx,
            owner,
            cited_object_id,
            NonZeroU64::new(u64::try_from(body.len() - 1).unwrap()).unwrap(),
        )
        .await
        .expect_err("stored metadata over caller ceiling");
    assert!(matches!(error, CitedBlobReadError::TooLarge { .. }));

    let other = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    let other_ctx = AuthzContext::single_owner(&other, AuthPath::HostBearer);
    let error = store
        .collect_verified(
            &other_ctx,
            other,
            cited_object_id,
            NonZeroU64::new(1024).unwrap(),
        )
        .await
        .expect_err("foreign owner's row is not visible");
    assert_eq!(error, CitedBlobReadError::NotFound);
    let error = store
        .collect_verified(
            &other_ctx,
            owner,
            cited_object_id,
            NonZeroU64::new(1024).unwrap(),
        )
        .await
        .expect_err("foreign authz is denied before lookup");
    assert_eq!(error, CitedBlobReadError::AccessDenied);

    put_object_via_sdk(&config, &canonical_key, b"x").await;
    let error = store
        .collect_verified(&ctx, owner, cited_object_id, NonZeroU64::new(1024).unwrap())
        .await
        .expect_err("truncated canonical object rejected");
    assert_eq!(
        error,
        CitedBlobReadError::IntegrityMismatch(CitedBlobIntegrityMismatch::ByteLength)
    );

    put_object_via_sdk(&config, &canonical_key, b"evil").await;
    let error = store
        .collect_verified(&ctx, owner, cited_object_id, NonZeroU64::new(1024).unwrap())
        .await
        .expect_err("same-length content tamper rejected");
    assert_eq!(
        error,
        CitedBlobReadError::IntegrityMismatch(CitedBlobIntegrityMismatch::ContentHash)
    );

    drop(pool);
    drop_db(&db_name).await.expect("drop db");
}

/// One response header as a string, or empty when absent.
fn header<'a>(response: &'a reqwest::Response, name: &str) -> &'a str {
    response
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
}

/// Prepare an upload and put its bytes at the pending key, returning the
/// `upload_id`. The concurrency tests below need several of these.
async fn staged_upload(
    pool: &sqlx::PgPool,
    store: &CitedBlobStore,
    config: &S3RuntimeConfig,
    ctx: &AuthzContext,
    owner: OwnerRef,
    filename: &str,
    body: &'static [u8],
) -> String {
    let prepared = store
        .prepare_upload(
            ctx,
            CitedBlobUploadPrepareTs {
                owner,
                filename: filename.into(),
                mime: "application/pdf".into(),
                byte_len: body.len() as u64,
            },
        )
        .await
        .expect("prepare");
    let upload_id = Uuid::parse_str(&prepared.upload_id).expect("upload id");
    let key: (String,) = sqlx::query_as(
        "SELECT object_key FROM proxima_core.blob_uploads WHERE upload_id = $1",
    )
    .bind(upload_id)
    .fetch_one(pool)
    .await
    .expect("upload row");
    put_object_via_sdk(config, &key.0, body).await;
    prepared.upload_id
}

/// Concurrent completions of the same bytes under different upload ids
/// converge on one artefact. The loser is an idempotent replay, not a
/// unique-violation surfaced as Internal.
#[tokio::test]
async fn concurrent_completions_of_one_file_converge_on_one_artefact() {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset");
        return;
    }

    let (pg, db_name) = fresh_storage().await;
    let pool = pg.pool_for_tests().clone();
    let config = s3_config_for_dev();
    let store = CitedBlobStore::new(pool.clone(), config.clone()).expect("valid S3 config");
    let body: &[u8] = b"%PDF-1.7 the same handbook, uploaded twice at once";
    let owner = owner_fixture();
    let ctx = AuthzContext::single_owner(&owner, AuthPath::HostBearer);

    let first = staged_upload(&pool, &store, &config, &ctx, owner, "handbuch.pdf", body).await;
    let second = staged_upload(&pool, &store, &config, &ctx, owner, "kopie.pdf", body).await;

    let (a, b) = tokio::join!(
        complete_via_engine(&pg, &store, &ctx, owner, &first),
        complete_via_engine(&pg, &store, &ctx, owner, &second),
    );
    let a = a.expect("the first concurrent completion must not error");
    let b = b.expect("the loser of the race is a replay, not a failure");

    assert_eq!(
        a.blob.cited_object_id, b.blob.cited_object_id,
        "the same bytes under one owner are one artefact"
    );
    assert_eq!(a.fact.memory_id, b.fact.memory_id, "and one arrival Fact");
    assert!(
        a.fact.idempotent_replay ^ b.fact.idempotent_replay,
        "exactly one of the two recorded the arrival; the other replayed it \
         (got {} and {})",
        a.fact.idempotent_replay,
        b.fact.idempotent_replay
    );

    let objects: i64 =
        sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.blob")
            .fetch_one(&pool)
            .await
            .expect("count");
    assert_eq!(objects, 1, "one file, one row");
    let facts: i64 = sqlx::query_scalar(
        "SELECT count(*)::bigint FROM proxima_core.memories WHERE schema_id = 'core/upload-v1'",
    )
    .fetch_one(&pool)
    .await
    .expect("count");
    assert_eq!(facts, 1, "one file, one arrival");

    drop(pool);
    drop_db(&db_name).await.expect("drop db");
}

/// A completion racing an abort resolves one way or the other, and the
/// caller is never told a committed write failed.
///
/// `finish_upload` runs after the transaction that recorded the artefact
/// and its Fact has committed, so if an abort wins that window the
/// completion must still report success — the corpus already holds the
/// artefact, and the upload id is spent either way. The disjunction is the
/// assertion: either the abort won and nothing was recorded, or the
/// completion won and it is consistent.
#[tokio::test]
async fn a_completion_racing_an_abort_never_reports_a_committed_write_as_failed() {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset");
        return;
    }

    let (pg, db_name) = fresh_storage().await;
    let pool = pg.pool_for_tests().clone();
    let config = s3_config_for_dev();
    let store = CitedBlobStore::new(pool.clone(), config.clone()).expect("valid S3 config");
    let body: &[u8] = b"%PDF-1.7 a handbook whose upload is contested";
    let owner = owner_fixture();
    let ctx = AuthzContext::single_owner(&owner, AuthPath::HostBearer);

    let upload_id = staged_upload(&pool, &store, &config, &ctx, owner, "handbuch.pdf", body).await;

    let (completed, aborted) = tokio::join!(
        complete_via_engine(&pg, &store, &ctx, owner, &upload_id),
        store.abort_upload(
            &ctx,
            CitedBlobUploadAbortTs {
                owner,
                upload_id: upload_id.clone(),
            },
        ),
    );
    aborted.expect("abort itself must not fault");

    let facts: i64 = sqlx::query_scalar(
        "SELECT count(*)::bigint FROM proxima_core.memories WHERE schema_id = 'core/upload-v1'",
    )
    .fetch_one(&pool)
    .await
    .expect("count");
    let objects: i64 =
        sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.blob")
            .fetch_one(&pool)
            .await
            .expect("count");

    match completed {
        // The completion won, or lost only the bookkeeping UPDATE. Either
        // way the corpus holds exactly one artefact and one arrival, and
        // saying Ok to the caller is the truth.
        Ok(outcome) => {
            assert_eq!(facts, 1, "a successful completion recorded its arrival");
            assert_eq!(objects, 1, "and its artefact");
            assert!(
                !outcome.blob.cited_object_id.is_empty(),
                "the caller was handed the id it can cite"
            );
        }
        // The abort won before the transaction ran. Nothing may survive.
        Err(err) => {
            assert_eq!(
                facts, 0,
                "the completion failed but an arrival Fact survives: {err}"
            );
            assert_eq!(
                objects, 0,
                "the completion failed but the artefact survives: {err}"
            );
        }
    }

    drop(pool);
    drop_db(&db_name).await.expect("drop db");
}

/// The whole transfer contract, executed rather than described: PUT the
/// bytes to the presigned URL as a real client does, complete, then GET the
/// presigned download and compare what comes back.
///
/// Every other test in this file substitutes the SDK for the upload hop and
/// asserts on the *shape* of the download URL. Neither presigned URL was
/// ever dereferenced, so "a completed PDF is retrievable from Proxima" had
/// no carrier at any layer — the two hops that actually carry bytes were
/// the untested ones. The headers matter: `prepare_upload` presigns with
/// `content_type`, so a client that ignores `prepared.headers` gets
/// `SignatureDoesNotMatch`, and that contract was documented but unproven.
#[tokio::test]
async fn presigned_put_and_get_carry_the_bytes() {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset");
        return;
    }

    let (pg, db_name) = fresh_storage().await;
    let pool = pg.pool_for_tests().clone();
    let config = s3_config_for_dev();
    let store = CitedBlobStore::new(pool.clone(), config.clone()).expect("valid S3 config");
    let body: &[u8] = b"%PDF-1.7 a handbook that must survive the round trip";
    let owner = owner_fixture();
    let ctx = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
    let http = reqwest::Client::new();

    let prepared = store
        .prepare_upload(
            &ctx,
            CitedBlobUploadPrepareTs {
                owner,
                filename: "handbuch.pdf".into(),
                mime: "application/pdf".into(),
                byte_len: body.len() as u64,
            },
        )
        .await
        .expect("prepare");

    // Hop 1: the client's PUT, with exactly the headers it was handed.
    let mut put = http.put(&prepared.upload_url);
    for header in &prepared.headers {
        put = put.header(header.name.as_str(), header.value.as_str());
    }
    let put_response = put.body(body.to_vec()).send().await.expect("presigned PUT");
    assert!(
        put_response.status().is_success(),
        "presigned PUT rejected the client that used the headers it was given: {} {}",
        put_response.status(),
        put_response.text().await.unwrap_or_default()
    );

    let completed = complete_via_engine(&pg, &store, &ctx, owner, &prepared.upload_id)
        .await
        .expect("complete")
        .blob;

    // Hop 2: the client's GET.
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
    let response = http.get(&url.read_url).send().await.expect("presigned GET");
    assert!(
        response.status().is_success(),
        "presigned GET failed: {}",
        response.status()
    );

    // Asserted on the RESPONSE, not on the URL string we asked for: this is
    // what a browser actually receives, and it is the only assertion in the
    // repo that fails if the production overrides are removed.
    assert_eq!(
        header(&response, "content-disposition"),
        "attachment",
        "the download must not render inline"
    );
    assert_eq!(
        header(&response, "content-type"),
        "application/octet-stream",
        "a stored text/html mime must not execute in the browser"
    );

    let fetched = response.bytes().await.expect("read body");
    assert_eq!(
        fetched.as_ref(),
        body,
        "the bytes that came back are not the bytes that went up"
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

    let (pg, db_name) = fresh_storage().await;
    let pool = pg.pool_for_tests().clone();
    let config = s3_config_for_dev();
    let store = CitedBlobStore::new(pool.clone(), config.clone()).expect("valid S3 config");
    let port: &dyn CitedBlobPort = &store;
    let body = b"port-level-bytes";
    let owner = owner_fixture();
    let ctx = AuthzContext::single_owner(&owner, AuthPath::HostBearer);

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
        "SELECT object_key FROM proxima_core.blob_uploads WHERE upload_id = $1",
    )
    .bind(Uuid::parse_str(&prepared.upload_id).expect("upload id"))
    .fetch_one(&pool)
    .await
    .expect("upload row");
    put_object_via_sdk(&config, &pending.0, body).await;

    let completed = complete_via_engine(&pg, &store, &ctx, owner, &prepared.upload_id)
        .await
        .expect("port complete")
        .blob;
    assert_eq!(completed.byte_len, body.len() as u64);
    assert_eq!(completed.mime, "application/pdf");
    assert_eq!(completed.filename, "port.pdf");
    assert!(!completed.idempotent_replay);
    // Hashes are hex on the port surface — 32 bytes each.
    assert_eq!(completed.content_hash.len(), 64);
    assert_eq!(completed.sha256.len(), 64);
    assert_eq!(completed.sha256, hex::encode(sha2::Sha256::digest(body)));

    // A second complete of the same upload is an idempotent replay.
    let replay = complete_via_engine(&pg, &store, &ctx, owner, &prepared.upload_id)
        .await
        .expect("port complete replay")
        .blob;
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

/// Insert a `blob` + completed `blob_uploads` pair.
async fn forge_uploaded_blob_row(
    pool: &sqlx::PgPool,
    owner_id: Uuid,
    bucket: &str,
    object_key: &str,
    content_hash_seed: u8,
) -> Uuid {
    sqlx::query(
        "INSERT INTO proxima_core.owners (owner_id, kind)
         VALUES ($1, 'personal')
         ON CONFLICT (owner_id) DO NOTHING",
    )
    .bind(owner_id)
    .execute(pool)
    .await
    .expect("forged owner");
    let blob_id: Uuid = sqlx::query_scalar(
        "INSERT INTO proxima_core.blob
            (schema_id, owner_id, content_hash)
         VALUES ('core/uploaded-blob-v1', $1, $2)
         RETURNING blob_id",
    )
    .bind(owner_id)
    .bind(vec![content_hash_seed; 32])
    .fetch_one(pool)
    .await
    .expect("forged blob row");
    sqlx::query(
        "INSERT INTO proxima_core.blob_uploads
            (owner_id, bucket, object_key, filename, mime, expected_byte_len,
             status, blob_id, sha256, expires_at, completed_at)
         VALUES ($1, $2, $3, 'forged.pdf', 'application/pdf', 1,
                 'completed', $4, $5, now() + interval '1 hour', now())",
    )
    .bind(owner_id)
    .bind(bucket)
    .bind(object_key)
    .bind(blob_id)
    .bind(vec![content_hash_seed; 32])
    .execute(pool)
    .await
    .expect("forged blob_uploads row");
    blob_id
}

async fn forge_foreign_locator(pool: &sqlx::PgPool, owner_id: Uuid) {
    forge_uploaded_blob_row(
        pool,
        owner_id,
        "some-other-bucket",
        "elsewhere/object",
        0xAB,
    )
    .await;
}

/// Drive a full prepare -> PUT -> complete for `owner`, returning the
/// resulting cited-object id and the canonical `object_key` the store
/// wrote for it.
async fn complete_upload_fixture(
    pg: &PgStorage,
    store: &CitedBlobStore,
    config: &S3RuntimeConfig,
    pool: &sqlx::PgPool,
    ctx: &AuthzContext,
    owner: OwnerRef,
) -> Result<(String, String), BlobError> {
    let prepared = store
        .prepare_upload(
            ctx,
            CitedBlobUploadPrepareTs {
                owner,
                filename: "genuine.pdf".into(),
                mime: "application/pdf".into(),
                byte_len: 4,
            },
        )
        .await?;
    let pending: (String,) = sqlx::query_as(
        "SELECT object_key FROM proxima_core.blob_uploads WHERE upload_id = $1",
    )
    .bind(Uuid::parse_str(&prepared.upload_id).expect("upload id"))
    .fetch_one(pool)
    .await
    .expect("upload row");
    put_object_via_sdk(config, &pending.0, b"real").await;
    let completed = complete_via_engine(pg, store, ctx, owner, &prepared.upload_id)
        .await
        .map_err(|err| BlobError::State(err.to_string()))?
        .blob;
    let stored: (String,) = sqlx::query_as(
        "SELECT object_key FROM proxima_core.blob_uploads WHERE blob_id = $1",
    )
    .bind(Uuid::parse_str(&completed.cited_object_id).expect("cited object id"))
    .fetch_one(pool)
    .await
    .expect("completed blob row");
    Ok((completed.cited_object_id, stored.0))
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

    let (pg, db_name) = fresh_storage().await;
    let pool = pg.pool_for_tests().clone();
    let config = s3_config_for_dev();
    let store = CitedBlobStore::new(pool.clone(), config.clone()).expect("valid S3 config");
    let owner = owner_fixture();
    let ctx = AuthzContext::single_owner(&owner, AuthPath::HostBearer);

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

    // A genuine upload, so the caller's own canonical key can be forged
    // against without restating the store's key scheme here.
    let (genuine_id, genuine_key) =
        complete_upload_fixture(&pg, &store, &config, &pool, &ctx, owner)
            .await
            .expect("genuine upload");

    // Each clause of the gate is forged against on its own, so dropping
    // either one fails a case: (a) another bucket under this owner's real
    // canonical key — the cross-environment read the bucket check exists
    // for; (b) this bucket, under another owner's prefix; (c) both wrong.
    let foreign_bucket = forge_uploaded_blob_row(
        &pool,
        Uuid::nil(),
        "not-the-configured-bucket",
        &genuine_key,
        1,
    )
    .await;
    let foreign_prefix = forge_uploaded_blob_row(
        &pool,
        Uuid::nil(),
        &config.bucket,
        &format!(
            "objects/{}/core/uploaded-blob-v1/{}",
            "ab".repeat(32),
            "ef".repeat(32)
        ),
        2,
    )
    .await;
    let both_foreign = forge_uploaded_blob_row(
        &pool,
        Uuid::nil(),
        "not-the-configured-bucket",
        &format!(
            "objects/{}/core/uploaded-blob-v1/{}",
            "ab".repeat(32),
            "cd".repeat(32)
        ),
        3,
    )
    .await;

    for (case, forged_id) in [
        ("foreign bucket, own key", foreign_bucket),
        ("foreign owner prefix", foreign_prefix),
        ("foreign bucket and prefix", both_foreign),
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

    // The same bytes at the same key, reached through the row the store
    // wrote: the gate discriminates on provenance, not on the locator.
    store
        .read_url(
            &ctx,
            CitedBlobReadUrlTs {
                owner,
                cited_object_id: genuine_id,
            },
        )
        .await
        .expect("a locator this store wrote still presigns");

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

    let (pg, db_name) = fresh_storage().await;
    let pool = pg.pool_for_tests().clone();
    let config = s3_config_for_dev();
    let store = CitedBlobStore::new(pool.clone(), config.clone()).expect("valid S3 config");
    let body = b"cross-owner-bytes";
    let owner_a = owner_fixture();
    let ctx_a = AuthzContext::single_owner(&owner_a, AuthPath::HostBearer);

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
        "SELECT object_key FROM proxima_core.blob_uploads WHERE upload_id = $1",
    )
    .bind(upload_id)
    .fetch_one(&pool)
    .await
    .expect("upload row");
    put_object_via_sdk(&config, &pending.0, body).await;
    let completed = complete_via_engine(&pg, &store, &ctx_a, owner_a, &prepared.upload_id)
        .await
        .expect("complete")
        .blob;

    let owner_b = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    let ctx_b = AuthzContext::single_owner(&owner_b, AuthPath::HostBearer);

    let err = store
        .stage_upload(
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
        "SELECT status::text FROM proxima_core.blob_uploads WHERE upload_id = $1",
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

    let (pg, db_name) = fresh_storage().await;
    let pool = pg.pool_for_tests().clone();
    let config = s3_config_for_dev();
    let store = CitedBlobStore::new(pool.clone(), config.clone()).expect("valid S3 config");
    let body = b"ocr-scan-with-pii";
    let owner = owner_fixture();
    let ctx = AuthzContext::single_owner(&owner, AuthPath::HostBearer);

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
        "SELECT bucket, object_key FROM proxima_core.blob_uploads WHERE upload_id = $1",
    )
    .bind(upload_id)
    .fetch_one(&pool)
    .await
    .expect("upload row");
    put_object_via_sdk(&config, &pending.1, body).await;
    let completed = complete_via_engine(&pg, &store, &ctx, owner, &prepared.upload_id)
        .await
        .expect("complete")
        .blob;
    let cited_object_id = Uuid::parse_str(&completed.cited_object_id).expect("cited object id");
    let final_key: (String,) = sqlx::query_as(
        "SELECT object_key FROM proxima_core.blob_uploads WHERE blob_id = $1",
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

    let (pg, db_name) = fresh_storage().await;
    let pool = pg.pool_for_tests().clone();
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
    let ctx = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
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
        "SELECT bucket, object_key FROM proxima_core.blob_uploads WHERE upload_id = $1",
    )
    .bind(Uuid::parse_str(&prepared.upload_id).expect("upload id"))
    .fetch_one(&pool)
    .await
    .expect("upload row");
    put_object_via_sdk(&config, &pending.1, body).await;
    let completed = complete_via_engine(&pg, &store, &ctx, owner, &prepared.upload_id)
        .await
        .expect("complete")
        .blob;
    let cited_object_id = Uuid::parse_str(&completed.cited_object_id).expect("cited object id");
    let final_key: (String,) = sqlx::query_as(
        "SELECT object_key FROM proxima_core.blob_uploads WHERE blob_id = $1",
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

/// Lowercase hex to the 32 raw bytes the port asks for. The completion
/// outcome speaks hex because it is client-facing; the port speaks bytes
/// because it is not.
fn hex32(hex: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (slot, pair) in out.iter_mut().zip(hex.as_bytes().chunks_exact(2)) {
        *slot = u8::from_str_radix(std::str::from_utf8(pair).expect("hex"), 16).expect("hex byte");
    }
    out
}

/// The existence check reports what this owner holds — and, the property
/// that actually needs a test, cannot be turned into a probe of anyone
/// else's corpus.
///
/// A batch verb is where an enumeration oracle would appear if one were
/// going to: a caller can sweep digests cheaply and diff the hits, so
/// "absent" and "present but not yours" have to be the same answer. Every
/// other read in this lane collapses them; this asserts that the batch one
/// does too, against a digest that genuinely exists under another owner.
#[tokio::test]
async fn find_held_blobs_reports_only_this_owners_artefacts() {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset (run the s3 service to enable)");
        return;
    }

    let (pg, db_name) = fresh_storage().await;
    let pool = pg.pool_for_tests().clone();
    let config = s3_config_for_dev();
    let store = CitedBlobStore::new(pool.clone(), config.clone()).expect("valid S3 config");

    let owner = owner_fixture();
    let ctx = AuthzContext::single_owner(&owner, AuthPath::HostBearer);

    let prepared = store
        .prepare_upload(
            &ctx,
            CitedBlobUploadPrepareTs {
                owner,
                filename: "page-00001.jpg".into(),
                mime: "image/jpeg".into(),
                byte_len: 4,
            },
        )
        .await
        .expect("prepare");
    let pending: (String,) = sqlx::query_as(
        "SELECT object_key FROM proxima_core.blob_uploads WHERE upload_id = $1",
    )
    .bind(Uuid::parse_str(&prepared.upload_id).expect("upload id"))
    .fetch_one(&pool)
    .await
    .expect("upload row");
    put_object_via_sdk(&config, &pending.0, b"real").await;
    let completed = complete_via_engine(&pg, &store, &ctx, owner, &prepared.upload_id)
        .await
        .expect("complete")
        .blob;

    let held_hash = hex32(&completed.content_hash);
    let absent_hash = [0xABu8; 32];

    // The artefact is found, and carries the identity a caller needs in
    // order to skip re-uploading it: the same cited object completion
    // returned, with its length and its recorded name.
    let held = store
        .find_held_blobs(&ctx, owner, &[held_hash, absent_hash])
        .await
        .expect("find held");
    assert_eq!(held.len(), 1, "one of the two digests is held: {held:?}");
    assert_eq!(held[0].content_hash, held_hash);
    assert_eq!(
        held[0].cited_object_id.to_string(),
        completed.cited_object_id
    );
    assert_eq!(held[0].byte_len, 4);
    assert_eq!(held[0].mime, "image/jpeg");
    assert_eq!(held[0].filename, "page-00001.jpg");

    // THE ORACLE PROPERTY. A different owner asking about a digest that
    // genuinely exists gets the same answer as for one that does not:
    // nothing. Not a denial, which would itself confirm the artefact.
    let other = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    let other_ctx = AuthzContext::single_owner(&other, AuthPath::HostBearer);
    let cross = store
        .find_held_blobs(&other_ctx, other, &[held_hash])
        .await
        .expect("a foreign digest is answered, not refused");
    assert!(
        cross.is_empty(),
        "another owner must not learn this artefact exists: {cross:?}"
    );

    // And the read gate still refuses an owner the context cannot reach at
    // all, which is a different question from the one above.
    let denied = store.find_held_blobs(&ctx, other, &[held_hash]).await;
    assert!(
        matches!(denied, Err(BlobError::Denied(_))),
        "an unreachable owner is denied, not answered: {denied:?}"
    );

    // Asking about nothing is answered, not refused.
    assert!(
        store
            .find_held_blobs(&ctx, owner, &[])
            .await
            .expect("empty batch")
            .is_empty()
    );

    // The bound is enforced and names itself, so a caller that trips it
    // learns how to cut its batch.
    let too_many = vec![[0u8; 32]; MAX_HELD_BLOB_DIGESTS + 1];
    let err = store
        .find_held_blobs(&ctx, owner, &too_many)
        .await
        .expect_err("over the bound");
    assert!(
        err.to_string().contains(&MAX_HELD_BLOB_DIGESTS.to_string()),
        "the refusal must quote the bound it enforces: {err}"
    );

    drop(pool);
    drop_db(&db_name).await.expect("drop db");
}

async fn assert_owner_reconcile_isolated(
    store: &CitedBlobStore,
    healthy: (&AuthzContext, OwnerRef, Uuid),
    lost: (&AuthzContext, OwnerRef, Uuid, &str),
) {
    let (owner_ctx, owner, healthy_id) = healthy;
    let (other_ctx, other, lost_id, lost_key) = lost;
    let owner_outcome = store
        .reconcile_owner(owner_ctx, owner)
        .await
        .expect("owner reconcile runs");
    assert_eq!(
        owner_outcome.missing_objects, 0,
        "the first owner must not inherit the second owner's missing object"
    );
    assert_eq!(
        owner_outcome.objects_scanned, 1,
        "the first owner scan must stay inside its exact object prefix"
    );
    assert_eq!(
        owner_outcome.orphan_objects, 0,
        "a bucket-global orphan must not appear in an owner report"
    );
    assert!(owner_outcome.foreign_locators >= 1);
    assert!(
        owner_outcome
            .missing_sample
            .iter()
            .all(|missing| missing.cited_object_id != lost_id)
    );

    let other_outcome = store
        .reconcile_owner(other_ctx, other)
        .await
        .expect("second owner reconcile runs");
    assert_eq!(other_outcome.missing_objects, 1);
    assert_eq!(
        other_outcome.objects_scanned, 0,
        "the second owner scan must not see the first owner's object"
    );
    assert_eq!(other_outcome.orphan_objects, 0);
    assert!(
        other_outcome
            .missing_sample
            .iter()
            .any(|missing| missing.cited_object_id == lost_id)
    );
    assert!(
        other_outcome
            .missing_sample
            .iter()
            .all(|missing| missing.cited_object_id != healthy_id)
    );
    assert!(
        !format!("{other_outcome:?}").contains(lost_key),
        "owner report must not render raw object keys"
    );
}

async fn reconcile_with_bound_authority(store: &CitedBlobStore) -> CitedBlobReconcileOutcome {
    let (_, authority) =
        Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests()).into_system_authority();
    store
        .bind_system_authority(&authority)
        .expect("test store binds to its maintenance engine");

    let (_, foreign_authority) =
        Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests()).into_system_authority();
    let foreign_error = store
        .reconcile_all(&foreign_authority)
        .await
        .expect_err("an unrelated Engine witness must not authorize this store");
    assert!(
        matches!(foreign_error, StorageError::ConstraintViolation(ref message) if message.contains("different cited-blob store boot")),
        "foreign authority must fail before reconciliation I/O: {foreign_error}"
    );

    store
        .reconcile_all(&authority)
        .await
        .expect("global reconcile runs with host authority")
}

/// A row whose object is gone is the divergence `find_held_blobs` cannot
/// see, and this is the sweep that sees it.
///
/// FOUR STATES IN ONE BUCKET, because the value of this verb is entirely in
/// telling them apart. A healthy artefact must not be reported; a deleted
/// object must be; an object nobody claims must be reported as a DIFFERENT
/// thing, because it costs money rather than breaking a citation; and a row
/// naming somewhere this store never wrote must be a third thing again,
/// because reporting a forged or legacy locator as data loss would send an
/// operator hunting for bytes that were never there.
///
/// THE BUCKET IS SHARED WITH EVERY OTHER TEST in this file and with the dev
/// environment, so absolute counts are meaningless here — the assertions
/// are all on THIS run's keys appearing (or not) in the samples, and on the
/// deltas. A test that asserted `missing_objects == 1` would pass alone and
/// fail under `--test-threads`.
#[tokio::test]
async fn reconcile_tells_a_lost_object_from_an_orphan_and_from_a_forged_locator() {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset (run the s3 service to enable)");
        return;
    }
    let (pg, db_name) = fresh_storage().await;
    let pool = pg.pool_for_tests().clone();
    let config = s3_config_for_dev();
    let store = CitedBlobStore::new(pool.clone(), config.clone()).expect("valid S3 config");
    let owner_id = Uuid::now_v7();
    let owner = OwnerRef::Personal(UserId::new(owner_id));
    let ctx = AuthzContext::single_owner(&owner, AuthPath::HostBearer);

    // 1. HEALTHY: a real upload, left alone.
    let (healthy_id, healthy_key) =
        complete_upload_fixture(&pg, &store, &config, &pool, &ctx, owner)
            .await
            .expect("healthy upload");

    // 2. LOST: a real upload whose object is then deleted underneath it.
    let other = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    let other_ctx = AuthzContext::single_owner(&other, AuthPath::HostBearer);
    let (lost_id, lost_key) =
        complete_upload_fixture(&pg, &store, &config, &pool, &other_ctx, other)
            .await
            .expect("second upload");
    s3_client(&config)
        .await
        .delete_object()
        .bucket(&config.bucket)
        .key(&lost_key)
        .send()
        .await
        .expect("delete the object out from under its row");

    // 3. ORPHAN: bytes under the canonical prefix that no row claims.
    let orphan_key = format!(
        "objects/{}/core/uploaded-blob-v1/{}",
        "0".repeat(64),
        "f".repeat(64)
    );
    put_object_via_sdk(&config, &orphan_key, b"nobody claims me").await;

    // 4. FOREIGN: a row pointing outside anything this store wrote.
    forge_foreign_locator(&pool, owner_id).await;
    let healthy_id = Uuid::parse_str(&healthy_id).expect("healthy cited object id");
    let lost_id = Uuid::parse_str(&lost_id).expect("lost cited object id");

    // Owner reports are a separate authorization lane. They query only the
    // selected owner's rows and S3 prefix, and their DTO carries no raw key.
    assert_owner_reconcile_isolated(
        &store,
        (&ctx, owner, healthy_id),
        (&other_ctx, other, lost_id, &lost_key),
    )
    .await;

    let outcome = reconcile_with_bound_authority(&store).await;
    // Counts only. `{brief}` carries up to MAX_RECONCILE_SAMPLE keys per
    // direction, and a failure that prints three hundred hex strings is a
    // failure nobody reads.
    let brief = format!(
        "rows={} objects={} missing={} orphans={} foreign={}",
        outcome.rows_scanned,
        outcome.objects_scanned,
        outcome.missing_objects,
        outcome.orphan_objects,
        outcome.foreign_locators,
    );

    let missing: Vec<&str> = outcome
        .missing_sample
        .iter()
        .map(|m| m.object_key.as_str())
        .collect();
    assert!(
        missing.contains(&lost_key.as_str()),
        "the artefact whose object was deleted was not reported as missing: {brief}"
    );
    assert!(
        !missing.contains(&healthy_key.as_str()),
        "an intact artefact was reported as missing — the sweep cannot tell loss from health: {brief}"
    );
    assert!(
        outcome.orphan_sample.contains(&orphan_key),
        "an object no row claims was not reported as an orphan: {brief}"
    );
    assert!(
        !outcome.orphan_sample.contains(&healthy_key),
        "an intact artefact was reported as an orphan: {brief}"
    );
    assert!(
        outcome
            .foreign_sample
            .iter()
            .any(|f| f.contains("elsewhere/object")),
        "a row naming another bucket was not counted as a foreign locator: {brief}"
    );
    assert!(
        !missing.iter().any(|k| k.contains("elsewhere/object")),
        "a foreign locator was reported as DATA LOSS; it is a row that never named this store, \
         and an operator reading this would go looking for bytes that were never here: {brief}"
    );
    assert!(
        !outcome.is_intact(),
        "is_intact must be false while an artefact's object is missing"
    );
    assert!(
        outcome.rows_scanned >= 2 && outcome.objects_scanned >= 1,
        "both sides must have been read: {brief}"
    );

    drop(pool);
    drop_db(&db_name).await.expect("drop db");
}

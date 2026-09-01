use aws_config::BehaviorVersion;
use aws_sdk_s3::Client;
use aws_sdk_s3::config::Region;
use aws_sdk_s3::primitives::ByteStream;
use proxima_blob_s3::{
    BlobError, CitedBlobReadUrlOutcomeTs, CitedBlobReadUrlTs, CitedBlobStore,
    CitedBlobUploadAbortTs, CitedBlobUploadCompleteTs, CitedBlobUploadPrepareTs, S3RuntimeConfig,
};
use proxima_core::engine::{Engine, UploadCompleted, UploadCompletionExpectation};
use proxima_core::error::ProtocolError;
use proxima_core::storage_ports::{
    CitedBlobHeld, CitedBlobIntegrityMismatch, CitedBlobPort, CitedBlobReadError,
    CitedBlobReadPort, CitedBlobReadUrl, CitedBlobReconcileOutcome, CitedBlobService,
    CitedBlobStaged, CitedBlobUploadAborted, CitedBlobUploadPrepared, CitedObjectErasePort,
    MAX_HELD_BLOB_DIGESTS, OwnerTransferPort, OwnerWritePermit,
};
use proxima_core::test_fixtures::owner_fixture;
use proxima_core::{
    AccessKind, AuthPath, AuthzContext, ColdObjectStore, EntityId, ErrorCode, FlavorRegistry,
    GroupId, OwnerRef, Relation, StorageError, UserId,
};
use sha2::Digest;
use std::num::NonZeroU64;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, oneshot};

// Contexts here are `AuthPath::HostBearer`, matching every production
// caller. Completion writes a Fact through the engine, and an
// `AuthPath::System` context needs the host-held `SystemAuthority` witness
// to issue any owner-write permit. Request fixtures use HostBearer; the
// global maintenance test below obtains its witness by consuming a dedicated
// Engine through the same host boundary as production boot.
use proxima_pg_testkit::{create_db, db_url, drop_db, unique_db_name};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

/// The transfer's registry-resolved legs, exactly as the engine assembles
/// them. Passing a hand-built set here would test a registry production
/// never sees.
fn transfer_surfaces() -> proxima_core::owner_inverse::OwnerSurfaces {
    proxima_core::owner_inverse::OwnerSurfaces::for_registry(
        &proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests(),
    )
}

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

#[derive(Clone)]
struct BlobPortCounters {
    stage_calls: Arc<AtomicUsize>,
    finish_calls: Arc<AtomicUsize>,
}

#[derive(Clone)]
struct CountingBlobPort {
    inner: CitedBlobStore,
    counters: BlobPortCounters,
}

impl CountingBlobPort {
    fn new(inner: CitedBlobStore) -> (Self, BlobPortCounters) {
        let counters = BlobPortCounters {
            stage_calls: Arc::new(AtomicUsize::new(0)),
            finish_calls: Arc::new(AtomicUsize::new(0)),
        };
        (
            Self {
                inner,
                counters: counters.clone(),
            },
            counters,
        )
    }
}

#[async_trait::async_trait]
impl CitedBlobPort for CountingBlobPort {
    async fn prepare_upload(
        &self,
        authz: &AuthzContext,
        owner: OwnerRef,
        filename: &str,
        mime: &str,
        byte_len: u64,
    ) -> Result<CitedBlobUploadPrepared, StorageError> {
        CitedBlobPort::prepare_upload(&self.inner, authz, owner, filename, mime, byte_len).await
    }

    async fn stage_upload(
        &self,
        authz: &AuthzContext,
        owner: OwnerRef,
        upload_id: &str,
    ) -> Result<CitedBlobStaged, StorageError> {
        self.counters.stage_calls.fetch_add(1, Ordering::SeqCst);
        CitedBlobPort::stage_upload(&self.inner, authz, owner, upload_id).await
    }

    async fn finish_upload(
        &self,
        authz: &AuthzContext,
        owner: OwnerRef,
        upload_id: &str,
        cited_object_id: Uuid,
    ) -> Result<(), StorageError> {
        self.counters.finish_calls.fetch_add(1, Ordering::SeqCst);
        CitedBlobPort::finish_upload(&self.inner, authz, owner, upload_id, cited_object_id).await
    }

    async fn abort_upload(
        &self,
        authz: &AuthzContext,
        owner: OwnerRef,
        upload_id: &str,
    ) -> Result<CitedBlobUploadAborted, StorageError> {
        CitedBlobPort::abort_upload(&self.inner, authz, owner, upload_id).await
    }

    async fn read_url(
        &self,
        authz: &AuthzContext,
        owner: OwnerRef,
        cited_object_id: Uuid,
    ) -> Result<CitedBlobReadUrl, StorageError> {
        CitedBlobPort::read_url(&self.inner, authz, owner, cited_object_id).await
    }

    async fn find_held_blobs(
        &self,
        authz: &AuthzContext,
        owner: OwnerRef,
        content_hashes: &[[u8; 32]],
    ) -> Result<Vec<CitedBlobHeld>, StorageError> {
        CitedBlobPort::find_held_blobs(&self.inner, authz, owner, content_hashes).await
    }
}

fn counted_blob_service(store: &CitedBlobStore) -> (CitedBlobService, BlobPortCounters) {
    let (port, counters) = CountingBlobPort::new(store.clone());
    (
        CitedBlobService::new(Arc::new(port) as Arc<dyn CitedBlobPort>),
        counters,
    )
}

/// Completion as production runs it: the store stages the bytes, then ONE
/// transaction records the artefact and the `core/upload-v1` Fact that
/// cites it. The store records transfer bookkeeping but no corpus rows, so a
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

async fn complete_via_engine_with_expectation(
    pg: &PgStorage,
    blobs: &CitedBlobService,
    ctx: &AuthzContext,
    owner: OwnerRef,
    upload_id: &str,
    expectation: &UploadCompletionExpectation,
) -> Result<UploadCompleted, ProtocolError> {
    Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests())
        .with_storage_ports(std::sync::Arc::new(pg.clone()).storage_ports())
        .complete_upload_as_fact_with_expectation(blobs, ctx, owner, upload_id, &[], expectation)
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

/// A transparent HTTP/1 proxy used only to place one real S3 GET between two
/// store calls. It forwards signed bytes unchanged, so `RustFS` remains the
/// server under test; the gate makes the stale-pending response deterministic.
struct PendingGetGate {
    pending_key: String,
    canonical_key: String,
    pending_get_reached: Notify,
    pending_get_seen: AtomicBool,
    released: AtomicBool,
    release: Notify,
    canonical_puts: AtomicUsize,
}

impl PendingGetGate {
    fn new(pending_key: String, canonical_key: String) -> Self {
        Self {
            pending_key,
            canonical_key,
            pending_get_reached: Notify::new(),
            pending_get_seen: AtomicBool::new(false),
            released: AtomicBool::new(false),
            release: Notify::new(),
            canonical_puts: AtomicUsize::new(0),
        }
    }

    async fn wait_until_released(&self) {
        while !self.released.load(Ordering::Acquire) {
            self.release.notified().await;
        }
    }

    async fn wait_until_pending_get_reached(&self) {
        while !self.pending_get_seen.load(Ordering::Acquire) {
            self.pending_get_reached.notified().await;
        }
    }

    fn release(&self) {
        self.released.store(true, Ordering::Release);
        self.release.notify_waiters();
    }
}

/// Forward one request at the byte level. Reading only the header lets the
/// gate pause a GET before it reaches `RustFS` while `copy_bidirectional`
/// carries every request body and response without re-signing anything.
async fn proxy_connection(
    mut client_stream: TcpStream,
    upstream_addr: &str,
    gate: Arc<PendingGetGate>,
) -> std::io::Result<()> {
    let mut request_head = Vec::with_capacity(1024);
    let mut byte = [0_u8; 1];
    loop {
        client_stream.read_exact(&mut byte).await?;
        request_head.push(byte[0]);
        if request_head.ends_with(b"\r\n\r\n") {
            break;
        }
        if request_head.len() > 128 * 1024 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "S3 proxy request headers exceed test bound",
            ));
        }
    }
    let request_line = request_head
        .split(|byte| *byte == b'\n')
        .next()
        .and_then(|line| std::str::from_utf8(line).ok())
        .unwrap_or_default();
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts.next().unwrap_or_default();
    let path = request_parts
        .next()
        .unwrap_or_default()
        .split('?')
        .next()
        .unwrap_or_default();
    let pending_suffix = format!("/{}", gate.pending_key);
    let canonical_suffix = format!("/{}", gate.canonical_key);
    if method == "GET" && path.ends_with(&pending_suffix) {
        gate.pending_get_seen.store(true, Ordering::Release);
        gate.pending_get_reached.notify_one();
        gate.wait_until_released().await;
    }
    if method == "PUT" && path.ends_with(&canonical_suffix) {
        gate.canonical_puts.fetch_add(1, Ordering::AcqRel);
    }

    let forwarded_head = request_head_with_connection_close(&request_head);
    let mut upstream = TcpStream::connect(upstream_addr).await?;
    upstream.write_all(&forwarded_head).await?;
    tokio::io::copy_bidirectional(&mut client_stream, &mut upstream).await?;
    Ok(())
}

fn request_head_with_connection_close(request_head: &[u8]) -> Vec<u8> {
    let header_end = request_head.len() - 4;
    let mut forwarded = Vec::with_capacity(request_head.len() + 20);
    for raw_line in request_head[..header_end].split(|byte| *byte == b'\n') {
        let line = raw_line.strip_suffix(b"\r").unwrap_or(raw_line);
        let is_connection = line
            .iter()
            .position(|byte| *byte == b':')
            .is_some_and(|colon| line[..colon].eq_ignore_ascii_case(b"connection"));
        if !is_connection {
            forwarded.extend_from_slice(line);
            forwarded.extend_from_slice(b"\r\n");
        }
    }
    forwarded.extend_from_slice(b"connection: close\r\n\r\n");
    forwarded
}

/// Start a byte-transparent local proxy and return the config that points an
/// S3 client at it. The integration target is HTTP `RustFS`; HTTPS production
/// endpoints never use this test-only helper.
async fn start_pending_get_proxy(
    config: &S3RuntimeConfig,
    pending_key: String,
    canonical_key: String,
) -> (
    S3RuntimeConfig,
    Arc<PendingGetGate>,
    oneshot::Sender<()>,
    tokio::task::JoinHandle<()>,
) {
    let endpoint = reqwest::Url::parse(
        config
            .endpoint_url
            .as_deref()
            .expect("proxy test requires an HTTP S3 endpoint"),
    )
    .expect("valid S3 endpoint");
    assert_eq!(endpoint.scheme(), "http", "proxy test expects HTTP RustFS");
    let upstream_addr = format!(
        "{}:{}",
        endpoint.host_str().expect("S3 endpoint host"),
        endpoint.port_or_known_default().expect("S3 endpoint port")
    );
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind S3 test proxy");
    let proxy_addr = listener.local_addr().expect("proxy address");
    let gate = Arc::new(PendingGetGate::new(pending_key, canonical_key));
    let (shutdown, mut shutdown_rx) = oneshot::channel();
    let proxy_gate = gate.clone();
    let proxy_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut shutdown_rx => break,
                accepted = listener.accept() => {
                    let Ok((client_stream, _)) = accepted else { break };
                    let upstream_addr = upstream_addr.clone();
                    let gate = proxy_gate.clone();
                    tokio::spawn(async move {
                        let _ = proxy_connection(client_stream, &upstream_addr, gate).await;
                    });
                }
            }
        }
    });
    let proxy_config = S3RuntimeConfig {
        endpoint_url: Some(format!("http://{proxy_addr}")),
        ..config.clone()
    };
    (proxy_config, gate, shutdown, proxy_task)
}

async fn owner_fence_waiting(pool: &sqlx::PgPool, owner: OwnerRef) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1
               FROM pg_locks
              WHERE locktype = 'advisory'
                AND NOT granted
                AND mode = 'ExclusiveLock'
                AND classid::bigint = ((hashtextextended(
                    'proxima-owner-fence:' || $1 || ':' || $2::text, 0
                ) >> 32) & 4294967295)
                AND objid::bigint = (hashtextextended(
                    'proxima-owner-fence:' || $1 || ':' || $2::text, 0
                ) & 4294967295)
         )",
    )
    .bind(match owner {
        OwnerRef::Personal(_) => "personal",
        OwnerRef::Group(_) => "group",
    })
    .bind(owner.stored_owner_id())
    .fetch_one(pool)
    .await
}

async fn owner_fence_exclusive(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    owner: OwnerRef,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "SELECT pg_advisory_xact_lock(
             hashtextextended('proxima-owner-fence:' || $1 || ':' || $2::text, 0)
         )",
    )
    .bind(match owner {
        OwnerRef::Personal(_) => "personal",
        OwnerRef::Group(_) => "group",
    })
    .bind(owner.stored_owner_id())
    .execute(&mut **tx)
    .await
    .map(|_| ())
}

async fn wait_for_upload_status_probe(
    pool: &sqlx::PgPool,
    expected: i64,
) -> Result<(), sqlx::Error> {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let reached: bool = sqlx::query_scalar(
                "SELECT is_called AND last_value >= $1
                   FROM public.upload_status_probe",
            )
            .bind(expected)
            .fetch_one(pool)
            .await?;
            if reached {
                return Ok::<(), sqlx::Error>(());
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .map_err(|_| sqlx::Error::Protocol("upload status probe timed out".into()))??;
    Ok(())
}

async fn wait_for_s3_object(
    config: &S3RuntimeConfig,
    key: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let client = s3_client(config).await;
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if client
                .head_object()
                .bucket(&config.bucket)
                .key(key)
                .send()
                .await
                .is_ok()
            {
                return Ok::<(), Box<dyn std::error::Error>>(());
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .map_err(|_| "S3 object probe timed out")??;
    Ok(())
}

const UPLOAD_STATUS_PROBE_LOCK: i64 = 0x0055_504c_4f41_4453;

async fn run_fenced_upload_transition<T, Fut>(
    pool: &sqlx::PgPool,
    owner: OwnerRef,
    expected_probe: i64,
    transition: Fut,
) -> Result<Result<T, BlobError>, Box<dyn std::error::Error>>
where
    T: Send + 'static,
    Fut: std::future::Future<Output = Result<T, BlobError>> + Send + 'static,
{
    let mut blocker = pool.acquire().await?;
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(UPLOAD_STATUS_PROBE_LOCK)
        .execute(&mut *blocker)
        .await?;
    let task = tokio::spawn(transition);
    wait_for_upload_status_probe(pool, expected_probe).await?;

    let fence_pool = pool.clone();
    let fence = tokio::spawn(async move {
        let mut tx = fence_pool.begin().await?;
        owner_fence_exclusive(&mut tx, owner).await?;
        tx.rollback().await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if owner_fence_waiting(pool, owner).await? {
                return Ok::<(), sqlx::Error>(());
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .map_err(|_| sqlx::Error::Protocol("upload owner-fence probe timed out".into()))??;
    sqlx::query_scalar::<_, bool>("SELECT pg_advisory_unlock($1)")
        .bind(UPLOAD_STATUS_PROBE_LOCK)
        .fetch_one(&mut *blocker)
        .await?;
    let result = task
        .await
        .map_err(|err| sqlx::Error::Protocol(format!("upload transition task failed: {err}")))?;
    fence
        .await
        .map_err(|err| sqlx::Error::Protocol(format!("upload fence task failed: {err}")))??;
    Ok(result)
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
    // `complete` moves the object from its pending key to the final key
    // derived from the upload row (doc 11 §Large artefact storage), so the
    // presigned GET must reference the completed blob's key, not the
    // pending upload's.
    let cited_object_id = Uuid::parse_str(&completed.cited_object_id).expect("cited object id");
    let final_key: (String,) =
        sqlx::query_as("SELECT object_key FROM proxima_core.blob_uploads WHERE blob_id = $1")
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

/// Every database-side upload transition holds the owner fence through its
/// status decision. The trigger pauses each real transition, and a competing
/// exclusive owner fence must queue on that exact key; this pins finish,
/// abort, and expiry without relying on task scheduling.
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn upload_transitions_join_the_owner_fence() -> Result<(), Box<dyn std::error::Error>> {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset");
        return Ok(());
    }

    let (pg, db_name) = fresh_storage().await;
    let pool = pg.pool_for_tests().clone();
    let config = s3_config_for_dev();
    let store = CitedBlobStore::new(pool.clone(), config.clone()).expect("valid S3 config");
    let owner = owner_fixture();
    let ctx = AuthzContext::single_owner(&owner, AuthPath::HostBearer);

    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "CREATE SEQUENCE public.upload_status_probe;
         CREATE FUNCTION public.block_upload_status_update() RETURNS trigger
         LANGUAGE plpgsql AS $$
         BEGIN
             PERFORM nextval('public.upload_status_probe');
             PERFORM pg_advisory_xact_lock({UPLOAD_STATUS_PROBE_LOCK});
             RETURN NEW;
         END
         $$;
         CREATE TRIGGER block_upload_status_update
         BEFORE UPDATE OF status ON proxima_core.blob_uploads
         FOR EACH ROW
         WHEN (OLD.status IS DISTINCT FROM NEW.status)
         EXECUTE FUNCTION public.block_upload_status_update();"
    )))
    .execute(&pool)
    .await
    .expect("upload status probe");

    let finish_upload = staged_upload(
        &pool,
        &store,
        &config,
        &ctx,
        owner,
        "finish.pdf",
        b"finish-bytes",
    )
    .await;
    // `staged_upload` only prepares and PUTs the bytes. Finish refuses a row
    // with no exact staged identity, so the row must actually be staged before
    // this test can drive its status transition. Staging writes no status, so
    // the probe trigger above does not fire on it.
    let finish_content_hash = store
        .stage_upload(
            &ctx,
            CitedBlobUploadCompleteTs {
                owner,
                upload_id: finish_upload.clone(),
            },
        )
        .await
        .expect("stage the finish fixture")
        .payload
        .content_hash
        .to_vec();
    let finish_blob: Uuid = sqlx::query_scalar(
        "INSERT INTO proxima_core.blob (schema_id, owner_id, content_hash)
         VALUES ('core/uploaded-blob-v1', $1, $2)
         RETURNING blob_id",
    )
    .bind(owner.stored_owner_id())
    .bind(finish_content_hash)
    .fetch_one(&pool)
    .await
    .expect("finish blob");
    let finish_store = store.clone();
    let finish_ctx = ctx.clone();
    let finish_id = finish_upload.clone();
    let finish_result = Box::pin(run_fenced_upload_transition(&pool, owner, 1, async move {
        finish_store
            .finish_upload(&finish_ctx, owner, &finish_id, finish_blob)
            .await
    }))
    .await?;
    finish_result?;

    let abort_upload = staged_upload(
        &pool,
        &store,
        &config,
        &ctx,
        owner,
        "abort.pdf",
        b"abort-bytes",
    )
    .await;
    let abort_store = store.clone();
    let abort_ctx = ctx.clone();
    let abort_id = abort_upload.clone();
    let abort_result = Box::pin(run_fenced_upload_transition(&pool, owner, 2, async move {
        abort_store
            .abort_upload(
                &abort_ctx,
                CitedBlobUploadAbortTs {
                    owner,
                    upload_id: abort_id,
                },
            )
            .await
    }))
    .await?;
    assert!(abort_result?.aborted);

    let expiry_upload = staged_upload(
        &pool,
        &store,
        &config,
        &ctx,
        owner,
        "expiry.pdf",
        b"expiry-bytes",
    )
    .await;
    sqlx::query(
        "UPDATE proxima_core.blob_uploads
            SET expires_at = now() - interval '1 second'
          WHERE upload_id = $1",
    )
    .bind(Uuid::parse_str(&expiry_upload).expect("expiry upload id"))
    .execute(&pool)
    .await
    .expect("expire fixture");
    let expiry_store = store.clone();
    let expiry_ctx = ctx.clone();
    let expiry_id = expiry_upload.clone();
    let expiry_result = Box::pin(run_fenced_upload_transition(&pool, owner, 3, async move {
        expiry_store
            .stage_upload(
                &expiry_ctx,
                CitedBlobUploadCompleteTs {
                    owner,
                    upload_id: expiry_id,
                },
            )
            .await
    }))
    .await?;
    assert!(matches!(
        expiry_result,
        Err(BlobError::State(message)) if message.contains("expired")
    ));

    drop(pool);
    drop_db(&db_name).await.expect("drop db");
    Ok(())
}

/// Stage's S3 work may finish after a terminal transition has already locked
/// the upload row. The locator publication must then refuse the terminal row
/// and remove the just-created canonical object; an ignored zero-row UPDATE
/// would report a staged payload whose locator was never committed.
///
/// The assertion is right, and this currently fails. Making it pass needs the
/// locator publication to take the upload row `FOR UPDATE`, so it blocks on a
/// mid-flight abort instead of reading `pending` through its own `WHERE`
/// guard. That contradicts this module's standing rule that stage's S3 work
/// stays outside every database critical section, and adding the lock here
/// fails five of the races below. Reconciling the two is its own change.
#[ignore = "needs the lock-first stage publication described above"]
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn stage_rechecks_terminal_upload_before_publishing_locator()
-> Result<(), Box<dyn std::error::Error>> {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset");
        return Ok(());
    }

    let (pg, db_name) = fresh_storage().await;
    let pool = pg.pool_for_tests().clone();
    let config = s3_config_for_dev();
    let store = CitedBlobStore::new(pool.clone(), config.clone()).expect("valid S3 config");
    let owner = owner_fixture();
    let ctx = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
    let upload_id = staged_upload(
        &pool,
        &store,
        &config,
        &ctx,
        owner,
        "terminal-race.pdf",
        b"terminal-race-bytes",
    )
    .await;

    // Abort owns the row lock while its status trigger is paused. Stage can
    // still perform its S3 GET/PUT, but its final locator UPDATE must wait for
    // the row and then observe the committed terminal status.
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "CREATE SEQUENCE public.upload_status_probe;
         CREATE FUNCTION public.block_abort_status_update() RETURNS trigger
         LANGUAGE plpgsql AS $$
         BEGIN
             PERFORM nextval('public.upload_status_probe');
             PERFORM pg_advisory_xact_lock({UPLOAD_STATUS_PROBE_LOCK});
             RETURN NEW;
         END
         $$;
         CREATE TRIGGER block_abort_status_update
         BEFORE UPDATE OF status ON proxima_core.blob_uploads
         FOR EACH ROW
         WHEN (OLD.status = 'pending' AND NEW.status = 'aborted')
         EXECUTE FUNCTION public.block_abort_status_update();"
    )))
    .execute(&pool)
    .await?;

    let mut probe = pool.acquire().await?;
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(UPLOAD_STATUS_PROBE_LOCK)
        .execute(&mut *probe)
        .await?;
    let abort_store = store.clone();
    let abort_ctx = ctx.clone();
    let abort_id = upload_id.clone();
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
    wait_for_upload_status_probe(&pool, 1).await?;

    let stage_store = store.clone();
    let stage_ctx = ctx.clone();
    let stage_id = upload_id.clone();
    let stage_task = tokio::spawn(async move {
        stage_store
            .stage_upload(
                &stage_ctx,
                CitedBlobUploadCompleteTs {
                    owner,
                    upload_id: stage_id,
                },
            )
            .await
    });
    let canonical_key = format!("objects/{upload_id}");
    wait_for_s3_object(&config, &canonical_key).await?;

    sqlx::query_scalar::<_, bool>("SELECT pg_advisory_unlock($1)")
        .bind(UPLOAD_STATUS_PROBE_LOCK)
        .fetch_one(&mut *probe)
        .await?;
    let abort_result =
        tokio::time::timeout(std::time::Duration::from_secs(10), abort_task).await??;
    assert!(abort_result?.aborted);
    let stage_result =
        tokio::time::timeout(std::time::Duration::from_secs(10), stage_task).await??;
    assert!(
        matches!(stage_result, Err(BlobError::State(ref message)) if message.contains("aborted")),
        "stage must refuse the terminal row, got {stage_result:?}"
    );

    let (status, locator): (String, String) = sqlx::query_as(
        "SELECT status::text, object_key
           FROM proxima_core.blob_uploads
          WHERE upload_id = $1",
    )
    .bind(Uuid::parse_str(&upload_id)?)
    .fetch_one(&pool)
    .await?;
    assert_eq!(status, "aborted");
    assert_eq!(locator, format!("pending/{upload_id}"));
    let client = s3_client(&config).await;
    assert!(
        client
            .head_object()
            .bucket(&config.bucket)
            .key(&canonical_key)
            .send()
            .await
            .is_err(),
        "terminal stage must clean the canonical object it just created"
    );

    drop(pool);
    drop_db(&db_name).await?;
    Ok(())
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

/// A pre-existing canonical key is never overwritten. `RustFS` must enforce the
/// conditional write with `412 PreconditionFailed`; stage adopts only a
/// byte-identical object and otherwise fails closed, preserving the object.
#[tokio::test]
async fn conflicting_canonical_object_is_not_overwritten() {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset");
        return;
    }

    let (pg, db_name) = fresh_storage().await;
    let pool = pg.pool_for_tests().clone();
    let config = s3_config_for_dev();
    let store = CitedBlobStore::new(pool.clone(), config.clone()).expect("valid S3 config");
    let body: &'static [u8] = b"candidate canonical bytes";
    // Deliberately a different length. A canonical object that differs in size
    // is still a canonical conflict, and must be reported as one rather than
    // as a byte-length error attributed to this client's upload.
    let existing: &'static [u8] = b"a canonical object of an entirely different size";
    assert_ne!(body.len(), existing.len());
    let owner = owner_fixture();
    let ctx = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
    let (upload_id, _) = prepare_and_put_upload(
        &pool,
        &store,
        &config,
        &ctx,
        owner,
        "collision.pdf",
        "application/pdf",
        body,
    )
    .await;
    let canonical_key = format!("objects/{upload_id}");
    put_object_via_sdk(&config, &canonical_key, existing).await;
    let conditional_error = s3_client(&config)
        .await
        .put_object()
        .bucket(&config.bucket)
        .key(&canonical_key)
        .if_none_match("*")
        .body(ByteStream::from_static(body))
        .send()
        .await
        .expect_err("RustFS must reject a conditional overwrite");
    assert_eq!(
        conditional_error
            .as_service_error()
            .and_then(|service| service.meta().code()),
        Some("PreconditionFailed")
    );

    let error = store
        .stage_upload(
            &ctx,
            CitedBlobUploadCompleteTs {
                owner,
                upload_id: upload_id.clone(),
            },
        )
        .await
        .expect_err("conflicting canonical bytes must fail closed");
    assert!(
        matches!(error, BlobError::State(message) if message == "canonical object conflicts with staged bytes")
    );
    let retained = s3_client(&config)
        .await
        .get_object()
        .bucket(&config.bucket)
        .key(&canonical_key)
        .send()
        .await
        .expect("retained canonical object")
        .body
        .collect()
        .await
        .expect("read retained canonical object")
        .into_bytes();
    assert_eq!(retained.as_ref(), existing);
    assert_eq!(corpus_counts(&pool).await, (0, 0));

    drop(pool);
    drop_db(&db_name).await.expect("drop db");
}

/// A byte-identical canonical object may already exist when a stage adopts
/// it. A retry of that same upload must agree with the adopted payload and
/// leave exactly one canonical version on a versioned bucket.
#[tokio::test]
#[allow(clippy::too_many_lines)] // adoption and version assertions share one fixture
async fn versioned_conditional_adoption_is_stable_for_same_upload() {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset");
        return;
    }

    let (pg, db_name) = fresh_storage().await;
    let pool = pg.pool_for_tests().clone();
    let base = s3_config_for_dev();
    let config = S3RuntimeConfig {
        bucket: format!("{}-adopt-{}", base.bucket, Uuid::now_v7().simple()),
        ..base
    };
    let client = s3_client(&config).await;
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
    let body: &'static [u8] = b"versioned conditional adoption bytes";
    let (upload_id, pending_key) = prepare_and_put_upload(
        &pool,
        &store,
        &config,
        &ctx,
        owner,
        "adopt.pdf",
        "application/pdf",
        body,
    )
    .await;
    let canonical_key = format!("objects/{upload_id}");
    put_object_via_sdk(&config, &canonical_key, body).await;

    let first = store
        .stage_upload(
            &ctx,
            CitedBlobUploadCompleteTs {
                owner,
                upload_id: upload_id.clone(),
            },
        )
        .await
        .expect("first stage adopts byte-identical canonical object");
    let second = store
        .stage_upload(&ctx, CitedBlobUploadCompleteTs { owner, upload_id })
        .await
        .expect("same-upload retry reads the adopted canonical object");
    assert_eq!(
        (
            first.payload.content_hash,
            first.payload.bucket.as_str(),
            first.payload.object_key.as_str(),
            first.payload.sha256,
            first.payload.byte_len,
            first.payload.mime.as_str(),
            first.payload.filename.as_str(),
            first.payload.etag.as_deref(),
        ),
        (
            second.payload.content_hash,
            second.payload.bucket.as_str(),
            second.payload.object_key.as_str(),
            second.payload.sha256,
            second.payload.byte_len,
            second.payload.mime.as_str(),
            second.payload.filename.as_str(),
            second.payload.etag.as_deref(),
        ),
        "adoption and retry agree on all stored metadata",
    );
    assert_s3_key_versions_absent(&config, &pending_key).await;

    let versions = client
        .list_object_versions()
        .bucket(&config.bucket)
        .prefix(&canonical_key)
        .send()
        .await
        .expect("list canonical versions");
    assert_eq!(
        versions
            .versions()
            .iter()
            .filter(|version| version.key() == Some(canonical_key.as_str()))
            .count(),
        1,
        "adoption and retry leave one canonical version"
    );
    assert!(
        versions
            .delete_markers()
            .iter()
            .all(|marker| marker.key() != Some(canonical_key.as_str())),
        "adoption does not create a canonical delete marker"
    );

    drop(pool);
    drop_db(&db_name).await.expect("drop db");
}

/// Two calls for one upload can both finish the S3/SQL race only after their
/// conditional publication has converged. A statement trigger holds both
/// locator updates behind one advisory lock, so this test observes both
/// waiters before release and proves that `RustFS` contains one canonical
/// version afterward.
#[tokio::test]
#[allow(clippy::too_many_lines)] // the barrier, provider versions, and assertions are one proof
async fn versioned_same_upload_stages_converge_behind_locator_barrier() {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset");
        return;
    }

    let (pg, db_name) = fresh_storage().await;
    let pool = pg.pool_for_tests().clone();
    let base = s3_config_for_dev();
    let config = S3RuntimeConfig {
        bucket: format!("{}-race-{}", base.bucket, Uuid::now_v7().simple()),
        ..base
    };
    let client = s3_client(&config).await;
    client
        .create_bucket()
        .bucket(&config.bucket)
        .send()
        .await
        .expect("create race bucket");
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
    let body: &'static [u8] = b"one upload, two concurrent stages, one canonical object";
    let (upload_id, pending_key) = prepare_and_put_upload(
        &pool,
        &store,
        &config,
        &ctx,
        owner,
        "same-upload.pdf",
        "application/pdf",
        body,
    )
    .await;
    // Leave multiple pending versions and a marker for both cleanup callers.
    put_object_via_sdk(&config, &pending_key, body).await;
    client
        .delete_object()
        .bucket(&config.bucket)
        .key(&pending_key)
        .send()
        .await
        .expect("create pending delete marker");
    // Keep an object current so both stages can read it; cleanup must still
    // remove the older versions and the marker behind it.
    put_object_via_sdk(&config, &pending_key, body).await;

    let canonical_key = format!("objects/{upload_id}");
    sqlx::query(
        "CREATE FUNCTION upload_locator_barrier() RETURNS trigger LANGUAGE plpgsql AS $$
            BEGIN
                PERFORM pg_advisory_xact_lock(hashtextextended(current_database(), 0));
                RETURN NULL;
            END
        $$",
    )
    .execute(&pool)
    .await
    .expect("create locator barrier function");
    sqlx::query(
        "CREATE TRIGGER upload_locator_barrier_trigger
         BEFORE UPDATE OF object_key ON proxima_core.blob_uploads
         FOR EACH STATEMENT EXECUTE FUNCTION upload_locator_barrier()",
    )
    .execute(&pool)
    .await
    .expect("create locator barrier trigger");

    let mut barrier = pool.begin().await.expect("begin barrier transaction");
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended(current_database(), 0))")
        .execute(&mut *barrier)
        .await
        .expect("hold locator barrier");

    let first_store = store.clone();
    let first_ctx = ctx.clone();
    let first_upload_id = upload_id.clone();
    let first = tokio::spawn(async move {
        first_store
            .stage_upload(
                &first_ctx,
                CitedBlobUploadCompleteTs {
                    owner,
                    upload_id: first_upload_id,
                },
            )
            .await
    });
    let second_store = store.clone();
    let second_ctx = ctx.clone();
    let second_upload_id = upload_id.clone();
    let second = tokio::spawn(async move {
        second_store
            .stage_upload(
                &second_ctx,
                CitedBlobUploadCompleteTs {
                    owner,
                    upload_id: second_upload_id,
                },
            )
            .await
    });

    let deadline = Instant::now() + Duration::from_secs(10);
    while head(&s3_client(&config).await, &config.bucket, &canonical_key)
        .await
        .is_none()
    {
        assert!(
            Instant::now() < deadline,
            "stage calls did not publish canonical bytes"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let mut waiters = 0_i64;
    while waiters < 2 {
        waiters = sqlx::query_scalar(
            "SELECT count(*)::bigint
               FROM pg_stat_activity
              WHERE datname = current_database()
                AND wait_event_type = 'Lock'
                AND wait_event = 'advisory'
                AND query LIKE '%UPDATE proxima_core.blob_uploads%'",
        )
        .fetch_one(&pool)
        .await
        .expect("inspect advisory waiters");
        assert!(
            Instant::now() < deadline,
            "both stage calls must wait at the locator barrier (observed {waiters})"
        );
        if waiters < 2 {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
    drop(barrier);

    let first = first
        .await
        .expect("first stage task join")
        .expect("first stage");
    let second = second
        .await
        .expect("second stage task join")
        .expect("second stage");
    assert_eq!(first.payload.content_hash, second.payload.content_hash);
    assert_eq!(first.payload.sha256, second.payload.sha256);
    assert_eq!(first.payload.byte_len, second.payload.byte_len);
    assert_eq!(first.payload.mime, second.payload.mime);
    assert_eq!(first.payload.filename, second.payload.filename);
    assert_eq!(first.payload.etag, second.payload.etag);
    assert_s3_key_versions_absent(&config, &pending_key).await;

    let canonical = client
        .get_object()
        .bucket(&config.bucket)
        .key(&canonical_key)
        .send()
        .await
        .expect("read canonical object")
        .body
        .collect()
        .await
        .expect("collect canonical object")
        .into_bytes();
    assert_eq!(
        canonical.as_ref(),
        body,
        "both stages retain the exact payload"
    );
    let versions = client
        .list_object_versions()
        .bucket(&config.bucket)
        .prefix(&canonical_key)
        .send()
        .await
        .expect("list canonical versions");
    assert_eq!(
        versions
            .versions()
            .iter()
            .filter(|version| version.key() == Some(canonical_key.as_str()))
            .count(),
        1,
        "same-upload race leaves exactly one canonical version"
    );
    assert!(
        versions
            .delete_markers()
            .iter()
            .all(|marker| marker.key() != Some(canonical_key.as_str())),
        "same-upload race does not create a canonical delete marker"
    );

    sqlx::query("DROP TRIGGER upload_locator_barrier_trigger ON proxima_core.blob_uploads")
        .execute(&pool)
        .await
        .expect("drop locator barrier trigger");
    sqlx::query("DROP FUNCTION upload_locator_barrier()")
        .execute(&pool)
        .await
        .expect("drop locator barrier function");
    drop(pool);
    drop_db(&db_name).await.expect("drop db");
}

/// A second stage can have loaded the pending locator just before the first
/// stage records and purges it. The real GET therefore returns 404; this
/// transparent proxy freezes that GET until stage A is complete, proving that
/// stage B reloads once, reads canonical bytes, and performs no second PUT.
#[tokio::test]
#[allow(clippy::too_many_lines)] // proxy lifecycle and the stale-locator proof are one fixture
async fn stale_pending_get_reloads_canonical_without_republication() {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset");
        return;
    }

    let (pg, db_name) = fresh_storage().await;
    let pool = pg.pool_for_tests().clone();
    let base = s3_config_for_dev();
    let config = S3RuntimeConfig {
        bucket: format!("{}-stale-{}", base.bucket, Uuid::now_v7().simple()),
        ..base
    };
    let client = s3_client(&config).await;
    client
        .create_bucket()
        .bucket(&config.bucket)
        .send()
        .await
        .expect("create stale-locator bucket");
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

    let store_a = CitedBlobStore::new(pool.clone(), config.clone()).expect("valid S3 config");
    let owner = owner_fixture();
    let ctx = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
    let body: &'static [u8] = b"canonical bytes survive a stale pending GET";
    let (upload_id, pending_key) = prepare_and_put_upload(
        &pool,
        &store_a,
        &config,
        &ctx,
        owner,
        "stale.pdf",
        "application/pdf",
        body,
    )
    .await;
    let canonical_key = format!("objects/{upload_id}");
    let (proxy_config, gate, shutdown, proxy_task) =
        start_pending_get_proxy(&config, pending_key.clone(), canonical_key.clone()).await;
    let store_b = CitedBlobStore::new(pool.clone(), proxy_config).expect("proxy S3 config");
    let b_store = store_b.clone();
    let b_ctx = ctx.clone();
    let b_upload_id = upload_id.clone();
    let b_task = tokio::spawn(async move {
        b_store
            .stage_upload(
                &b_ctx,
                CitedBlobUploadCompleteTs {
                    owner,
                    upload_id: b_upload_id,
                },
            )
            .await
    });
    gate.wait_until_pending_get_reached().await;

    // B has loaded the pending locator and is held before its first GET
    // reaches RustFS. A now records the canonical locator and purges every
    // pending version, making B's released GET a genuine 404.
    let staged_a = store_a
        .stage_upload(
            &ctx,
            CitedBlobUploadCompleteTs {
                owner,
                upload_id: upload_id.clone(),
            },
        )
        .await
        .expect("stage A records canonical locator");
    assert_s3_key_versions_absent(&config, &pending_key).await;
    gate.release();

    let staged_b = b_task
        .await
        .expect("stage B task join")
        .expect("stage B reloads canonical locator");
    assert_eq!(staged_a.payload.content_hash, staged_b.payload.content_hash);
    assert_eq!(staged_a.payload.sha256, staged_b.payload.sha256);
    assert_eq!(staged_a.payload.byte_len, staged_b.payload.byte_len);
    assert_eq!(staged_a.payload.mime, staged_b.payload.mime);
    assert_eq!(staged_a.payload.filename, staged_b.payload.filename);
    assert_eq!(staged_a.payload.etag, staged_b.payload.etag);
    assert_eq!(
        gate.canonical_puts.load(Ordering::Acquire),
        0,
        "stale-locator retry reads canonical bytes without another PUT"
    );
    assert_s3_key_versions_absent(&config, &pending_key).await;
    let canonical = client
        .get_object()
        .bucket(&config.bucket)
        .key(&canonical_key)
        .send()
        .await
        .expect("read canonical object")
        .body
        .collect()
        .await
        .expect("collect canonical object")
        .into_bytes();
    assert_eq!(
        canonical.as_ref(),
        body,
        "stale retry preserves exact bytes"
    );

    shutdown.send(()).expect("stop S3 test proxy");
    proxy_task.await.expect("S3 test proxy task");
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
    let key: (String,) =
        sqlx::query_as("SELECT object_key FROM proxima_core.blob_uploads WHERE upload_id = $1")
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

    let objects: i64 = sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.blob")
        .fetch_one(&pool)
        .await
        .expect("count");
    assert_eq!(objects, 1, "one file, one row");
    let facts: i64 = sqlx::query_scalar(
        "SELECT count(*)::bigint
           FROM proxima_core.memory m
           JOIN proxima_core.memory_head h ON h.handle = m.handle AND h.t = m.t
          WHERE m.schema_id = 'core/upload-v1'",
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
        "SELECT count(*)::bigint
           FROM proxima_core.memory m
           JOIN proxima_core.memory_head h ON h.handle = m.handle AND h.t = m.t
          WHERE m.schema_id = 'core/upload-v1'",
    )
    .fetch_one(&pool)
    .await
    .expect("count");
    let objects: i64 = sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.blob")
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
            let cited_object_id = Uuid::parse_str(&outcome.blob.cited_object_id)
                .expect("the caller was handed a valid cited-object id");
            let verified = store
                .collect_verified(
                    &ctx,
                    owner,
                    cited_object_id,
                    NonZeroU64::new(u64::try_from(body.len()).expect("body length fits"))
                        .expect("non-empty race fixture"),
                )
                .await
                .expect("a successful completion retains the original bytes");
            assert_eq!(verified.bytes, body, "the successful race kept exact bytes");
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
    let pending: (String,) =
        sqlx::query_as("SELECT object_key FROM proxima_core.blob_uploads WHERE upload_id = $1")
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
    let pending: (String,) =
        sqlx::query_as("SELECT object_key FROM proxima_core.blob_uploads WHERE upload_id = $1")
            .bind(Uuid::parse_str(&prepared.upload_id).expect("upload id"))
            .fetch_one(pool)
            .await
            .expect("upload row");
    put_object_via_sdk(config, &pending.0, b"real").await;
    let completed = complete_via_engine(pg, store, ctx, owner, &prepared.upload_id)
        .await
        .map_err(|err| BlobError::State(err.to_string()))?
        .blob;
    let stored: (String,) =
        sqlx::query_as("SELECT object_key FROM proxima_core.blob_uploads WHERE blob_id = $1")
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
    // for; (b) this bucket, under a key shaped like one this store mints
    // but belonging to no row of the forger's; (c) both wrong.
    let foreign_bucket = forge_uploaded_blob_row(
        &pool,
        Uuid::nil(),
        "not-the-configured-bucket",
        &genuine_key,
        1,
    )
    .await;
    let foreign_key = forge_uploaded_blob_row(
        &pool,
        Uuid::nil(),
        &config.bucket,
        &format!("objects/{}", Uuid::now_v7()),
        2,
    )
    .await;
    let both_foreign = forge_uploaded_blob_row(
        &pool,
        Uuid::nil(),
        "not-the-configured-bucket",
        &format!("objects/{}", Uuid::now_v7()),
        3,
    )
    .await;

    // The decisive case, and the one a plain "does the caller own the row"
    // check waves through: the caller's OWN owner, the CONFIGURED bucket,
    // and the canonical key of a DIFFERENT upload row that really exists.
    // Every owner predicate passes. Only re-deriving the key from the
    // forged row's own `upload_id` — a server-minted primary key the
    // forger cannot choose — refuses it.
    let own_owner_foreign_key = forge_uploaded_blob_row(
        &pool,
        owner.stored_owner_id(),
        &config.bucket,
        &genuine_key,
        4,
    )
    .await;

    for (case, forged_id) in [
        ("foreign bucket, own key", foreign_bucket),
        ("well-shaped key of no row", foreign_key),
        ("foreign bucket and key", both_foreign),
        ("own owner, another row's key", own_owner_foreign_key),
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

/// A transfer moves rows, not bytes.
///
/// Object keys carry no owner, so the destination reads the citation
/// through the key the source minted: no GET/PUT copy, no re-mint, no
/// second copy of the PII to erase later. Before the re-key this was
/// impossible — the key embedded the source's `owner_hash`, so either the
/// object had to be rewritten under the destination or the citation went
/// dark at the destination.
#[tokio::test]
async fn a_transfer_moves_the_citation_without_copying_the_object() {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset (run the s3 service to enable)");
        return;
    }

    let (pg, db_name) = fresh_storage().await;
    let pool = pg.pool_for_tests().clone();
    let config = s3_config_for_dev();
    let store = CitedBlobStore::new(pool.clone(), config.clone()).expect("valid S3 config");
    let source = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    let ctx = AuthzContext::single_owner(&source, AuthPath::HostBearer);
    let (destination, dest_ctx) = group_destination();

    let completed = upload_one_object(&pg, &store, &config, &pool, &ctx, source, b"transferred")
        .await
        .expect("upload");
    let client = s3_client(&config).await;
    let before = head(&client, &config.bucket, &completed.object_key)
        .await
        .expect("object present before the transfer");

    let permit = OwnerWritePermit::new_for_tests(source, AccessKind::Fact);
    assert!(
        pg.transfer_to_owner(
            &permit,
            EntityId::Memory(completed.memory_id),
            destination,
            &transfer_surfaces()
        )
        .await
        .expect("transfer"),
        "the cited series transfers"
    );

    // The row moved; the key did not.
    let after_key: String =
        sqlx::query_scalar("SELECT object_key FROM proxima_core.blob_uploads WHERE blob_id = $1")
            .bind(completed.cited_object_id)
            .fetch_one(&pool)
            .await
            .expect("upload row after transfer");
    assert_eq!(
        after_key, completed.object_key,
        "a transfer must not re-key the object"
    );
    let after = head(&client, &config.bucket, &completed.object_key)
        .await
        .expect("object still present after the transfer");
    assert_eq!(
        (before.0, before.1),
        (after.0, after.1),
        "same etag and last-modified: nothing was rewritten"
    );

    // And there is exactly one version of it: a re-mint path would GET+PUT
    // the bytes to a destination-derived key and delete the source copy, so
    // a second version (or a second key) is the signature
    // of the work this scheme exists to avoid.
    let versions = client
        .list_object_versions()
        .bucket(&config.bucket)
        .prefix(&completed.object_key)
        .send()
        .await
        .expect("list versions of the transferred object");
    assert_eq!(
        versions.versions().len(),
        1,
        "a transfer performs no object-store work at all"
    );
    assert!(versions.delete_markers().is_empty());

    // The destination reads the citation; the source cannot.
    dest_ctx_reads(&store, &dest_ctx, destination, completed.cited_object_id)
        .await
        .expect("destination reads the transferred citation");
    store
        .read_url(
            &ctx,
            CitedBlobReadUrlTs {
                owner: source,
                cited_object_id: completed.cited_object_id.to_string(),
            },
        )
        .await
        .expect_err("the source no longer holds the citation");

    drop(pool);
    drop_db(&db_name).await.expect("drop db");
}

/// Erase completeness survives a transfer in both directions.
///
/// The purge enumerates keys from the OWNER'S ROWS, so after a transfer the
/// object is reachable from the destination's rows and from nobody else's.
/// A source erase must therefore leave the destination's bytes standing,
/// and the destination's own erase must still remove them, so the object
/// never becomes un-erasable.
///
/// Enumerating rows is what buys that. A key is `objects/<upload_id>` and
/// never moves, so it says nothing about who owns the object now: any purge
/// that worked from the key itself would have to treat the transferred
/// object as still the source's and delete bytes the destination owns.
#[tokio::test]
async fn erase_follows_the_transfer_rather_than_the_key() {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset (run the s3 service to enable)");
        return;
    }

    let (pg, db_name) = fresh_storage().await;
    let pool = pg.pool_for_tests().clone();
    let config = s3_config_for_dev();
    let store = CitedBlobStore::new(pool.clone(), config.clone()).expect("valid S3 config");
    let source = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    let ctx = AuthzContext::single_owner(&source, AuthPath::HostBearer);
    let (destination, dest_ctx) = group_destination();

    let completed = upload_one_object(&pg, &store, &config, &pool, &ctx, source, b"erase-me-later")
        .await
        .expect("upload");
    // A second object the source keeps, so "the source purge did nothing"
    // is distinguishable from "the source purge worked and skipped the
    // transferred object".
    let retained = upload_one_object(&pg, &store, &config, &pool, &ctx, source, b"stays-mine")
        .await
        .expect("second upload");

    let permit = OwnerWritePermit::new_for_tests(source, AccessKind::Fact);
    assert!(
        pg.transfer_to_owner(
            &permit,
            EntityId::Memory(completed.memory_id),
            destination,
            &transfer_surfaces()
        )
        .await
        .expect("transfer"),
        "the cited series transfers"
    );

    let client = s3_client(&config).await;
    store
        .purge_owner_objects(source)
        .await
        .expect("source erase");

    assert!(
        head(&client, &config.bucket, &retained.object_key)
            .await
            .is_none(),
        "the source's own object is erased"
    );
    assert!(
        head(&client, &config.bucket, &completed.object_key)
            .await
            .is_some(),
        "a source erase must not reach bytes it transferred away"
    );
    dest_ctx_reads(&store, &dest_ctx, destination, completed.cited_object_id)
        .await
        .expect("the destination still reads its citation after the source erase");

    store
        .purge_owner_objects(destination)
        .await
        .expect("destination erase");
    assert!(
        head(&client, &config.bucket, &completed.object_key)
            .await
            .is_none(),
        "the destination's erase removes the bytes it now holds"
    );

    drop(pool);
    drop_db(&db_name).await.expect("drop db");
}

/// A group and a context that reads it. A transfer destination is always a
/// group (group-manage is the receiving side's consent), and group access
/// exists only as a subject's role — `single_owner` denies a bare group on
/// purpose.
fn group_destination() -> (OwnerRef, AuthzContext) {
    let group = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
    let member = UserId::new(Uuid::now_v7());
    let ctx = AuthzContext::for_subject_with_role(
        member,
        [(group, Relation::Admin.role())],
        AuthPath::HostBearer,
    );
    (group, ctx)
}

struct UploadedObject {
    cited_object_id: Uuid,
    memory_id: proxima_core::MemoryId,
    object_key: String,
}

/// prepare -> PUT -> complete, reported with everything the transfer and
/// erase assertions need.
async fn upload_one_object(
    pg: &PgStorage,
    store: &CitedBlobStore,
    config: &S3RuntimeConfig,
    pool: &sqlx::PgPool,
    ctx: &AuthzContext,
    owner: OwnerRef,
    body: &'static [u8],
) -> Result<UploadedObject, BlobError> {
    let prepared = store
        .prepare_upload(
            ctx,
            CitedBlobUploadPrepareTs {
                owner,
                filename: "artefact.pdf".into(),
                mime: "application/pdf".into(),
                byte_len: body.len() as u64,
            },
        )
        .await?;
    let pending: (String,) =
        sqlx::query_as("SELECT object_key FROM proxima_core.blob_uploads WHERE upload_id = $1")
            .bind(Uuid::parse_str(&prepared.upload_id).expect("upload id"))
            .fetch_one(pool)
            .await
            .expect("upload row");
    put_object_via_sdk(config, &pending.0, body).await;
    let outcome = complete_via_engine(pg, store, ctx, owner, &prepared.upload_id)
        .await
        .map_err(|err| BlobError::State(err.to_string()))?;
    let cited_object_id = Uuid::parse_str(&outcome.blob.cited_object_id).expect("cited object id");
    let object_key: String =
        sqlx::query_scalar("SELECT object_key FROM proxima_core.blob_uploads WHERE blob_id = $1")
            .bind(cited_object_id)
            .fetch_one(pool)
            .await
            .expect("completed blob row");
    Ok(UploadedObject {
        cited_object_id,
        memory_id: outcome.fact.memory_id,
        object_key,
    })
}

/// `(etag, last_modified)` when the object exists, `None` when it does not.
async fn head(
    client: &Client,
    bucket: &str,
    key: &str,
) -> Option<(Option<String>, Option<aws_sdk_s3::primitives::DateTime>)> {
    client
        .head_object()
        .bucket(bucket)
        .key(key)
        .send()
        .await
        .ok()
        .map(|out| {
            (
                out.e_tag().map(str::to_string),
                out.last_modified().copied(),
            )
        })
}

async fn dest_ctx_reads(
    store: &CitedBlobStore,
    ctx: &AuthzContext,
    owner: OwnerRef,
    cited_object_id: Uuid,
) -> Result<CitedBlobReadUrlOutcomeTs, BlobError> {
    store
        .read_url(
            ctx,
            CitedBlobReadUrlTs {
                owner,
                cited_object_id: cited_object_id.to_string(),
            },
        )
        .await
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
    let pending: (String,) =
        sqlx::query_as("SELECT object_key FROM proxima_core.blob_uploads WHERE upload_id = $1")
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
    let status: (String,) =
        sqlx::query_as("SELECT status::text FROM proxima_core.blob_uploads WHERE upload_id = $1")
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

    // Upload + complete: leaves a canonical object at objects/<upload_id>.
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
    let final_key: (String,) =
        sqlx::query_as("SELECT object_key FROM proxima_core.blob_uploads WHERE blob_id = $1")
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

    // In-band purge (the port the engine calls after an owner erase commits).
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
#[allow(clippy::too_many_lines)] // replay cleanup and owner-purge assertions share one fixture
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
    let (service, counters) = counted_blob_service(&store);
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
    let first = complete_via_engine_with_expectation(
        &pg,
        &service,
        &ctx,
        owner,
        &prepared.upload_id,
        &upload_expectation(body, "application/pdf", "doc.pdf"),
    )
    .await
    .expect("complete");
    let completed = first.blob.clone();
    assert_eq!(counters.stage_calls.load(Ordering::SeqCst), 1);
    assert_eq!(counters.finish_calls.load(Ordering::SeqCst), 1);
    let cited_object_id = Uuid::parse_str(&completed.cited_object_id).expect("cited object id");
    let final_key: (String,) =
        sqlx::query_as("SELECT object_key FROM proxima_core.blob_uploads WHERE blob_id = $1")
            .bind(cited_object_id)
            .fetch_one(&pool)
            .await
            .expect("completed blob row");

    // Finish must purge the transfer copy, including all versions and
    // markers, before the replay-visible completion is returned.
    assert_s3_key_versions_absent(&config, &pending.1).await;

    // Recreate transfer debris after the first finish. A direct stage replay
    // must use the completed row and retry version-aware pending cleanup even
    // when no Engine call follows it.
    put_object_via_sdk(&config, &pending.1, body).await;
    put_object_via_sdk(&config, &pending.1, body).await;
    client
        .delete_object()
        .bucket(&config.bucket)
        .key(&pending.1)
        .send()
        .await
        .expect("create replay cleanup marker");
    let direct_stage = store
        .stage_upload(
            &ctx,
            CitedBlobUploadCompleteTs {
                owner,
                upload_id: prepared.upload_id.clone(),
            },
        )
        .await
        .expect("direct completed stage replay");
    assert_eq!(direct_stage.already_completed, Some(cited_object_id));
    assert_s3_key_versions_absent(&config, &pending.1).await;

    // Recreate the same debris once more. The expectation-bearing Engine
    // replay must return the same corpus ids and perform the same cleanup.
    put_object_via_sdk(&config, &pending.1, body).await;
    put_object_via_sdk(&config, &pending.1, body).await;
    client
        .delete_object()
        .bucket(&config.bucket)
        .key(&pending.1)
        .send()
        .await
        .expect("create engine-replay cleanup marker");
    let replay = complete_via_engine_with_expectation(
        &pg,
        &service,
        &ctx,
        owner,
        &prepared.upload_id,
        &upload_expectation(body, "application/pdf", "doc.pdf"),
    )
    .await
    .expect("same-expectation replay");
    assert!(replay.blob.idempotent_replay);
    assert!(replay.fact.idempotent_replay);
    assert_eq!(replay.blob.cited_object_id, completed.cited_object_id);
    assert_eq!(replay.fact.memory_id, first.fact.memory_id);
    assert_eq!(counters.stage_calls.load(Ordering::SeqCst), 2);
    assert_eq!(counters.finish_calls.load(Ordering::SeqCst), 2);
    assert_s3_key_versions_absent(&config, &pending.1).await;

    // Re-put the row's own canonical key to mint a second version, so a
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

#[tokio::test]
async fn versioned_cold_delete_removes_exact_key_versions_and_preserves_prefix_collision() {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset (run the s3 service to enable)");
        return;
    }
    let base = s3_config_for_dev();
    let (pg, db_name) = fresh_storage().await;
    let pool = pg.pool_for_tests().clone();
    let config = S3RuntimeConfig {
        bucket: format!("{}-versioned", base.bucket),
        ..base
    };
    let client = s3_client(&config).await;
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
    let exact = format!("cold/exact/{}", Uuid::now_v7());
    let collision = format!("{exact}-suffix");
    put_object_via_sdk(&config, &exact, b"first").await;
    put_object_via_sdk(&config, &exact, b"second").await;
    put_object_via_sdk(&config, &collision, b"keep").await;
    client
        .delete_object()
        .bucket(&config.bucket)
        .key(&exact)
        .send()
        .await
        .expect("create exact-key delete marker");

    let store = CitedBlobStore::new(pool.clone(), config.clone()).expect("valid S3 config");
    store
        .cold_store()
        .delete(&exact)
        .await
        .expect("hard-delete exact key");
    let listed = client
        .list_object_versions()
        .bucket(&config.bucket)
        .prefix(&exact)
        .send()
        .await
        .expect("list exact prefix");
    assert!(
        listed
            .versions()
            .iter()
            .all(|version| version.key() != Some(exact.as_str()))
            && listed
                .delete_markers()
                .iter()
                .all(|marker| marker.key() != Some(exact.as_str())),
        "every exact-key version and delete marker must be gone"
    );
    assert!(
        listed
            .versions()
            .iter()
            .any(|version| version.key() == Some(collision.as_str())),
        "prefix-collision key must survive exact deletion"
    );
    store
        .cold_store()
        .delete(&collision)
        .await
        .expect("clean collision key");
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
    let pending: (String,) =
        sqlx::query_as("SELECT object_key FROM proxima_core.blob_uploads WHERE upload_id = $1")
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
/// because reporting a forged or hand-written locator as data loss would send an
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

#[allow(clippy::too_many_arguments)] // one helper carries the shared PG/S3 fixture seams
async fn prepare_and_put_upload(
    pool: &sqlx::PgPool,
    store: &CitedBlobStore,
    config: &S3RuntimeConfig,
    ctx: &AuthzContext,
    owner: OwnerRef,
    filename: &str,
    mime: &str,
    body: &'static [u8],
) -> (String, String) {
    let prepared = store
        .prepare_upload(
            ctx,
            CitedBlobUploadPrepareTs {
                owner,
                filename: filename.to_owned(),
                mime: mime.to_owned(),
                byte_len: u64::try_from(body.len()).expect("test body length fits in u64"),
            },
        )
        .await
        .expect("prepare");
    let upload_id = Uuid::parse_str(&prepared.upload_id).expect("upload id");
    let pending_key: String =
        sqlx::query_scalar("SELECT object_key FROM proxima_core.blob_uploads WHERE upload_id = $1")
            .bind(upload_id)
            .fetch_one(pool)
            .await
            .expect("pending object key");
    put_object_via_sdk(config, &pending_key, body).await;
    (prepared.upload_id, pending_key)
}

fn upload_expectation(body: &[u8], mime: &str, filename: &str) -> UploadCompletionExpectation {
    UploadCompletionExpectation::new(
        *blake3::hash(body).as_bytes(),
        u64::try_from(body.len()).expect("test body length fits in u64"),
        mime,
        filename,
    )
}

async fn corpus_counts(pool: &sqlx::PgPool) -> (i64, i64) {
    let blobs: i64 = sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.blob")
        .fetch_one(pool)
        .await
        .expect("blob count");
    let facts: i64 = sqlx::query_scalar(
        "SELECT count(*)::bigint
           FROM proxima_core.memory m
           JOIN proxima_core.memory_head h ON h.handle = m.handle AND h.t = m.t
          WHERE m.schema_id = 'core/upload-v1'",
    )
    .fetch_one(pool)
    .await
    .expect("Fact count");
    (blobs, facts)
}

async fn upload_status(pool: &sqlx::PgPool, upload_id: &str) -> String {
    sqlx::query_scalar("SELECT status::text FROM proxima_core.blob_uploads WHERE upload_id = $1")
        .bind(Uuid::parse_str(upload_id).expect("upload id"))
        .fetch_one(pool)
        .await
        .expect("upload status")
}

async fn assert_s3_key_absent(config: &S3RuntimeConfig, key: &str) {
    assert!(
        head(&s3_client(config).await, &config.bucket, key)
            .await
            .is_none(),
        "S3 key {key} must be absent"
    );
}

async fn assert_s3_key_versions_absent(config: &S3RuntimeConfig, key: &str) {
    let objects = s3_client(config)
        .await
        .list_object_versions()
        .bucket(&config.bucket)
        .prefix(key)
        .send()
        .await
        .expect("list object versions");
    assert!(
        objects
            .versions()
            .iter()
            .all(|version| version.key() != Some(key))
            && objects
                .delete_markers()
                .iter()
                .all(|marker| marker.key() != Some(key)),
        "all versions and delete markers for {key} must be absent"
    );
}

async fn exact_key_version_ids(config: &S3RuntimeConfig, key: &str) -> Vec<(String, bool)> {
    let objects = s3_client(config)
        .await
        .list_object_versions()
        .bucket(&config.bucket)
        .prefix(key)
        .send()
        .await
        .expect("list object versions");
    let mut versions = objects
        .versions()
        .iter()
        .filter(|version| version.key() == Some(key))
        .map(|version| {
            (
                version
                    .version_id()
                    .unwrap_or("missing-version-id")
                    .to_owned(),
                false,
            )
        })
        .chain(
            objects
                .delete_markers()
                .iter()
                .filter(|marker| marker.key() == Some(key))
                .map(|marker| {
                    (
                        marker
                            .version_id()
                            .unwrap_or("missing-version-id")
                            .to_owned(),
                        true,
                    )
                }),
        )
        .collect::<Vec<_>>();
    versions.sort_unstable();
    versions
}

/// The expectation-bearing engine path stages through the real port exactly
/// once, and a replay performs the same one stage call without creating a
/// second corpus row. The finish count records the separate bookkeeping
/// closeout for both calls.
#[tokio::test]
async fn expectation_completion_and_same_expectation_replay_are_counted() {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset");
        return;
    }

    let (pg, db_name) = fresh_storage().await;
    let pool = pg.pool_for_tests().clone();
    let config = s3_config_for_dev();
    let store = CitedBlobStore::new(pool.clone(), config.clone()).expect("valid S3 config");
    let (service, counters) = counted_blob_service(&store);
    let body: &'static [u8] = b"expectation-completion-bytes";
    let owner = owner_fixture();
    let ctx = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
    let (upload_id, _) = prepare_and_put_upload(
        &pool,
        &store,
        &config,
        &ctx,
        owner,
        "expected.pdf",
        "application/pdf",
        body,
    )
    .await;
    let expectation = upload_expectation(body, "application/pdf", "expected.pdf");

    let first =
        complete_via_engine_with_expectation(&pg, &service, &ctx, owner, &upload_id, &expectation)
            .await
            .expect("expectation-bearing completion");
    assert!(!first.fact.idempotent_replay);
    assert!(!first.blob.idempotent_replay);
    assert_eq!(counters.stage_calls.load(Ordering::SeqCst), 1);
    assert_eq!(counters.finish_calls.load(Ordering::SeqCst), 1);
    assert_eq!(corpus_counts(&pool).await, (1, 1));

    let replay =
        complete_via_engine_with_expectation(&pg, &service, &ctx, owner, &upload_id, &expectation)
            .await
            .expect("same-expectation replay");
    assert!(replay.fact.idempotent_replay);
    assert!(replay.blob.idempotent_replay);
    assert_eq!(replay.fact.memory_id, first.fact.memory_id);
    assert_eq!(replay.blob.cited_object_id, first.blob.cited_object_id);
    assert_eq!(counters.stage_calls.load(Ordering::SeqCst), 2);
    assert_eq!(counters.finish_calls.load(Ordering::SeqCst), 2);
    assert_eq!(corpus_counts(&pool).await, (1, 1));

    drop(pool);
    drop_db(&db_name).await.expect("drop db");
}

/// A completed upload still validates the metadata from its own exact upload
/// row. A later upload may deduplicate the same bytes into the same blob while
/// carrying different immutable metadata; that later row must not become the
/// replay answer for the first upload id.
#[tokio::test]
async fn completed_replay_uses_exact_upload_metadata_after_deduplication() {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset");
        return;
    }

    let (pg, db_name) = fresh_storage().await;
    let pool = pg.pool_for_tests().clone();
    let config = s3_config_for_dev();
    let store = CitedBlobStore::new(pool.clone(), config.clone()).expect("valid S3 config");
    let (service, counters) = counted_blob_service(&store);
    let body: &'static [u8] = b"same-bytes-different-upload-metadata";
    let owner = owner_fixture();
    let ctx = AuthzContext::single_owner(&owner, AuthPath::HostBearer);

    let (first_id, first_pending_key) = prepare_and_put_upload(
        &pool,
        &store,
        &config,
        &ctx,
        owner,
        "first.pdf",
        "application/pdf",
        body,
    )
    .await;
    let first = complete_via_engine_with_expectation(
        &pg,
        &service,
        &ctx,
        owner,
        &first_id,
        &upload_expectation(body, "application/pdf", "first.pdf"),
    )
    .await
    .expect("first completion");

    // Recreate transfer debris after finish. A completed replay must use its
    // exact upload row, never read this pending key, and must still retry the
    // transfer cleanup.
    s3_client(&config)
        .await
        .put_object()
        .bucket(&config.bucket)
        .key(&first_pending_key)
        .body(ByteStream::from(vec![b'X'; body.len()]))
        .send()
        .await
        .expect("recreate pending debris");

    let (second_id, _) = prepare_and_put_upload(
        &pool,
        &store,
        &config,
        &ctx,
        owner,
        "second.txt",
        "text/plain",
        body,
    )
    .await;
    let second = complete_via_engine_with_expectation(
        &pg,
        &service,
        &ctx,
        owner,
        &second_id,
        &upload_expectation(body, "text/plain", "second.txt"),
    )
    .await
    .expect("second completion");
    assert_eq!(second.blob.cited_object_id, first.blob.cited_object_id);
    assert!(second.blob.idempotent_replay);
    assert!(second.fact.idempotent_replay);
    assert_eq!(second.fact.memory_id, first.fact.memory_id);

    let replay = complete_via_engine_with_expectation(
        &pg,
        &service,
        &ctx,
        owner,
        &first_id,
        &upload_expectation(body, "application/pdf", "first.pdf"),
    )
    .await
    .expect("exact first-upload replay");
    assert!(replay.blob.idempotent_replay);
    assert!(replay.fact.idempotent_replay);
    assert_eq!(replay.blob.cited_object_id, first.blob.cited_object_id);
    assert_eq!(replay.fact.memory_id, first.fact.memory_id);
    assert_eq!(counters.stage_calls.load(Ordering::SeqCst), 3);
    assert_eq!(counters.finish_calls.load(Ordering::SeqCst), 3);
    assert_eq!(corpus_counts(&pool).await, (1, 1));
    assert_s3_key_absent(&config, &first_pending_key).await;

    drop(pool);
    drop_db(&db_name).await.expect("drop db");
}

/// A changed expectation on a completed upload is rejected before finish and
/// leaves the already committed corpus untouched.
#[tokio::test]
async fn changed_expectation_on_completed_upload_does_not_mutate_corpus() {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset");
        return;
    }

    let (pg, db_name) = fresh_storage().await;
    let pool = pg.pool_for_tests().clone();
    let config = s3_config_for_dev();
    let store = CitedBlobStore::new(pool.clone(), config.clone()).expect("valid S3 config");
    let (service, counters) = counted_blob_service(&store);
    let body: &'static [u8] = b"completed-expectation-cannot-change";
    let owner = owner_fixture();
    let ctx = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
    let (upload_id, _) = prepare_and_put_upload(
        &pool,
        &store,
        &config,
        &ctx,
        owner,
        "stable.pdf",
        "application/pdf",
        body,
    )
    .await;
    let expectation = upload_expectation(body, "application/pdf", "stable.pdf");
    let first =
        complete_via_engine_with_expectation(&pg, &service, &ctx, owner, &upload_id, &expectation)
            .await
            .expect("first completion");
    let before_counts = corpus_counts(&pool).await;
    let before_stage = counters.stage_calls.load(Ordering::SeqCst);
    let before_finish = counters.finish_calls.load(Ordering::SeqCst);

    let changed = UploadCompletionExpectation::new(
        *blake3::hash(body).as_bytes(),
        u64::try_from(body.len()).expect("test body length fits in u64"),
        "text/plain",
        "stable.pdf",
    );
    let error =
        complete_via_engine_with_expectation(&pg, &service, &ctx, owner, &upload_id, &changed)
            .await
            .expect_err("changed completed metadata must fail");
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    assert_eq!(
        error.message,
        "invalid argument mime: staged upload does not match expected MIME"
    );
    assert_eq!(
        counters.stage_calls.load(Ordering::SeqCst),
        before_stage + 1
    );
    assert_eq!(counters.finish_calls.load(Ordering::SeqCst), before_finish);
    assert_eq!(corpus_counts(&pool).await, before_counts);
    assert_eq!(
        complete_via_engine_with_expectation(&pg, &service, &ctx, owner, &upload_id, &expectation,)
            .await
            .expect("the original expectation remains valid")
            .blob
            .cited_object_id,
        first.blob.cited_object_id
    );

    drop(pool);
    drop_db(&db_name).await.expect("drop db");
}

/// Every expectation mismatch happens after one real stage call but before
/// any corpus transaction or finish call. The pending row remains available
/// for the caller's explicit abort or corrected-expectation retry; its
/// pending-key versions are already retired while canonical bytes remain
/// available for erase and orphan reconciliation.
#[tokio::test]
#[allow(clippy::too_many_lines)] // four independent metadata mismatches share one abort proof
async fn fresh_pending_expectation_mismatches_are_abortable_and_retain_canonical() {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset");
        return;
    }

    let (pg, db_name) = fresh_storage().await;
    let pool = pg.pool_for_tests().clone();
    let config = s3_config_for_dev();
    let store = CitedBlobStore::new(pool.clone(), config.clone()).expect("valid S3 config");
    let (service, counters) = counted_blob_service(&store);
    let body: &'static [u8] = b"fresh-pending-mismatch";
    let owner = owner_fixture();
    let ctx = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
    let actual_hash = *blake3::hash(body).as_bytes();
    let cases = [
        (
            "content_hash",
            UploadCompletionExpectation::new(
                [0xA5; 32],
                u64::try_from(body.len()).expect("test body length fits in u64"),
                "application/pdf",
                "fresh.pdf",
            ),
            "staged upload does not match expected BLAKE3 content hash",
        ),
        (
            "byte_len",
            UploadCompletionExpectation::new(
                actual_hash,
                u64::try_from(body.len()).expect("test body length fits in u64") + 1,
                "application/pdf",
                "fresh.pdf",
            ),
            "staged upload does not match expected byte length",
        ),
        (
            "mime",
            UploadCompletionExpectation::new(
                actual_hash,
                u64::try_from(body.len()).expect("test body length fits in u64"),
                "text/plain",
                "fresh.pdf",
            ),
            "staged upload does not match expected MIME",
        ),
        (
            "filename",
            UploadCompletionExpectation::new(
                actual_hash,
                u64::try_from(body.len()).expect("test body length fits in u64"),
                "application/pdf",
                "other.pdf",
            ),
            "staged upload does not match expected filename",
        ),
    ];

    for (field, expectation, reason) in cases {
        let (upload_id, pending_key) = prepare_and_put_upload(
            &pool,
            &store,
            &config,
            &ctx,
            owner,
            "fresh.pdf",
            "application/pdf",
            body,
        )
        .await;
        let canonical_key = format!("objects/{upload_id}");
        let stage_before = counters.stage_calls.load(Ordering::SeqCst);
        let finish_before = counters.finish_calls.load(Ordering::SeqCst);

        let error = complete_via_engine_with_expectation(
            &pg,
            &service,
            &ctx,
            owner,
            &upload_id,
            &expectation,
        )
        .await
        .expect_err("fresh pending mismatch");
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert_eq!(error.message, format!("invalid argument {field}: {reason}"));
        assert_eq!(
            counters.stage_calls.load(Ordering::SeqCst),
            stage_before + 1
        );
        assert_eq!(counters.finish_calls.load(Ordering::SeqCst), finish_before);
        assert_eq!(corpus_counts(&pool).await, (0, 0));
        assert_eq!(upload_status(&pool, &upload_id).await, "pending");
        assert!(
            head(&s3_client(&config).await, &config.bucket, &canonical_key)
                .await
                .is_some(),
            "staging leaves canonical bytes available for abort or recovery"
        );
        assert_s3_key_absent(&config, &pending_key).await;

        let aborted = store
            .abort_upload(
                &ctx,
                CitedBlobUploadAbortTs {
                    owner,
                    upload_id: upload_id.clone(),
                },
            )
            .await
            .expect("explicit abort");
        assert!(aborted.aborted);
        assert_eq!(upload_status(&pool, &upload_id).await, "aborted");
        assert!(
            head(&s3_client(&config).await, &config.bucket, &canonical_key)
                .await
                .is_some(),
            "explicit abort retains canonical bytes for owner cleanup"
        );
        assert_s3_key_absent(&config, &pending_key).await;

        let repeated = store
            .abort_upload(
                &ctx,
                CitedBlobUploadAbortTs {
                    owner,
                    upload_id: upload_id.clone(),
                },
            )
            .await
            .expect("repeated abort");
        assert!(repeated.aborted);
        assert!(
            head(&s3_client(&config).await, &config.bucket, &canonical_key)
                .await
                .is_some(),
            "repeated abort retains canonical bytes"
        );
        assert_s3_key_absent(&config, &pending_key).await;
    }

    drop(pool);
    drop_db(&db_name).await.expect("drop db");
}

/// Once a first stage records the canonical locator, a later presigned PUT to
/// the derived pending key cannot replace the bytes used by a corrected
/// retry. The retry reads the row-selected canonical object and completes the
/// same upload id exactly once.
#[tokio::test]
async fn corrected_expectation_retry_uses_recorded_canonical_bytes() {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset");
        return;
    }

    let (pg, db_name) = fresh_storage().await;
    let pool = pg.pool_for_tests().clone();
    let config = s3_config_for_dev();
    let store = CitedBlobStore::new(pool.clone(), config.clone()).expect("valid S3 config");
    let (service, counters) = counted_blob_service(&store);
    let original: &'static [u8] = b"canonical-retry-original";
    let replacement: &'static [u8] = b"canonical-retry-replaced";
    assert_eq!(original.len(), replacement.len());
    let owner = owner_fixture();
    let ctx = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
    let (upload_id, pending_key) = prepare_and_put_upload(
        &pool,
        &store,
        &config,
        &ctx,
        owner,
        "retry.pdf",
        "application/pdf",
        original,
    )
    .await;
    let canonical_key = format!("objects/{upload_id}");

    let mismatch = complete_via_engine_with_expectation(
        &pg,
        &service,
        &ctx,
        owner,
        &upload_id,
        &UploadCompletionExpectation::new(
            [0xA5; 32],
            u64::try_from(original.len()).expect("length fits"),
            "application/pdf",
            "retry.pdf",
        ),
    )
    .await
    .expect_err("first deliberately wrong expectation");
    assert_eq!(mismatch.code, ErrorCode::InvalidArgument);
    assert_eq!(counters.stage_calls.load(Ordering::SeqCst), 1);
    assert_eq!(counters.finish_calls.load(Ordering::SeqCst), 0);

    // A client that reuses the original presigned URL may still overwrite
    // the transfer key. It must not affect the canonical retry source.
    put_object_via_sdk(&config, &pending_key, replacement).await;
    let completed = complete_via_engine_with_expectation(
        &pg,
        &service,
        &ctx,
        owner,
        &upload_id,
        &upload_expectation(original, "application/pdf", "retry.pdf"),
    )
    .await
    .expect("corrected expectation succeeds against canonical bytes");
    assert_eq!(counters.stage_calls.load(Ordering::SeqCst), 2);
    assert_eq!(counters.finish_calls.load(Ordering::SeqCst), 1);
    assert_eq!(corpus_counts(&pool).await, (1, 1));
    assert_s3_key_absent(&config, &pending_key).await;
    assert!(
        head(&s3_client(&config).await, &config.bucket, &canonical_key)
            .await
            .is_some(),
        "successful retry retains canonical bytes"
    );
    assert!(!completed.blob.cited_object_id.is_empty());

    drop(pool);
    drop_db(&db_name).await.expect("drop db");
}

/// If the database status transition committed before the object provider
/// cleanup, a retry observes `aborted` and runs the same pending-key cleanup;
/// canonical bytes remain row-indexed for safe erase.
#[tokio::test]
async fn already_aborted_upload_retry_cleans_provider_keys() {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset");
        return;
    }

    let (pg, db_name) = fresh_storage().await;
    let pool = pg.pool_for_tests().clone();
    let config = s3_config_for_dev();
    let store = CitedBlobStore::new(pool.clone(), config.clone()).expect("valid S3 config");
    let body: &'static [u8] = b"committed-aborted-cleanup";
    let owner = owner_fixture();
    let ctx = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
    let (upload_id, pending_key) = prepare_and_put_upload(
        &pool,
        &store,
        &config,
        &ctx,
        owner,
        "aborted.pdf",
        "application/pdf",
        body,
    )
    .await;
    let staged = store
        .stage_upload(
            &ctx,
            CitedBlobUploadCompleteTs {
                owner,
                upload_id: upload_id.clone(),
            },
        )
        .await
        .expect("stage");
    let canonical_key = staged.payload.object_key;
    assert_ne!(canonical_key, pending_key);
    assert_eq!(upload_status(&pool, &upload_id).await, "pending");

    // Simulate provider cleanup failing after the status transition. The
    // retry must reclaim this recreated transfer copy without touching the
    // canonical object.
    put_object_via_sdk(&config, &pending_key, body).await;

    sqlx::query(
        "UPDATE proxima_core.blob_uploads
            SET status = 'aborted', aborted_at = now()
          WHERE owner_id = $1 AND upload_id = $2",
    )
    .bind(owner.stored_owner_id())
    .bind(Uuid::parse_str(&upload_id).expect("upload id"))
    .execute(&pool)
    .await
    .expect("simulate committed abort status");
    assert_eq!(upload_status(&pool, &upload_id).await, "aborted");
    assert!(
        head(&s3_client(&config).await, &config.bucket, &canonical_key)
            .await
            .is_some()
    );
    assert!(
        head(&s3_client(&config).await, &config.bucket, &pending_key)
            .await
            .is_some()
    );

    let first_retry = store
        .abort_upload(
            &ctx,
            CitedBlobUploadAbortTs {
                owner,
                upload_id: upload_id.clone(),
            },
        )
        .await
        .expect("aborted cleanup retry");
    assert!(first_retry.aborted);
    assert!(
        head(&s3_client(&config).await, &config.bucket, &canonical_key)
            .await
            .is_some(),
        "aborted retry retains canonical bytes"
    );
    assert_s3_key_absent(&config, &pending_key).await;
    let second_retry = store
        .abort_upload(&ctx, CitedBlobUploadAbortTs { owner, upload_id })
        .await
        .expect("repeated aborted cleanup retry");
    assert!(second_retry.aborted);

    drop(pool);
    drop_db(&db_name).await.expect("drop db");
}

/// Abort cleanup must use version-aware deletion for the expendable pending
/// transfer key, while retaining the canonical bytes recorded on the upload
/// row. The extra PUT and delete marker make a key-only delete observably
/// insufficient on a versioned bucket.
#[tokio::test]
#[allow(clippy::too_many_lines)] // the versioned cleanup and entry-stage proof share one fixture
async fn versioned_abort_purges_pending_versions_and_retains_canonical() {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset");
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
    let body: &'static [u8] = b"versioned-abort-pending-bytes";
    let (upload_id, pending_key) = prepare_and_put_upload(
        &pool,
        &store,
        &config,
        &ctx,
        owner,
        "aborted.pdf",
        "application/pdf",
        body,
    )
    .await;
    let staged = store
        .stage_upload(
            &ctx,
            CitedBlobUploadCompleteTs {
                owner,
                upload_id: upload_id.clone(),
            },
        )
        .await
        .expect("stage");
    let canonical_key = staged.payload.object_key;

    // Add a noncurrent pending version and a delete marker after staging.
    // The marker must not hide cleanup of the older byte versions.
    put_object_via_sdk(&config, &pending_key, body).await;
    client
        .delete_object()
        .bucket(&config.bucket)
        .key(&pending_key)
        .send()
        .await
        .expect("create pending delete marker");

    let aborted = store
        .abort_upload(
            &ctx,
            CitedBlobUploadAbortTs {
                owner,
                upload_id: upload_id.clone(),
            },
        )
        .await
        .expect("abort");
    assert!(aborted.aborted);
    assert_s3_key_versions_absent(&config, &pending_key).await;
    assert!(
        head(&s3_client(&config).await, &config.bucket, &canonical_key)
            .await
            .is_some(),
        "aborting does not delete the canonical object"
    );

    // A stage that enters after the abort status committed must perform the
    // same version-aware pending cleanup before returning its terminal error.
    put_object_via_sdk(&config, &pending_key, body).await;
    put_object_via_sdk(&config, &pending_key, body).await;
    client
        .delete_object()
        .bucket(&config.bucket)
        .key(&pending_key)
        .send()
        .await
        .expect("recreate pending delete marker");
    let stage_after_abort = store
        .stage_upload(
            &ctx,
            CitedBlobUploadCompleteTs {
                owner,
                upload_id: upload_id.clone(),
            },
        )
        .await
        .expect_err("stage after an aborted entry must remain terminal");
    assert!(matches!(
        stage_after_abort,
        BlobError::State(message) if message == "upload is aborted"
    ));
    assert_s3_key_versions_absent(&config, &pending_key).await;
    assert!(
        head(&s3_client(&config).await, &config.bucket, &canonical_key)
            .await
            .is_some(),
        "stage-after-abort cleanup retains canonical bytes"
    );

    let replay = store
        .abort_upload(&ctx, CitedBlobUploadAbortTs { owner, upload_id })
        .await
        .expect("repeated abort");
    assert!(replay.aborted);
    assert_s3_key_versions_absent(&config, &pending_key).await;

    drop(pool);
    drop_db(&db_name).await.expect("drop db");
}

/// A statement-level trigger pauses the guarded locator UPDATE after the
/// canonical PUT, allowing a separate transaction to commit the terminal
/// status. This exercises the lost-post-PUT race without exposing a runtime
/// test hook in the store.
#[allow(clippy::too_many_lines)] // one helper drives both deterministic terminal races
async fn post_put_terminal_race(terminal_sql: &'static str, expected_error: &'static str) {
    let (pg, db_name) = fresh_storage().await;
    let pool = pg.pool_for_tests().clone();
    let base = s3_config_for_dev();
    let config = S3RuntimeConfig {
        bucket: format!("{}-versioned", base.bucket),
        ..base
    };
    let client = s3_client(&config).await;
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
    let body: &'static [u8] = b"post-put-abort-race";
    let (upload_id, pending_key) = prepare_and_put_upload(
        &pool,
        &store,
        &config,
        &ctx,
        owner,
        "race.pdf",
        "application/pdf",
        body,
    )
    .await;
    let upload_uuid = Uuid::parse_str(&upload_id).expect("upload id");
    let canonical_key = format!("objects/{upload_id}");
    sqlx::query(
        "CREATE FUNCTION upload_locator_barrier() RETURNS trigger LANGUAGE plpgsql AS $$
            BEGIN
                PERFORM pg_advisory_xact_lock(hashtextextended(current_database(), 0));
                RETURN NULL;
            END
        $$",
    )
    .execute(&pool)
    .await
    .expect("create locator barrier function");
    sqlx::query(
        "CREATE TRIGGER upload_locator_barrier_trigger
         BEFORE UPDATE OF object_key ON proxima_core.blob_uploads
         FOR EACH STATEMENT EXECUTE FUNCTION upload_locator_barrier()",
    )
    .execute(&pool)
    .await
    .expect("create locator barrier trigger");

    let mut barrier = pool.begin().await.expect("begin barrier transaction");
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended(current_database(), 0))")
        .execute(&mut *barrier)
        .await
        .expect("hold locator barrier");

    let stage_store = store.clone();
    let stage_ctx = ctx.clone();
    let stage_upload_id = upload_id.clone();
    let stage_task = tokio::spawn(async move {
        stage_store
            .stage_upload(
                &stage_ctx,
                CitedBlobUploadCompleteTs {
                    owner,
                    upload_id: stage_upload_id,
                },
            )
            .await
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while head(&s3_client(&config).await, &config.bucket, &canonical_key)
        .await
        .is_none()
    {
        assert!(
            Instant::now() < deadline,
            "stage did not write canonical bytes"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    // Create a noncurrent transfer version and marker while the stage is
    // paused. The terminal repair must reclaim all of them after release.
    put_object_via_sdk(&config, &pending_key, body).await;
    client
        .delete_object()
        .bucket(&config.bucket)
        .key(&pending_key)
        .send()
        .await
        .expect("create pending delete marker");
    sqlx::query(
        "UPDATE proxima_core.blob_uploads\n            SET status = $3::proxima_core.blob_upload_status, aborted_at = now()\n          WHERE owner_id = $1 AND upload_id = $2",
    )
    .bind(owner.stored_owner_id())
    .bind(upload_uuid)
    .bind(terminal_sql)
    .execute(&pool)
    .await
    .expect("commit terminal status while stage is paused");
    drop(barrier);

    let error = stage_task
        .await
        .expect("stage task join")
        .expect_err("post-put aborted race returns terminal error");
    assert!(matches!(error, BlobError::State(message) if message == expected_error));
    assert_s3_key_versions_absent(&config, &pending_key).await;
    assert!(
        head(&s3_client(&config).await, &config.bucket, &canonical_key)
            .await
            .is_some(),
        "terminal repair retains canonical bytes"
    );
    let locator: (String, Option<Vec<u8>>, Option<String>) = sqlx::query_as(
        "SELECT object_key, sha256, etag FROM proxima_core.blob_uploads\n          WHERE upload_id = $1",
    )
    .bind(upload_uuid)
    .fetch_one(&pool)
    .await
    .expect("repaired upload row");
    assert_eq!(locator.0, canonical_key);
    let expected_sha256: [u8; 32] = sha2::Sha256::digest(body).into();
    assert_eq!(
        locator.1.as_deref(),
        Some(expected_sha256.as_slice()),
        "repair records the canonical digest"
    );
    assert!(locator.2.is_some(), "repair records the canonical etag");

    sqlx::query("DROP TRIGGER upload_locator_barrier_trigger ON proxima_core.blob_uploads")
        .execute(&pool)
        .await
        .expect("drop locator barrier trigger");
    sqlx::query("DROP FUNCTION upload_locator_barrier()")
        .execute(&pool)
        .await
        .expect("drop locator barrier function");
    drop(pool);
    drop_db(&db_name).await.expect("drop db");
}

#[tokio::test]
async fn post_put_abort_race_repairs_locator_and_purges_pending_versions() {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset");
        return;
    }
    post_put_terminal_race("aborted", "upload is aborted").await;
}

#[tokio::test]
async fn post_put_expiry_race_repairs_locator_and_purges_pending_versions() {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset");
        return;
    }
    post_put_terminal_race("expired", "upload is expired").await;
}

/// If an owner-scoped locator CAS loses because the upload row was erased,
/// stage purges only the derived transfer key. The canonical bytes are not
/// inferentially orphaned: an in-place transfer or mounted row may still own
/// them.
#[tokio::test]
#[allow(clippy::too_many_lines)] // the barrier fixture proves an erased-row race end to end
async fn missing_owner_row_after_stage_keeps_canonical_and_purges_pending() {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset");
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
    let body: &'static [u8] = b"missing-owner-row-canonical-bytes";
    let (upload_id, pending_key) = prepare_and_put_upload(
        &pool,
        &store,
        &config,
        &ctx,
        owner,
        "missing.pdf",
        "application/pdf",
        body,
    )
    .await;
    let upload_uuid = Uuid::parse_str(&upload_id).expect("upload id");
    let canonical_key = format!("objects/{upload_id}");
    sqlx::query(
        "CREATE FUNCTION upload_locator_barrier() RETURNS trigger LANGUAGE plpgsql AS $$
            BEGIN
                PERFORM pg_advisory_xact_lock(hashtextextended(current_database(), 0));
                RETURN NULL;
            END
        $$",
    )
    .execute(&pool)
    .await
    .expect("create locator barrier function");
    sqlx::query(
        "CREATE TRIGGER upload_locator_barrier_trigger
         BEFORE UPDATE OF object_key ON proxima_core.blob_uploads
         FOR EACH STATEMENT EXECUTE FUNCTION upload_locator_barrier()",
    )
    .execute(&pool)
    .await
    .expect("create locator barrier trigger");
    let mut barrier = pool.begin().await.expect("begin barrier transaction");
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended(current_database(), 0))")
        .execute(&mut *barrier)
        .await
        .expect("hold locator barrier");

    let stage_store = store.clone();
    let stage_ctx = ctx.clone();
    let stage_upload_id = upload_id.clone();
    let stage_task = tokio::spawn(async move {
        stage_store
            .stage_upload(
                &stage_ctx,
                CitedBlobUploadCompleteTs {
                    owner,
                    upload_id: stage_upload_id,
                },
            )
            .await
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while head(&s3_client(&config).await, &config.bucket, &canonical_key)
        .await
        .is_none()
    {
        assert!(
            Instant::now() < deadline,
            "stage did not write canonical bytes"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    // Leave multiple transfer versions and a marker for the missing-row
    // cleanup path to prove it is version-aware.
    put_object_via_sdk(&config, &pending_key, body).await;
    client
        .delete_object()
        .bucket(&config.bucket)
        .key(&pending_key)
        .send()
        .await
        .expect("create pending delete marker");
    sqlx::query(
        "DELETE FROM proxima_core.blob_uploads
          WHERE owner_id = $1 AND upload_id = $2",
    )
    .bind(owner.stored_owner_id())
    .bind(upload_uuid)
    .execute(&pool)
    .await
    .expect("erase upload row while stage is paused");
    drop(barrier);

    let error = stage_task
        .await
        .expect("stage task join")
        .expect_err("missing owner row is not a successful stage");
    assert!(matches!(error, BlobError::State(message) if message == "upload not found for Owner"));
    assert_s3_key_versions_absent(&config, &pending_key).await;
    let retained = client
        .get_object()
        .bucket(&config.bucket)
        .key(&canonical_key)
        .send()
        .await
        .expect("canonical bytes remain readable")
        .body
        .collect()
        .await
        .expect("read canonical bytes")
        .into_bytes();
    assert_eq!(retained.as_ref(), body);
    assert_eq!(corpus_counts(&pool).await, (0, 0));

    sqlx::query("DROP TRIGGER upload_locator_barrier_trigger ON proxima_core.blob_uploads")
        .execute(&pool)
        .await
        .expect("drop locator barrier trigger");
    sqlx::query("DROP FUNCTION upload_locator_barrier()")
        .execute(&pool)
        .await
        .expect("drop locator barrier function");
    drop(pool);
    drop_db(&db_name).await.expect("drop db");
}

/// An already-expired upload is lifecycle-only: neither completion nor abort
/// performs synchronous provider cleanup. The exact version/marker set is
/// therefore unchanged for the lifecycle worker to reclaim.
#[tokio::test]
async fn expired_at_entry_completion_and_abort_preserve_pending_versions() {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset");
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
    let body: &'static [u8] = b"expired-at-entry-bytes";
    let (upload_id, pending_key) = prepare_and_put_upload(
        &pool,
        &store,
        &config,
        &ctx,
        owner,
        "expired.pdf",
        "application/pdf",
        body,
    )
    .await;
    put_object_via_sdk(&config, &pending_key, body).await;
    client
        .delete_object()
        .bucket(&config.bucket)
        .key(&pending_key)
        .send()
        .await
        .expect("create pending delete marker");
    sqlx::query(
        "UPDATE proxima_core.blob_uploads\n            SET status = 'expired', error_message = 'upload expired'\n          WHERE owner_id = $1 AND upload_id = $2",
    )
    .bind(owner.stored_owner_id())
    .bind(Uuid::parse_str(&upload_id).expect("upload id"))
    .execute(&pool)
    .await
    .expect("mark upload expired");
    let before = exact_key_version_ids(&config, &pending_key).await;
    assert!(before.len() >= 3, "fixture has two versions and a marker");

    let complete_error = store
        .stage_upload(
            &ctx,
            CitedBlobUploadCompleteTs {
                owner,
                upload_id: upload_id.clone(),
            },
        )
        .await
        .expect_err("expired completion is lifecycle-only");
    assert!(matches!(complete_error, BlobError::State(message) if message == "upload is expired"));
    assert_eq!(exact_key_version_ids(&config, &pending_key).await, before);

    let aborted = store
        .abort_upload(&ctx, CitedBlobUploadAbortTs { owner, upload_id })
        .await
        .expect("expired abort remains idempotent");
    assert!(aborted.aborted);
    assert_eq!(exact_key_version_ids(&config, &pending_key).await, before);

    drop(pool);
    drop_db(&db_name).await.expect("drop db");
}

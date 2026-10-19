use super::*;
use std::io;
use std::sync::Mutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::{JoinHandle, JoinSet};

const PATIENCE: Duration = Duration::from_secs(30);
const MAX_HEADER_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
struct ObservedRequest {
    method: String,
    host: String,
    query_signed: bool,
    header_signed: bool,
}

struct RecordingProxy {
    endpoint: String,
    observed: Arc<Mutex<Vec<ObservedRequest>>>,
    errors: Arc<Mutex<Vec<io::ErrorKind>>>,
    task: JoinHandle<()>,
}

impl Drop for RecordingProxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl RecordingProxy {
    async fn start(backend: &str) -> Self {
        let backend = reqwest::Url::parse(backend).expect("test S3 endpoint URL");
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("proxy listener");
        let address = listener.local_addr().expect("proxy address");
        let endpoint = format!("http://{address}{}", backend.path());
        let backend = backend.origin().ascii_serialization();
        let observed = Arc::new(Mutex::new(Vec::new()));
        let errors = Arc::new(Mutex::new(Vec::new()));
        let requests = observed.clone();
        let failures = errors.clone();
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let (stream, _) = accepted.expect("proxy accepts connection");
                        let backend = backend.clone();
                        let requests = requests.clone();
                        let failures = failures.clone();
                        connections.spawn(async move {
                            let result = tokio::time::timeout(
                                PATIENCE, forward_request(stream, &backend, &requests)
                            ).await;
                            let error = match result {
                                Ok(Ok(())) => None,
                                Ok(Err(error)) => Some(error.kind()),
                                Err(_) => Some(io::ErrorKind::TimedOut),
                            };
                            if let Some(error) = error {
                                failures.lock().expect("proxy failures").push(error);
                            }
                        });
                    }
                    completed = connections.join_next(), if !connections.is_empty() => {
                        completed.expect("completed connection").expect("proxy task");
                    }
                }
            }
        });
        Self {
            endpoint,
            observed,
            errors,
            task,
        }
    }

    fn requests(&self) -> Vec<ObservedRequest> {
        assert!(self.errors.lock().expect("proxy errors").is_empty());
        self.observed.lock().expect("proxy requests").clone()
    }
}

async fn forward_request(
    mut stream: TcpStream,
    backend: &str,
    observed: &Mutex<Vec<ObservedRequest>>,
) -> io::Result<()> {
    let mut received = Vec::new();
    let header_end = loop {
        if let Some(end) = received.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            break end + 4;
        }
        if received.len() >= MAX_HEADER_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "proxy header limit",
            ));
        }
        let mut buffer = [0_u8; 4096];
        let count = stream.read(&mut buffer).await?;
        if count == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        received.extend_from_slice(&buffer[..count]);
    };
    let header = std::str::from_utf8(&received[..header_end])
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "proxy header encoding"))?;
    let mut lines = header.split("\r\n");
    let mut request = lines.next().unwrap_or_default().split_whitespace();
    let method = request
        .next()
        .unwrap_or_default()
        .parse::<reqwest::Method>()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "proxy method"))?;
    let target = request.next().unwrap_or_default().to_owned();
    let mut headers = reqwest::header::HeaderMap::new();
    for line in lines.filter(|line| !line.is_empty()) {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "proxy header"))?;
        let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "proxy header name"))?;
        let value = reqwest::header::HeaderValue::from_str(value.trim())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "proxy header value"))?;
        headers.append(name, value);
    }
    let query_signed = target.to_ascii_lowercase().contains("x-amz-signature=");
    let header_signed = headers.contains_key(reqwest::header::AUTHORIZATION);
    observed
        .lock()
        .expect("proxy requests")
        .push(ObservedRequest {
            method: method.to_string(),
            host: headers
                .get(reqwest::header::HOST)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_owned(),
            query_signed,
            header_signed,
        });
    // Reject object I/O at this endpoint, making a misplaced SDK operation fail
    // rather than allowing it to pass behind a connection-count assertion.
    if !query_signed || header_signed || !matches!(method.as_str(), "PUT" | "GET") {
        stream
            .write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await?;
        return Ok(());
    }
    let content_length = headers
        .get(reqwest::header::CONTENT_LENGTH)
        .map(|value| {
            value
                .to_str()
                .ok()
                .and_then(|value| value.parse::<usize>().ok())
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "proxy content length"))
        })
        .transpose()?
        .unwrap_or(0);
    let mut body = received[header_end..].to_vec();
    body.resize(content_length, 0);
    let already_read = (received.len() - header_end).min(content_length);
    stream.read_exact(&mut body[already_read..]).await?;
    headers.remove(reqwest::header::CONNECTION);
    // The incoming Host, path, query and signed headers reach the real S3
    // verifier. Only the connection address differs.
    let response = reqwest::Client::new()
        .request(method, format!("{backend}{target}"))
        .headers(headers)
        .body(body)
        .send()
        .await
        .map_err(|_| io::Error::other("proxy upstream request failed"))?;
    write_response(stream, response).await
}

async fn write_response(mut stream: TcpStream, response: reqwest::Response) -> io::Result<()> {
    let status = response.status();
    let headers = response.headers().clone();
    let body = response
        .bytes()
        .await
        .map_err(|_| io::Error::other("proxy upstream response failed"))?;
    let mut response = format!(
        "HTTP/1.1 {} {}\r\nConnection: close\r\nContent-Length: {}\r\n",
        status.as_str(),
        status.canonical_reason().unwrap_or_default(),
        body.len()
    )
    .into_bytes();
    for (name, value) in &headers {
        if matches!(
            name.as_str(),
            "content-length" | "transfer-encoding" | "connection"
        ) {
            continue;
        }
        response.extend_from_slice(name.as_str().as_bytes());
        response.extend_from_slice(b": ");
        response.extend_from_slice(value.as_bytes());
        response.extend_from_slice(b"\r\n");
    }
    response.extend_from_slice(b"\r\n");
    response.extend_from_slice(&body);
    stream.write_all(&response).await?;
    stream.shutdown().await
}

async fn public_roundtrip(
    pg: &PgStorage,
    store: &CitedBlobStore,
    ctx: &AuthzContext,
    owner: OwnerId,
    body: &[u8],
    endpoint: &str,
) -> UploadCompleted {
    let prepared = store
        .prepare_upload(
            ctx,
            CitedBlobUploadPrepareTs {
                owner,
                filename: "public.pdf".into(),
                mime: "application/pdf".into(),
                byte_len: u64::try_from(body.len()).expect("fixture length"),
            },
        )
        .await
        .expect("prepare public upload");
    assert!(prepared.upload_url.starts_with(endpoint));
    let http = reqwest::Client::new();
    let mut request = http.put(&prepared.upload_url);
    for header in &prepared.headers {
        request = request.header(header.name.as_str(), header.value.as_str());
    }
    let response = request
        .body(body.to_vec())
        .send()
        .await
        .unwrap_or_else(|_| panic!("public PUT transport failed"));
    assert!(
        response.status().is_success(),
        "public PUT status {}",
        response.status()
    );
    let completed = complete_via_engine(pg, store, ctx, owner, &prepared.upload_id)
        .await
        .expect("stage and complete through internal endpoint");
    let download = store
        .read_url(
            ctx,
            CitedBlobReadUrlTs {
                owner,
                cited_object_id: completed.blob.cited_object_id.clone(),
            },
        )
        .await
        .expect("prepare public download");
    assert!(download.read_url.starts_with(endpoint));
    let response = http
        .get(&download.read_url)
        .send()
        .await
        .unwrap_or_else(|_| panic!("public GET transport failed"));
    assert!(
        response.status().is_success(),
        "public GET status {}",
        response.status()
    );
    assert_eq!(header(&response, "content-disposition"), "attachment");
    assert_eq!(
        header(&response, "content-type"),
        "application/octet-stream"
    );
    assert_eq!(
        response
            .bytes()
            .await
            .unwrap_or_else(|_| panic!("public GET body failed"))
            .as_ref(),
        body
    );
    completed
}

async fn signed_host_is_required(
    store: &CitedBlobStore,
    ctx: &AuthzContext,
    owner: OwnerId,
    object: &str,
    internal_endpoint: &str,
) {
    let download = store
        .read_url(
            ctx,
            CitedBlobReadUrlTs {
                owner,
                cited_object_id: object.to_owned(),
            },
        )
        .await
        .expect("prepare signature control");
    let internal = reqwest::Url::parse(internal_endpoint).expect("internal endpoint");
    let host = internal.host_str().expect("internal host");
    let host = internal
        .port()
        .map_or_else(|| host.to_owned(), |port| format!("{host}:{port}"));
    let response = reqwest::Client::new()
        .get(&download.read_url)
        .header(reqwest::header::HOST, host)
        .send()
        .await
        .unwrap_or_else(|_| panic!("signature control transport failed"));
    assert_eq!(
        response.status(),
        reqwest::StatusCode::FORBIDDEN,
        "the real S3 verifier must refuse a changed signed Host"
    );
}

async fn internal_operations(
    pg: &PgStorage,
    store: &CitedBlobStore,
    ctx: &AuthzContext,
    owner: OwnerId,
    object: Uuid,
    body: &[u8],
) {
    let verified = store
        .collect_verified(
            ctx,
            owner,
            object,
            NonZeroU64::new(u64::try_from(body.len()).expect("fixture length")).unwrap(),
        )
        .await
        .expect("internal verified read");
    assert_eq!(verified.bytes.as_slice(), body);
    let cold = store.cold_store();
    // The purge store only deletes (#488); an absent key is already purged.
    let key = format!("objects/public-endpoint-test/{}", Uuid::now_v7());
    cold.delete(&key).await.expect("internal cold DELETE");
    let reconciled = store
        .reconcile_owner(ctx, owner)
        .await
        .expect("internal reconcile");
    assert_eq!(reconciled.missing_objects, 0);
    erase_owner_and_drain(pg, store, owner).await;
}

#[tokio::test]
async fn public_links_carry_bytes_while_object_io_stays_internal() {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset");
        return;
    }
    let (pg, db_name) = fresh_storage().await;
    let configured = s3_config_for_dev();
    let proxy = RecordingProxy::start(
        configured
            .endpoint_url
            .as_deref()
            .expect("test S3 endpoint"),
    )
    .await;
    let config = S3RuntimeConfig::from_lookup(&|key| match key {
        "PROXIMA_S3_PUBLIC_ENDPOINT_URL" => Some(proxy.endpoint.clone()),
        "PROXIMA_S3_FORCE_PATH_STYLE" => Some("true".into()),
        "PROXIMA_S3_UPLOAD_TTL_SECONDS" => Some(configured.upload_ttl_seconds.to_string()),
        "PROXIMA_S3_READ_TTL_SECONDS" => Some(configured.read_ttl_seconds.to_string()),
        _ => std::env::var(key).ok(),
    })
    .expect("valid public endpoint setting")
    .expect("configured bucket");
    let store = CitedBlobStore::new(
        pg.clone_pool_for_backend(),
        config,
        pg.platform_scope_for_host(),
    )
    .expect("public endpoint config");
    let owner = owner_fixture();
    let ctx = proxima_core::test_fixtures::authenticated_context(AuthzContext::single_owner(
        &owner,
        AuthPath::HostBearer,
    ));
    let body = b"%PDF-1.7 public links, internal object operations";
    let completed = public_roundtrip(&pg, &store, &ctx, owner, body, &proxy.endpoint).await;
    let requests = proxy.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        (requests[0].method.as_str(), requests[1].method.as_str()),
        ("PUT", "GET")
    );
    let host = reqwest::Url::parse(&proxy.endpoint).expect("public endpoint");
    let host = format!(
        "{}:{}",
        host.host_str().expect("host"),
        host.port().expect("port")
    );
    assert!(
        requests
            .iter()
            .all(|request| request.host == host && request.query_signed && !request.header_signed)
    );
    signed_host_is_required(
        &store,
        &ctx,
        owner,
        &completed.blob.cited_object_id,
        configured
            .endpoint_url
            .as_deref()
            .expect("internal endpoint"),
    )
    .await;
    let requests = proxy.requests();
    assert_eq!(requests.len(), 3);
    assert_ne!(requests[2].host, host);
    let object = Uuid::parse_str(&completed.blob.cited_object_id).expect("cited object id");
    internal_operations(&pg, &store, &ctx, owner, object, body).await;
    assert_eq!(
        proxy.requests(),
        requests,
        "completed internal operations used no public requests"
    );
    drop_db(&db_name).await.expect("drop database");
}

//! The built-in HTTP sink against a local TLS receiver whose certificates
//! are minted per test (rcgen). No secret is a fixture: tokens are random
//! per run and live in a temp file.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use proxima_core::publication::intake::{DurableIntake, Intake, IntakeError, ReceivedEvent};
use rcgen::{BasicConstraints, CertificateParams, CertifiedIssuer, IsCa, KeyPair};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls;

use proxima_core::PlaintextHosts;

use super::{CONTENT_TYPE, DELIVERY_COUNT_HEADER, HttpIntake, MAX_REASON_CHARS, classify};

/// An intake that is never called: a registration-only fixture.
#[derive(Debug)]
pub(crate) struct Never;

#[async_trait::async_trait]
impl DurableIntake for Never {
    async fn accept(&self, _event: &ReceivedEvent) -> Result<Intake, IntakeError> {
        unreachable!("registration-only fixture")
    }
}

/// One request the receiver read in full.
#[derive(Debug, Clone)]
pub(crate) struct Seen {
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: Vec<u8>,
}

impl Seen {
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// What the receiver answers: a status, no answer at all, or a 422 whose
/// body never ends.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Reply {
    Status(u16),
    Hang,
    EndlessRejection,
}

/// A CA and the PEM of its certificate.
pub(crate) struct Ca {
    issuer: CertifiedIssuer<'static, KeyPair>,
}

impl Ca {
    pub(crate) fn new() -> Self {
        let mut params = CertificateParams::new(Vec::<String>::new()).expect("CA params");
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let issuer =
            CertifiedIssuer::self_signed(params, KeyPair::generate().expect("CA key")).expect("CA");
        Self { issuer }
    }

    pub(crate) fn pem(&self) -> String {
        self.issuer.pem()
    }

    fn server_config(&self) -> rustls::ServerConfig {
        let key = KeyPair::generate().expect("leaf key");
        let leaf = CertificateParams::new(vec!["localhost".to_owned()])
            .expect("leaf params")
            .signed_by(&key, &self.issuer)
            .expect("leaf cert");
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("TLS versions")
            .with_no_client_auth()
            .with_single_cert(
                vec![leaf.der().clone()],
                rustls::pki_types::PrivateKeyDer::Pkcs8(key.serialize_der().into()),
            )
            .expect("server config")
    }
}

/// A TLS receiver on `localhost`, answering `replies` in order (then 200).
pub(crate) struct Receiver {
    pub(crate) url: String,
    seen: Arc<Mutex<Vec<Seen>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Receiver {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Receiver {
    pub(crate) async fn start(ca: &Ca, replies: &[Reply]) -> Self {
        let acceptor = TlsAcceptor::from(Arc::new(ca.server_config()));
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let replies = Arc::new(Mutex::new(replies.iter().copied().collect::<VecDeque<_>>()));
        let task = {
            let seen = seen.clone();
            tokio::spawn(async move {
                loop {
                    let Ok((tcp, _)) = listener.accept().await else {
                        return;
                    };
                    let acceptor = acceptor.clone();
                    let seen = seen.clone();
                    let replies = replies.clone();
                    tokio::spawn(async move {
                        // A failed handshake (an untrusted cert) ends here:
                        // no request was ever read.
                        let Ok(mut tls) = acceptor.accept(tcp).await else {
                            return;
                        };
                        let Some(request) = read_request(&mut tls).await else {
                            return;
                        };
                        seen.lock().expect("seen").push(request);
                        let reply = replies
                            .lock()
                            .expect("replies")
                            .pop_front()
                            .unwrap_or(Reply::Status(200));
                        match reply {
                            Reply::Hang => std::future::pending::<()>().await,
                            Reply::EndlessRejection => {
                                let head = "HTTP/1.1 422 X\r\ntransfer-encoding: chunked\r\n\r\n";
                                if tls.write_all(head.as_bytes()).await.is_err() {
                                    return;
                                }
                                let chunk = format!("{:x}\r\n{}\r\n", 4096, "x".repeat(4096));
                                while tls.write_all(chunk.as_bytes()).await.is_ok() {}
                            }
                            Reply::Status(code) => {
                                let body = if code == 422 { "schema refused" } else { "" };
                                let response = format!(
                                    "HTTP/1.1 {code} X\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                                    body.len()
                                );
                                let _ = tls.write_all(response.as_bytes()).await;
                                let _ = tls.shutdown().await;
                            }
                        }
                    });
                }
            })
        };
        Self {
            url: format!("https://localhost:{port}/events"),
            seen,
            task,
        }
    }

    pub(crate) fn seen(&self) -> Vec<Seen> {
        self.seen.lock().expect("seen").clone()
    }
}

async fn read_request(stream: &mut (impl AsyncReadExt + Unpin)) -> Option<Seen> {
    let mut buf = Vec::new();
    let mut chunk = [0_u8; 4096];
    let head_end = loop {
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break end;
        }
    };
    let head = std::str::from_utf8(&buf[..head_end]).ok()?;
    let headers: Vec<(String, String)> = head
        .lines()
        .skip(1)
        .filter_map(|line| line.split_once(':'))
        .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
        .collect();
    let length: usize = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let mut body = buf[head_end + 4..].to_vec();
    while body.len() < length {
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    Some(Seen { headers, body })
}

/// A temp directory holding the CA PEM and the token file.
pub(crate) struct Files {
    dir: PathBuf,
}

impl Drop for Files {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Files {
    pub(crate) fn new(ca: &Ca) -> Self {
        let dir =
            std::env::temp_dir().join(format!("proxima-outbox-http-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        std::fs::write(dir.join("ca.pem"), ca.pem()).expect("CA file");
        Self { dir }
    }

    pub(crate) fn ca(&self) -> PathBuf {
        self.dir.join("ca.pem")
    }

    pub(crate) fn token(&self) -> PathBuf {
        self.dir.join("token")
    }

    /// A fresh random token, written to the token file.
    pub(crate) fn rotate(&self) -> String {
        let token = uuid::Uuid::new_v4().simple().to_string();
        std::fs::write(self.token(), format!("{token}\n")).expect("token file");
        token
    }
}

pub(crate) fn intake(receiver: &Receiver, files: &Files) -> HttpIntake {
    HttpIntake::new(
        &receiver.url,
        Some(files.token()),
        Some(&files.ca()),
        &PlaintextHosts::none(),
    )
    .expect("sink builds")
}

/// A token goes over `http://` only where every credential lane allows it:
/// loopback, or a host `PROXIMA_PLAINTEXT_HOSTS` names exactly.
#[test]
fn a_token_goes_over_http_only_to_loopback_or_a_named_host() {
    let token = || Some(PathBuf::from("/nonexistent/token"));
    let none = PlaintextHosts::none();
    let named = PlaintextHosts::parse("gateway").expect("a host name");
    for (url, hosts) in [
        ("http://gateway:9090/events", &named),
        ("http://GATEWAY:9090/events", &named),
        ("http://127.0.0.1:9090/events", &none),
        ("http://localhost/events", &named),
    ] {
        HttpIntake::new(url, token(), None, hosts)
            .unwrap_or_else(|error| panic!("{url} with {hosts:?}: {error}"));
    }
    for (url, hosts) in [
        ("http://gateway:9090/events", &none),
        ("http://other:9090/events", &named),
        ("http://gateway.example:9090/events", &named),
        ("http://gateway@other:9090/events", &named),
    ] {
        let error =
            HttpIntake::new(url, token(), None, hosts).expect_err(&format!("{url} with {hosts:?}"));
        assert!(
            error.to_string().contains("PROXIMA_PLAINTEXT_HOSTS"),
            "{url}: {error}"
        );
    }
}

#[test]
fn tokenless_http_is_only_allowed_to_loopback_or_a_named_host() {
    let none = PlaintextHosts::none();
    let named = PlaintextHosts::parse("gateway").expect("a host name");
    for (url, hosts) in [
        ("http://127.0.0.1:9090/events", &none),
        ("http://[::1]:9090/events", &none),
        ("http://localhost/events", &none),
        ("http://gateway:9090/events", &named),
    ] {
        HttpIntake::new(url, None, None, hosts)
            .unwrap_or_else(|error| panic!("{url} with {hosts:?}: {error}"));
    }
    for (url, hosts) in [
        ("http://gateway:9090/events", &none),
        ("http://other:9090/events", &named),
    ] {
        let error = HttpIntake::new(url, None, None, hosts)
            .expect_err("memory events require trusted plaintext hosts without a token too");
        assert!(error.to_string().contains("PROXIMA_PLAINTEXT_HOSTS"));
    }
}

fn event(delivered_count: u64) -> ReceivedEvent {
    let raw: Vec<u8> = serde_json::to_vec(&serde_json::json!({
        "specversion": "1.0",
        "id": "F:0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b",
        "source": "urn:test",
        "type": "probe/listenable-v1",
        "subject": "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b",
        "sequence": "00000000000000000001",
        "data": {"k": "v"},
    }))
    .expect("envelope");
    ReceivedEvent {
        id: "F:0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b".to_owned(),
        owner: proxima_core::OwnerId::from(proxima_core::UserId::new(uuid::Uuid::nil())),
        delivered_count,
        envelope: serde_json::from_slice(&raw).expect("envelope parses"),
        raw: raw.into(),
        position: proxima_core::publication::SubjectPosition::new(
            uuid::Uuid::nil(),
            std::num::NonZeroU64::MIN,
        ),
    }
}

#[test]
fn statuses_map_to_accept_reject_or_retry() {
    use reqwest::StatusCode;
    for code in [200, 201, 202, 204] {
        assert_eq!(
            classify(StatusCode::from_u16(code).expect("status"), ""),
            Ok(Intake::Accepted),
            "{code}"
        );
    }
    for code in [400, 422] {
        let Ok(Intake::Rejected { reason }) =
            classify(StatusCode::from_u16(code).expect("status"), "bad schema")
        else {
            panic!("{code} rejects");
        };
        assert_eq!(reason, format!("HTTP {code}: bad schema"));
    }
    let long = "x".repeat(10_000);
    let Ok(Intake::Rejected { reason }) = classify(StatusCode::UNPROCESSABLE_ENTITY, &long) else {
        panic!("422 rejects");
    };
    assert!(reason.len() < 600, "a rejection reason is truncated");
    let Ok(Intake::Rejected { reason }) = classify(StatusCode::BAD_REQUEST, "") else {
        panic!("400 rejects");
    };
    assert_eq!(reason, "HTTP 400: Bad Request");
    for code in [301, 302, 401, 403, 404, 408, 409, 429, 500, 502, 503, 504] {
        assert!(
            classify(StatusCode::from_u16(code).expect("status"), "").is_err(),
            "{code} is no decision: retry"
        );
    }
}

#[tokio::test]
async fn a_delivery_posts_the_raw_envelope_with_its_headers() {
    let ca = Ca::new();
    let receiver = Receiver::start(&ca, &[]).await;
    let files = Files::new(&ca);
    let token = files.rotate();
    let sink = intake(&receiver, &files);
    let event = event(3);

    assert_eq!(sink.accept(&event).await, Ok(Intake::Accepted));

    let seen = receiver.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(
        seen[0].body,
        event.raw.to_vec(),
        "body is the raw bytes, verbatim"
    );
    assert_eq!(seen[0].header("content-type"), Some(CONTENT_TYPE));
    assert_eq!(seen[0].header(DELIVERY_COUNT_HEADER), Some("3"));
    assert_eq!(
        seen[0].header("authorization"),
        Some(format!("Bearer {token}").as_str())
    );
}

#[tokio::test]
async fn the_token_file_is_read_on_every_delivery() {
    let ca = Ca::new();
    let receiver = Receiver::start(&ca, &[]).await;
    let files = Files::new(&ca);
    let sink = intake(&receiver, &files);

    let first = files.rotate();
    sink.accept(&event(1)).await.expect("first delivery");
    let second = files.rotate();
    sink.accept(&event(1)).await.expect("second delivery");

    let auth: Vec<_> = receiver
        .seen()
        .iter()
        .map(|seen| seen.header("authorization").map(str::to_owned))
        .collect();
    assert_eq!(
        auth,
        [
            Some(format!("Bearer {first}")),
            Some(format!("Bearer {second}"))
        ]
    );
}

#[tokio::test]
async fn a_missing_token_file_sends_nothing() {
    let ca = Ca::new();
    let receiver = Receiver::start(&ca, &[]).await;
    let files = Files::new(&ca);
    let sink = intake(&receiver, &files);

    sink.accept(&event(1))
        .await
        .expect_err("no token, no request");
    assert!(receiver.seen().is_empty());
}

#[tokio::test]
async fn a_receiver_certified_by_another_ca_never_sees_the_body() {
    let trusted = Ca::new();
    let other = Ca::new();
    let receiver = Receiver::start(&other, &[]).await;
    let files = Files::new(&trusted);
    files.rotate();
    let sink = intake(&receiver, &files);

    sink.accept(&event(1))
        .await
        .expect_err("the configured CA is the only trust root");
    assert!(receiver.seen().is_empty(), "no request body was ever read");
}

#[tokio::test]
async fn unauthorized_throttled_and_silent_receivers_are_retried() {
    let ca = Ca::new();
    let receiver =
        Receiver::start(&ca, &[Reply::Status(401), Reply::Status(429), Reply::Hang]).await;
    let files = Files::new(&ca);
    files.rotate();
    let sink = intake(&receiver, &files);

    for what in ["401", "429", "timeout"] {
        sink.accept(&event(1)).await.expect_err(what);
    }
    assert_eq!(receiver.seen().len(), 3);
}

/// A rejection whose body never ends still settles as rejected: the sink
/// reads at most `MAX_REASON_BYTES` of it, so neither the body's size nor
/// its length in time reaches this process.
#[tokio::test]
async fn an_endless_rejection_body_is_read_only_to_the_reason_bound() {
    let ca = Ca::new();
    let receiver = Receiver::start(&ca, &[Reply::EndlessRejection]).await;
    let files = Files::new(&ca);
    files.rotate();
    let sink = intake(&receiver, &files);

    let Intake::Rejected { reason } = sink.accept(&event(1)).await.expect("a decision") else {
        panic!("422 rejects");
    };
    let detail = reason.strip_prefix("HTTP 422: ").expect("status first");
    assert_eq!(detail.chars().count(), MAX_REASON_CHARS);
    assert!(detail.chars().all(|c| c == 'x'), "{detail}");
}

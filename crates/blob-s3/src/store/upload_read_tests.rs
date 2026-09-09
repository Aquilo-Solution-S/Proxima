//! Exercise canonical adoption through the real SDK's HTTP/error decoder.

use std::time::Duration;

use aws_sdk_s3::config::{BehaviorVersion, Credentials, Region};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::{BlobError, ByteStream, LengthExpectation, hash_uploaded_object, publish_canonical};
use crate::store::port::blob_error_to_storage;

const BODY: &[u8] = b"canonical upload bytes";
const KEY: &str = "objects/01900000-0000-7000-8000-000000000001";

struct PublicationFixture {
    client: aws_sdk_s3::Client,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for PublicationFixture {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

impl PublicationFixture {
    async fn finish(mut self) {
        let mut task = self.task.take().expect("fixture task");
        if let Ok(result) = tokio::time::timeout(Duration::from_secs(5), &mut task).await {
            result.expect("fixture request assertions pass");
        } else {
            task.abort();
            let _ = task.await;
            panic!("fixture must finish within deadline");
        }
    }
}

async fn read_request(socket: &mut TcpStream) -> String {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 2048];
    loop {
        let length = socket.read(&mut buffer).await.expect("read request");
        assert_ne!(length, 0, "complete HTTP request");
        bytes.extend_from_slice(&buffer[..length]);
        assert!(bytes.len() < 64 * 1024, "bounded fixture request");
        if let Some(offset) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            let header_end = offset + 4;
            let headers = String::from_utf8_lossy(&bytes[..header_end]);
            let body_length = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|length| length.trim().parse::<usize>().expect("content length"))
                })
                .unwrap_or(0);
            if bytes.len() >= header_end + body_length {
                return headers.into_owned();
            }
        }
    }
}

async fn publication_fixture(get_status: &str, get_body: &[u8]) -> PublicationFixture {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fixture");
    let endpoint = listener.local_addr().expect("fixture address");
    let client = aws_sdk_s3::Client::from_conf(
        aws_sdk_s3::Config::builder()
            .behavior_version(BehaviorVersion::latest())
            .region(Region::new("us-east-1"))
            .credentials_provider(Credentials::new(
                "fixture-key",
                "fixture-secret",
                None,
                None,
                "local-test",
            ))
            .endpoint_url(format!("http://{endpoint}"))
            .force_path_style(true)
            .retry_config(aws_sdk_s3::config::retry::RetryConfig::standard().with_max_attempts(1))
            .build(),
    );
    let get_status = get_status.to_owned();
    let get_body = get_body.to_vec();
    let task = tokio::spawn(async move {
        for method in ["PUT", "GET"] {
            let (mut socket, _) = listener.accept().await.expect("accept request");
            let request = read_request(&mut socket).await;
            let operation = if method == "PUT" {
                "PutObject"
            } else {
                "GetObject"
            };
            assert_eq!(
                request.lines().next(),
                Some(format!("{method} /fixture-bucket/{KEY}?x-id={operation} HTTP/1.1").as_str()),
                "only one conditional publication and one canonical read"
            );
            let (status, body) = if method == "PUT" {
                assert!(
                    request
                        .lines()
                        .any(|line| line.eq_ignore_ascii_case("if-none-match: *")),
                    "publication remains conditional"
                );
                (
                    "412 Precondition Failed",
                    b"<Error><Code>PreconditionFailed</Code></Error>".as_slice(),
                )
            } else {
                (get_status.as_str(), get_body.as_slice())
            };
            let headers = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nETag: \"fixture-etag\"\r\nConnection: close\r\n\r\n",
                body.len()
            );
            socket
                .write_all(headers.as_bytes())
                .await
                .expect("write headers");
            socket.write_all(body).await.expect("write body");
        }
    });
    PublicationFixture {
        client,
        task: Some(task),
    }
}

async fn publish_after_conflict(
    get_status: &str,
    get_body: &[u8],
) -> Result<(String, Option<String>), BlobError> {
    let fixture = publication_fixture(get_status, get_body).await;
    let candidate = hash_uploaded_object(
        ByteStream::from_static(BODY),
        LengthExpectation::CapOnly,
        Some(1024),
    )
    .await
    .expect("hash known fixture bytes");
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        publish_canonical(
            &fixture.client,
            "fixture-bucket",
            KEY,
            &candidate,
            BODY.to_vec(),
            Some(1024),
        ),
    )
    .await
    .expect("publication completes within deadline");
    fixture.finish().await;
    result
}

#[tokio::test]
async fn missing_bucket_after_canonical_conflict_remains_unavailable() {
    let error =
        publish_after_conflict("404 Not Found", b"<Error><Code>NoSuchBucket</Code></Error>")
            .await
            .expect_err("missing bucket cannot be adopted");
    let error = blob_error_to_storage(error);
    assert!(
        matches!(error, proxima_core::StorageError::Unavailable(_)),
        "missing bucket is a provider fault, not invalid upload input: {error:?}"
    );
}

#[tokio::test]
async fn named_provider_errors_on_404_remain_unavailable() {
    for code in [
        "AccessDenied",
        "InvalidObjectState",
        "NotFound",
        "UnexpectedProviderCode",
    ] {
        let body = format!("<Error><Code>{code}</Code></Error>");
        let error = publish_after_conflict("404 Not Found", body.as_bytes())
            .await
            .expect_err("named provider error cannot be adopted");
        let error = blob_error_to_storage(error);
        assert!(
            matches!(error, proxima_core::StorageError::Unavailable(_)),
            "explicit {code} must not be hidden by HTTP404: {error:?}"
        );
    }
}

#[tokio::test]
async fn missing_key_after_canonical_conflict_reports_disappeared_object() {
    let error = publish_after_conflict("404 Not Found", b"<Error><Code>NoSuchKey</Code></Error>")
        .await
        .expect_err("missing canonical key");
    assert!(matches!(error, BlobError::State(ref message)
        if message == "canonical object disappeared during conditional publication"));
}

#[tokio::test]
async fn bare_404_after_canonical_conflict_preserves_existing_missing_behavior() {
    for body in [b"".as_slice(), b"<Error/>", b"not an S3 error document"] {
        let error = publish_after_conflict("404 Not Found", body)
            .await
            .expect_err("code-less 404");
        assert!(
            matches!(error, BlobError::State(ref message)
            if message == "canonical object disappeared during conditional publication"),
            "existing code-less fallback for {body:?}: {error:?}"
        );
    }
}

#[tokio::test]
async fn matching_canonical_response_is_adopted_after_conflict() {
    let (key, etag) = publish_after_conflict("200 OK", BODY)
        .await
        .expect("adopt verified existing bytes");
    assert_eq!(key, KEY);
    assert_eq!(etag.as_deref(), Some("\"fixture-etag\""));
}

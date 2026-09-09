//! Exercise version cursors through the AWS SDK against a bounded local server.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use aws_sdk_s3::config::{BehaviorVersion, Credentials, Region};
use proxima_core::StorageError;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::purge_exact_key;

#[derive(Clone, Debug)]
struct Request {
    method: String,
    target: String,
    body: String,
}

struct Fixture {
    client: aws_sdk_s3::Client,
    requests: Arc<Mutex<Vec<Request>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn read_request(socket: &mut TcpStream) -> Request {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 2048];
    loop {
        let length = socket.read(&mut buffer).await.expect("read local request");
        assert_ne!(length, 0, "request ended early");
        bytes.extend_from_slice(&buffer[..length]);
        assert!(bytes.len() < 65_536, "fixture request exceeded its bound");
        let Some(offset) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
            continue;
        };
        let header_end = offset + 4;
        let headers = String::from_utf8_lossy(&bytes[..header_end]);
        let body_length = headers
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(|value| value.trim().parse::<usize>().expect("body length"))
            })
            .unwrap_or(0);
        if bytes.len() < header_end + body_length {
            continue;
        }
        let mut first_line = headers
            .lines()
            .next()
            .expect("request line")
            .split_whitespace();
        return Request {
            method: first_line.next().expect("method").to_owned(),
            target: first_line.next().expect("target").to_owned(),
            body: String::from_utf8(bytes[header_end..header_end + body_length].to_vec())
                .expect("XML body"),
        };
    }
}

fn page(truncated: bool, markers: &str, entries: &str) -> String {
    format!(
        "<ListVersionsResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
         <IsTruncated>{truncated}</IsTruncated>{markers}{entries}</ListVersionsResult>"
    )
}

fn cursor(version: &str) -> String {
    format!(
        "<NextKeyMarker>target</NextKeyMarker><NextVersionIdMarker>{version}</NextVersionIdMarker>"
    )
}

fn deleted_response(body: &str) -> String {
    let mut deleted = String::new();
    for object in body.split("<Object>").skip(1) {
        let identity = object.split_once("</Object>").expect("object end").0;
        deleted.push_str("<Deleted>");
        deleted.push_str(identity);
        deleted.push_str("</Deleted>");
    }
    format!(
        "<DeleteResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">{deleted}</DeleteResult>"
    )
}

async fn fixture(pages: Vec<String>) -> Fixture {
    assert!(!pages.is_empty());
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind local S3");
    let address = listener.local_addr().expect("local S3 address");
    let client = aws_sdk_s3::Client::from_conf(
        aws_sdk_s3::Config::builder()
            .behavior_version(BehaviorVersion::latest())
            .region(Region::new("us-east-1"))
            .credentials_provider(Credentials::new(
                "fixture-key",
                "fixture-secret",
                None,
                None,
                "local-pagination-test",
            ))
            .endpoint_url(format!("http://{address}"))
            .force_path_style(true)
            .retry_config(aws_sdk_s3::config::retry::RetryConfig::standard().with_max_attempts(1))
            .build(),
    );
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&requests);
    let task = tokio::spawn(async move {
        let mut lists = 0;
        loop {
            let (mut socket, _) = listener.accept().await.expect("accept local request");
            let request = read_request(&mut socket).await;
            let response = match request.method.as_str() {
                "GET" => {
                    // Repeat the malformed last page, then terminate even for
                    // the old implementation: red tests must never hang.
                    let response = if lists < pages.len() + 2 {
                        pages[lists.min(pages.len() - 1)].clone()
                    } else {
                        page(false, "", "")
                    };
                    lists += 1;
                    response
                }
                "POST" => deleted_response(&request.body),
                other => panic!("unexpected local method {other}"),
            };
            captured.lock().expect("capture lock").push(request);
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/xml\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{response}",
                        response.len()
                    )
                    .as_bytes(),
                )
                .await
                .expect("write local response");
        }
    });
    Fixture {
        client,
        requests,
        task,
    }
}

async fn run(fixture: &Fixture) -> Result<u64, StorageError> {
    tokio::time::timeout(
        Duration::from_secs(3),
        purge_exact_key(&fixture.client, "fixture-bucket", "target"),
    )
    .await
    .expect("purge must terminate")
}

async fn assert_bad_cursor(pages: Vec<String>, expected_lists: usize) {
    let fixture = fixture(pages).await;
    let result = run(&fixture).await;
    let requests = fixture.requests.lock().expect("capture lock");
    assert!(
        matches!(result, Err(StorageError::Unavailable(_))),
        "{result:?}; {requests:?}"
    );
    assert_eq!(
        requests.len(),
        expected_lists,
        "invalid page must not be deleted or fetched again"
    );
    assert!(requests.iter().all(|request| request.method == "GET"));
}

#[tokio::test]
async fn missing_key_marker_stops_before_deleting_page() {
    for markers in ["", "<NextVersionIdMarker>v1</NextVersionIdMarker>"] {
        assert_bad_cursor(
            vec![page(
                true,
                markers,
                "<Version><Key>target</Key><VersionId>v1</VersionId></Version>",
            )],
            1,
        )
        .await;
    }
}

#[tokio::test]
async fn empty_key_marker_stops_pagination() {
    for markers in [
        "<NextKeyMarker/>",
        "<NextKeyMarker/><NextVersionIdMarker/>",
        "<NextKeyMarker/><NextVersionIdMarker>v1</NextVersionIdMarker>",
    ] {
        assert_bad_cursor(vec![page(true, markers, "")], 1).await;
    }
}

#[tokio::test]
async fn repeated_marker_pair_stops_pagination() {
    assert_bad_cursor(vec![page(true, &cursor("v2"), "")], 2).await;
}

#[tokio::test]
async fn marker_pair_cycle_stops_pagination() {
    assert_bad_cursor(
        vec![
            page(true, &cursor("v2"), ""),
            page(true, &cursor("v1"), ""),
            page(true, &cursor("v2"), ""),
        ],
        3,
    )
    .await;
}

fn request_cursor(request: &Request) -> (Option<String>, Option<String>) {
    let url =
        reqwest::Url::parse(&format!("http://localhost{}", request.target)).expect("request URL");
    let field = |name| {
        url.query_pairs()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.into_owned())
    };
    (field("key-marker"), field("version-id-marker"))
}

#[tokio::test]
async fn advancing_versions_of_same_key_preserve_null_and_exclude_collisions() {
    let fixture = fixture(vec![
        page(
            true,
            &cursor("v2"),
            "<Version><Key>target</Key><VersionId>v3</VersionId></Version>\
             <Version><Key>target-other</Key><VersionId>v3</VersionId></Version>",
        ),
        page(
            true,
            &cursor("null"),
            "<DeleteMarker><Key>target</Key><VersionId>v2</VersionId></DeleteMarker>",
        ),
        page(
            false,
            "",
            "<Version><Key>target</Key><VersionId>null</VersionId></Version>\
             <DeleteMarker><Key>target-other</Key><VersionId>null</VersionId></DeleteMarker>",
        ),
    ])
    .await;
    assert_eq!(run(&fixture).await.expect("valid purge"), 3);
    let requests = fixture.requests.lock().expect("capture lock");
    let cursors: Vec<_> = requests
        .iter()
        .filter(|request| request.method == "GET")
        .map(request_cursor)
        .collect();
    assert_eq!(
        cursors,
        vec![
            (None, None),
            (Some("target".into()), Some("v2".into())),
            (Some("target".into()), Some("null".into())),
        ]
    );
    let deletes: Vec<_> = requests
        .iter()
        .filter(|request| request.method == "POST")
        .collect();
    assert_eq!(deletes.len(), 3);
    for (request, version) in deletes.iter().zip(["v3", "v2", "null"]) {
        assert!(request.body.contains("<Key>target</Key>"));
        assert!(
            request
                .body
                .contains(&format!("<VersionId>{version}</VersionId>"))
        );
        assert!(!request.body.contains("target-other"));
    }
}

#[tokio::test]
async fn key_only_markers_are_valid_with_missing_or_empty_version() {
    for version in ["", "<NextVersionIdMarker/>"] {
        let fixture = fixture(vec![
            page(
                true,
                &format!("<NextKeyMarker>target</NextKeyMarker>{version}"),
                "",
            ),
            page(false, "", ""),
        ])
        .await;
        assert_eq!(run(&fixture).await.expect("valid key-only cursor"), 0);
        let requests = fixture.requests.lock().expect("capture lock");
        assert_eq!(requests.len(), 2);
        assert_eq!(request_cursor(&requests[1]), (Some("target".into()), None));
    }
}

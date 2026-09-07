//! Malformed provider responses must not turn a version purge into a simple delete.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use aws_sdk_s3::config::{BehaviorVersion, Credentials, Region};
use proxima_core::StorageError;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::purge_exact_key;

struct VersionFixture {
    client: aws_sdk_s3::Client,
    requests: Arc<Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for VersionFixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn read_request(socket: &mut TcpStream) -> String {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 2048];
    loop {
        let length = socket
            .read(&mut buffer)
            .await
            .expect("read fixture request");
        assert_ne!(length, 0, "request must complete before connection closes");
        bytes.extend_from_slice(&buffer[..length]);
        if let Some(offset) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
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
            if bytes.len() >= header_end + body_length {
                return String::from_utf8(bytes).expect("fixture HTTP is UTF-8");
            }
        }
    }
}

async fn version_fixture(entries: &str) -> VersionFixture {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fixture");
    let address = listener.local_addr().expect("fixture address");
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
            .endpoint_url(format!("http://{address}"))
            .force_path_style(true)
            .build(),
    );
    let listing = format!(
        "<ListVersionsResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
         <IsTruncated>false</IsTruncated>{entries}</ListVersionsResult>"
    );
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&requests);
    let task = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.expect("accept fixture request");
            let request = read_request(&mut socket).await;
            let is_list = request.starts_with("GET ");
            captured.lock().expect("request lock").push(request);
            let response = if is_list {
                listing.as_str()
            } else {
                "<DeleteResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
                 <Deleted><Key>target</Key><VersionId>v1</VersionId></Deleted></DeleteResult>"
            };
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                        response.len()
                    )
                    .as_bytes(),
                )
                .await
                .expect("write fixture response");
        }
    });
    VersionFixture {
        client,
        requests,
        task,
    }
}

async fn assert_incomplete_entry_rejected(entry: &str) {
    // A preceding valid entry ensures the whole page is checked before any
    // delete is sent, even when its first identifier could be constructed.
    let entries = format!("<Version><Key>target</Key><VersionId>v1</VersionId></Version>{entry}");
    let fixture = version_fixture(&entries).await;
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        purge_exact_key(&fixture.client, "fixture-bucket", "target"),
    )
    .await
    .expect("purge must terminate");
    let requests = fixture.requests.lock().expect("request lock");
    assert!(
        matches!(result, Err(StorageError::Unavailable(_))),
        "{result:?}"
    );
    assert_eq!(
        requests.len(),
        1,
        "malformed listing must not send a delete"
    );
    assert!(requests[0].starts_with("GET "));
}

#[tokio::test]
async fn missing_version_identity_cannot_become_a_simple_delete() {
    for entry in [
        "<Version><Key>target</Key></Version>",
        "<Version><Key>target</Key><VersionId/></Version>",
        "<DeleteMarker><Key>target</Key></DeleteMarker>",
        "<DeleteMarker><Key>target</Key><VersionId/></DeleteMarker>",
    ] {
        assert_incomplete_entry_rejected(entry).await;
    }
}

#[tokio::test]
async fn unidentified_version_entries_cannot_be_silently_skipped() {
    for entry in [
        "<Version><VersionId>v2</VersionId></Version>",
        "<Version><Key/><VersionId>v2</VersionId></Version>",
        "<DeleteMarker><VersionId>v2</VersionId></DeleteMarker>",
        "<DeleteMarker><Key/><VersionId>v2</VersionId></DeleteMarker>",
    ] {
        assert_incomplete_entry_rejected(entry).await;
    }
}

#[tokio::test]
async fn exact_purge_retains_version_identity_and_ignores_other_keys() {
    for version in ["v1", "null"] {
        let entries = format!(
            "<Version><Key>target</Key><VersionId>{version}</VersionId></Version>\
             <Version><Key>target-suffix</Key></Version>"
        );
        let fixture = version_fixture(&entries).await;
        let deleted = tokio::time::timeout(
            Duration::from_secs(3),
            purge_exact_key(&fixture.client, "fixture-bucket", "target"),
        )
        .await
        .expect("purge must terminate")
        .expect("valid target version");
        assert_eq!(deleted, 1);
        let requests = fixture.requests.lock().expect("request lock");
        assert_eq!(requests.len(), 2);
        assert!(requests[1].starts_with("POST "));
        let body = requests[1].split_once("\r\n\r\n").expect("HTTP body").1;
        assert!(body.contains(&format!("<VersionId>{version}</VersionId>")));
        assert!(!body.contains("target-suffix"));
    }
}

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
    version_fixture_with_response(entries, None).await
}

fn delete_result(entries: &str) -> String {
    format!(
        "<DeleteResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">{entries}</DeleteResult>"
    )
}

fn echo_deleted(request: &str) -> String {
    let body = request.split_once("\r\n\r\n").expect("HTTP body").1;
    let mut deleted = String::new();
    for object in body.split("<Object>").skip(1) {
        let identity = object.split_once("</Object>").expect("object end").0;
        deleted.push_str("<Deleted>");
        deleted.push_str(identity);
        deleted.push_str("</Deleted>");
    }
    delete_result(&deleted)
}

async fn version_fixture_with_response(
    entries: &str,
    delete_response: Option<String>,
) -> VersionFixture {
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
            let response = if is_list {
                listing.clone()
            } else {
                delete_response
                    .clone()
                    .unwrap_or_else(|| echo_deleted(&request))
            };
            captured.lock().expect("request lock").push(request);
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

const TWO_VERSIONS: &str = "<Version><Key>target</Key><VersionId>v1</VersionId></Version>\
    <DeleteMarker><Key>target</Key><VersionId>v2</VersionId></DeleteMarker>";
const DELETED_V1: &str = "<Deleted><Key>target</Key><VersionId>v1</VersionId></Deleted>";

async fn run_purge(fixture: &VersionFixture) -> Result<u64, StorageError> {
    tokio::time::timeout(
        Duration::from_secs(3),
        purge_exact_key(&fixture.client, "fixture-bucket", "target"),
    )
    .await
    .expect("purge must terminate")
}

async fn assert_delete_response_rejected(acknowledgements: &str) {
    let fixture =
        version_fixture_with_response(TWO_VERSIONS, Some(delete_result(acknowledgements))).await;
    let result = run_purge(&fixture).await;
    assert!(
        matches!(result, Err(StorageError::Unavailable(_))),
        "{result:?}"
    );
    let requests = fixture.requests.lock().expect("request lock");
    assert_eq!(
        requests.len(),
        2,
        "one listing followed by one delete batch"
    );
    assert!(requests[1].starts_with("POST "));
}

#[tokio::test]
async fn delete_requires_acknowledgement_of_every_requested_version() {
    for response in ["", DELETED_V1] {
        assert_delete_response_rejected(response).await;
    }
}

#[tokio::test]
async fn duplicate_acknowledgements_cannot_replace_a_missing_version() {
    assert_delete_response_rejected(&DELETED_V1.repeat(2)).await;
}

#[tokio::test]
async fn duplicate_requested_versions_still_require_each_acknowledgement() {
    let entries = "<Version><Key>target</Key><VersionId>v1</VersionId></Version>".repeat(2);
    let fixture = version_fixture_with_response(&entries, Some(delete_result(DELETED_V1))).await;
    let result = run_purge(&fixture).await;
    assert!(
        matches!(result, Err(StorageError::Unavailable(_))),
        "{result:?}"
    );
}

#[tokio::test]
async fn complete_acknowledgements_of_duplicate_requests_are_accepted() {
    let entries = "<Version><Key>target</Key><VersionId>v1</VersionId></Version>".repeat(2);
    let fixture = version_fixture(&entries).await;
    assert_eq!(
        run_purge(&fixture)
            .await
            .expect("each requested identity acknowledged"),
        2
    );
}

#[tokio::test]
async fn acknowledgements_must_match_requested_key_and_version() {
    for unexpected in [
        "<Deleted><Key>target-suffix</Key><VersionId>v2</VersionId></Deleted>",
        "<Deleted><Key>target</Key><VersionId>v3</VersionId></Deleted>",
    ] {
        assert_delete_response_rejected(&format!("{DELETED_V1}{unexpected}")).await;
    }
}

#[tokio::test]
async fn incomplete_acknowledgement_identity_is_rejected() {
    for incomplete in [
        "<Deleted><VersionId>v2</VersionId></Deleted>",
        "<Deleted><Key/><VersionId>v2</VersionId></Deleted>",
        "<Deleted><Key>target</Key></Deleted>",
        "<Deleted><Key>target</Key><VersionId/></Deleted>",
        "<Deleted><Key>target</Key><DeleteMarker>true</DeleteMarker>\
         <DeleteMarkerVersionId>v2</DeleteMarkerVersionId></Deleted>",
    ] {
        assert_delete_response_rejected(&format!("{DELETED_V1}{incomplete}")).await;
    }
}

#[tokio::test]
async fn unexpected_extra_acknowledgements_are_rejected() {
    let complete = "<Deleted><Key>target</Key><VersionId>v2</VersionId></Deleted>";
    for extra in [
        DELETED_V1,
        "<Deleted><Key>other</Key><VersionId>v2</VersionId></Deleted>",
    ] {
        assert_delete_response_rejected(&format!("{DELETED_V1}{complete}{extra}")).await;
    }
}

#[tokio::test]
async fn reordered_complete_acknowledgements_preserve_null_and_delete_markers() {
    let entries =
        format!("{TWO_VERSIONS}<Version><Key>target</Key><VersionId>null</VersionId></Version>");
    let response = delete_result(&format!(
        "<Deleted><Key>target</Key><VersionId>v2</VersionId><DeleteMarker>true</DeleteMarker>\
         <DeleteMarkerVersionId>v2</DeleteMarkerVersionId></Deleted>\
         <Deleted><Key>target</Key><VersionId>null</VersionId></Deleted>{DELETED_V1}"
    ));
    let fixture = version_fixture_with_response(&entries, Some(response)).await;
    assert_eq!(
        run_purge(&fixture).await.expect("complete acknowledgement"),
        3
    );
}

#[tokio::test]
async fn per_object_delete_errors_remain_failures() {
    let response = delete_result(&format!(
        "{DELETED_V1}<Error><Key>target</Key><VersionId>v2</VersionId>\
         <Code>AccessDenied</Code><Message>Access denied for fixture version</Message></Error>"
    ));
    let fixture = version_fixture_with_response(TWO_VERSIONS, Some(response)).await;
    let error = run_purge(&fixture).await.expect_err("partial delete fails");
    assert!(matches!(error, StorageError::Unavailable(_)));
    assert!(
        error
            .to_string()
            .contains("Access denied for fixture version")
    );
}

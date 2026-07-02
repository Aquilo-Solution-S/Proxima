//! Shared test helpers for mcp-server integration tests.

use async_trait::async_trait;
use proxima_mcp_server::owner_key;
pub use proxima_pg_testkit::{db_url, drop_db};
use serde_json::json;

use proxima_core::test_fixtures::owner_fixture;
use proxima_core::{AccessError, Owner, OwnerAccessPort, OwnerRef, OwnerRoles, UserId};

/// Returns a nil owner for token tests.
#[allow(dead_code)]
pub fn nil_owner() -> Owner {
    owner_fixture()
}

#[allow(dead_code)]
pub fn nil_subject() -> UserId {
    let OwnerRef::Personal(subject) = nil_owner() else {
        unreachable!("owner_fixture is personal")
    };
    subject
}

#[allow(dead_code)]
pub fn nil_owner_header() -> String {
    owner_key(nil_owner())
}

#[derive(Debug)]
#[allow(dead_code)]
pub struct NilOwnerAccess;

#[async_trait]
impl OwnerAccessPort for NilOwnerAccess {
    async fn resolve_roles_for_subject(&self, subject: UserId) -> Result<OwnerRoles, AccessError> {
        OwnerRoles::for_subject(subject, [])
    }
}

/// MCP initialize request. Returns the `session_id`.
#[allow(dead_code)]
pub async fn initialize(
    client: &reqwest::Client,
    url: &str,
    bearer: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let response = client
        .post(url)
        .header("Origin", "http://localhost")
        .header("Authorization", bearer)
        .header("X-Proxima-Owner", nil_owner_header())
        .header("MCP-Protocol-Version", "2025-03-26")
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": {"name": "test", "version": "0"}
            }
        }))
        .send()
        .await?;
    assert!(response.status().is_success(), "{}", response.status());
    let session_id = response
        .headers()
        .get("Mcp-Session-Id")
        .ok_or("missing session id")?
        .to_str()?
        .to_string();
    let _ = sse_json(response).await?;
    Ok(session_id)
}

/// MCP initialized notification.
#[allow(dead_code)]
pub async fn initialized(
    client: &reqwest::Client,
    url: &str,
    session: &str,
    bearer: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let response = client
        .post(url)
        .header("Origin", "http://localhost")
        .header("Authorization", bearer)
        .header("X-Proxima-Owner", nil_owner_header())
        .header("MCP-Protocol-Version", "2025-03-26")
        .header("Mcp-Session-Id", session)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .json(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
        .send()
        .await?;
    assert!(response.status().is_success(), "{}", response.status());
    Ok(())
}

/// Post an MCP RPC request with a full JSON body. Returns the parsed JSON response.
#[allow(dead_code)]
pub async fn post_rpc(
    client: &reqwest::Client,
    url: &str,
    session_id: Option<&str>,
    bearer: &str,
    body: serde_json::Value,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let mut request = client
        .post(url)
        .header("Origin", "http://localhost")
        .header("Authorization", bearer)
        .header("X-Proxima-Owner", nil_owner_header())
        .header("MCP-Protocol-Version", "2025-03-26")
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .json(&body);
    if let Some(session_id) = session_id {
        request = request.header("Mcp-Session-Id", session_id);
    }
    let response = request.send().await?;
    assert!(response.status().is_success(), "{}", response.status());
    sse_json(response).await
}

/// Extract JSON from SSE response.
#[allow(dead_code)]
pub async fn sse_json(
    response: reqwest::Response,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let text = response.text().await?;
    for data in text.lines().filter_map(|line| line.strip_prefix("data:")) {
        let data = data.trim();
        if data.is_empty() {
            continue;
        }
        if let Ok(value) = serde_json::from_str(data) {
            return Ok(value);
        }
    }
    Err(format!("missing JSON SSE data in {text:?}").into())
}

/// Create a fresh test database.
#[allow(dead_code)]
pub async fn create_db() -> Result<String, Box<dyn std::error::Error>> {
    let db_name = proxima_pg_testkit::unique_db_name("proxima_test");
    proxima_pg_testkit::create_db(&db_name)
        .await
        .expect("PG required for tests");
    Ok(db_name)
}

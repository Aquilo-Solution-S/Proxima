//! Shared test helpers for mcp-server integration tests.

use proxima_mcp_server::owner_key;
pub use proxima_pg_testkit::{db_url, drop_db};
use serde_json::json;

use proxima_core::Owner;
use proxima_core::test_fixtures::owner_fixture;

/// Returns a nil owner for token tests.
#[allow(dead_code)]
pub fn nil_owner() -> Owner {
    owner_fixture()
}

#[allow(dead_code)]
pub fn nil_owner_header() -> String {
    owner_key(nil_owner())
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

/// The serve task returned by [`start_server`].
#[allow(dead_code)]
pub type ServeHandle = tokio::task::JoinHandle<Result<(), proxima_mcp_server::McpServerError>>;

/// Boot a streamable-HTTP server on a fresh database — the shared
/// spin-up every integration test used to hand-roll. Returns the serve
/// task, the bound loopback address, and the database name to pass to
/// [`stop_server`].
#[allow(dead_code)]
pub async fn start_server(
    auth_store: std::sync::Arc<proxima_mcp_server::McpEdgeAuth>,
) -> Result<(ServeHandle, std::net::SocketAddr, String), Box<dyn std::error::Error>> {
    let db_name = create_db().await?;
    let server = proxima_mcp_server::McpToolHost::from_database_url(
        &db_url(&db_name),
        proxima_core::FlavorRegistry::new(),
    )
    .await?;
    let (handle, addr) = proxima_mcp_server::serve_streamable_http(
        std::net::SocketAddr::new(std::net::Ipv4Addr::LOCALHOST.into(), 0),
        server,
        proxima_mcp_server::default_allowlist(),
        auth_store,
    )
    .await?;
    Ok((handle, addr, db_name))
}

/// Tear down a [`start_server`] boot: abort the serve task and drop its
/// database.
#[allow(dead_code)]
pub async fn stop_server(
    handle: ServeHandle,
    db_name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    handle.abort();
    let _ = handle.await;
    drop_db(db_name).await?;
    Ok(())
}

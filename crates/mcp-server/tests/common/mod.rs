//! Shared test helpers for mcp-server integration tests.

use serde_json::json;
use sqlx::{Connection, Executor, PgConnection};

use proxima_core::wake::token_store::WakeTokenContext;
use proxima_core::{HandleTable, OrgId, Owner, Principal, UserId};

const ADMIN_URL: &str = "postgres://proxima:proxima@localhost/proxima";

/// Returns a nil owner for token tests.
#[allow(dead_code)]
pub fn nil_owner() -> Owner {
    Owner {
        principal: Principal::User(UserId::new(uuid::Uuid::nil())),
        org_id: OrgId::new(uuid::Uuid::nil()),
    }
}

/// Creates a `WakeTokenContext` with the given palette.
#[allow(dead_code)]
pub fn make_token_ctx(palette: Vec<String>) -> WakeTokenContext {
    WakeTokenContext {
        invocation_id: uuid::Uuid::new_v4(),
        personality_instance_id: uuid::Uuid::new_v4(),
        wake_entry_id: uuid::Uuid::new_v4(),
        change_event_seq: uuid::Uuid::new_v4(),
        owner: nil_owner(),
        palette,
        model_id: "anthropic/claude-3-5-sonnet".into(),
        max_rounds: 4,
        current_root_perspective_memory_id: proxima_core::MemoryId::new(uuid::Uuid::now_v7()),
        triggering_event_memory_id: proxima_core::MemoryId::new(uuid::Uuid::now_v7()),
        triggering_event_depth: proxima_core::WakeChainDepth::new(0),
        read_log: std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new())),
        handles: std::sync::Arc::new(HandleTable::new()),
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

/// Create a fresh test database. Returns None if Postgres is unreachable.
#[allow(dead_code)]
pub async fn create_db() -> Result<Option<String>, Box<dyn std::error::Error>> {
    let db_name = format!("proxima_test_{}", uuid::Uuid::now_v7().simple());
    let Ok(mut conn) = PgConnection::connect(ADMIN_URL).await else {
        panic!("PG required for tests but admin connect failed");
    };
    conn.execute(format!("CREATE DATABASE \"{db_name}\"").as_str())
        .await?;
    conn.close().await?;
    Ok(Some(db_name))
}

/// Drop a test database.
#[allow(dead_code)]
pub async fn drop_db(name: &str) -> Result<(), sqlx::Error> {
    let mut conn = PgConnection::connect(ADMIN_URL).await?;
    conn.execute(format!("DROP DATABASE IF EXISTS \"{name}\" WITH (FORCE)").as_str())
        .await?;
    conn.close().await?;
    Ok(())
}

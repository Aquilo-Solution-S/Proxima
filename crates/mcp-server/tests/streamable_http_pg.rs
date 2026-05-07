use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use proxima_core::wake::token_store::{WakeTokenContext, WakeTokenStore};
use proxima_core::{FlavorRegistry, OrgId, Owner, Principal, UserId};
use proxima_mcp_server::{DevMcpServer, default_allowlist, serve_streamable_http};
use serde_json::json;
use sqlx::{Connection, Executor, PgConnection};

const ADMIN_URL: &str = "postgres://postgres@localhost/postgres";

#[tokio::test]
async fn streamable_http_initialize_list_and_remember() -> Result<(), Box<dyn std::error::Error>> {
    let Some(db_name) = create_db().await? else {
        return Ok(());
    };
    let database_url = format!("postgres://postgres@localhost/{db_name}");
    let mut registry = FlavorRegistry::new();
    proxima_mcp_substrate::register(&mut registry);
    let server = DevMcpServer::from_database_url(&database_url, nil_owner(), registry).await?;
    let store = Arc::new(WakeTokenStore::new(Duration::from_mins(1)));
    let token = store
        .mint(make_token_ctx(vec!["proxima-mcp/proxima_remember".into()]))
        .await;
    let (handle, addr) = serve_streamable_http(
        SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0),
        server,
        default_allowlist(),
        store,
    )
    .await?;

    let client = reqwest::Client::new();
    let url = format!("http://{addr}/mcp");
    let bearer = format!("Bearer {token}");
    let session_id = initialize(&client, &url, &bearer).await?;
    initialized(&client, &url, &session_id, &bearer).await?;

    let list = post_rpc(
        &client,
        &url,
        Some(&session_id),
        &bearer,
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
    )
    .await?;
    let names: Vec<_> = list["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert!(
        names.contains(&"proxima-mcp/proxima_remember"),
        "got {names:?}"
    );

    let remembered = post_rpc(
        &client,
        &url,
        Some(&session_id),
        &bearer,
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "proxima-mcp/proxima_remember",
                "arguments": {
                    "title": "mcp streamable test",
                    "body": "HTTP transport remembers notes.",
                    "idempotency_key": "streamable-http-test"
                }
            }
        }),
    )
    .await?;
    let content = remembered["result"]["content"][0]["text"]
        .as_str()
        .expect("text content");
    let output: serde_json::Value = serde_json::from_str(content)?;
    assert!(output["handle"].as_str().expect("handle").starts_with('N'));

    handle.abort();
    let _ = handle.await;
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn missing_origin_returns_403() -> Result<(), Box<dyn std::error::Error>> {
    let Some(db_name) = create_db().await? else {
        return Ok(());
    };
    let database_url = format!("postgres://postgres@localhost/{db_name}");
    let mut registry = FlavorRegistry::new();
    proxima_mcp_substrate::register(&mut registry);
    let server = DevMcpServer::from_database_url(&database_url, nil_owner(), registry).await?;
    let store = Arc::new(WakeTokenStore::new(Duration::from_mins(1)));
    let (handle, addr) = serve_streamable_http(
        SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0),
        server,
        default_allowlist(),
        store,
    )
    .await?;

    let response = reqwest::Client::new()
        .post(format!("http://{addr}/mcp"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}))
        .send()
        .await?;
    assert_eq!(response.status().as_u16(), 403);

    handle.abort();
    let _ = handle.await;
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn non_loopback_bind_refused_immediately() -> Result<(), Box<dyn std::error::Error>> {
    let Some(db_name) = create_db().await? else {
        return Ok(());
    };
    let database_url = format!("postgres://postgres@localhost/{db_name}");
    let mut registry = FlavorRegistry::new();
    proxima_mcp_substrate::register(&mut registry);
    let server = DevMcpServer::from_database_url(&database_url, nil_owner(), registry).await?;
    let bind: SocketAddr = "0.0.0.0:0".parse()?;
    let store = Arc::new(WakeTokenStore::new(Duration::from_mins(1)));
    let err = serve_streamable_http(bind, server, default_allowlist(), store)
        .await
        .expect_err("must refuse non-loopback");
    assert!(matches!(
        err,
        proxima_mcp_server::McpServerError::NonLoopbackBind(_)
    ));
    drop_db(&db_name).await?;
    Ok(())
}

fn nil_owner() -> Owner {
    Owner {
        principal: Principal::User(UserId::new(uuid::Uuid::nil())),
        org_id: OrgId::new(uuid::Uuid::nil()),
    }
}

fn make_token_ctx(palette: Vec<String>) -> WakeTokenContext {
    WakeTokenContext {
        invocation_id: uuid::Uuid::new_v4(),
        personality_instance_id: uuid::Uuid::new_v4(),
        wake_entry_id: uuid::Uuid::new_v4(),
        owner: nil_owner(),
        palette,
        model_id: "anthropic/claude-3-5-sonnet".into(),
        max_rounds: 4,
    }
}

async fn initialize(
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

async fn initialized(
    client: &reqwest::Client,
    url: &str,
    session_id: &str,
    bearer: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let response = client
        .post(url)
        .header("Origin", "http://localhost")
        .header("Authorization", bearer)
        .header("MCP-Protocol-Version", "2025-03-26")
        .header("Mcp-Session-Id", session_id)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .json(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
        .send()
        .await?;
    assert!(response.status().is_success(), "{}", response.status());
    Ok(())
}

async fn post_rpc(
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

async fn sse_json(
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

async fn create_db() -> Result<Option<String>, Box<dyn std::error::Error>> {
    let db_name = format!("proxima_test_{}", uuid::Uuid::now_v7().simple());
    let Ok(mut conn) = PgConnection::connect(ADMIN_URL).await else {
        eprintln!("skipping (no admin PG)");
        return Ok(None);
    };
    conn.execute(format!("CREATE DATABASE \"{db_name}\"").as_str())
        .await?;
    conn.close().await?;
    Ok(Some(db_name))
}

async fn drop_db(name: &str) -> Result<(), sqlx::Error> {
    let mut conn = PgConnection::connect(ADMIN_URL).await?;
    conn.execute(format!("DROP DATABASE IF EXISTS \"{name}\" WITH (FORCE)").as_str())
        .await?;
    conn.close().await?;
    Ok(())
}

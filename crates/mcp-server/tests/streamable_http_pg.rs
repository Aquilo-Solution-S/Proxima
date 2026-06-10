use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

mod common;

use common::{create_db, drop_db, initialize, initialized, post_rpc};
use proxima_core::FlavorRegistry;
use proxima_core::wake::token_store::WakeTokenStore;
use proxima_mcp_server::{McpAuthStore, McpToolHost, default_allowlist, serve_streamable_http};
use serde_json::json;

#[tokio::test]
async fn streamable_http_initialize_list_and_remember() -> Result<(), Box<dyn std::error::Error>> {
    let Some(db_name) = create_db().await? else {
        return Ok(());
    };
    let database_url = format!("postgres://proxima:proxima@localhost/{db_name}");
    let mut registry = FlavorRegistry::new();
    proxima_mcp_substrate::register(&mut registry);
    let server = McpToolHost::from_database_url(&database_url, nil_owner(), registry).await?;
    proxima_mcp_substrate::migrator().run(server.pool()).await?;
    let store = Arc::new(WakeTokenStore::new(Duration::from_mins(1)));
    let token = store
        .mint(make_token_ctx(vec![
            "proxima-mcp/proxima_remember".into(),
            "core/fetch_memory".into(),
        ]))
        .await;
    let auth_store = Arc::new(McpAuthStore::new(store));
    let (handle, addr) = serve_streamable_http(
        SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0),
        server,
        default_allowlist(),
        auth_store,
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
        names.contains(&"proxima-mcp_proxima_remember"),
        "got {names:?}"
    );
    assert!(names.contains(&"core_fetch_memory"), "got {names:?}");

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
                "name": "proxima-mcp_proxima_remember",
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
    assert!(
        output["handle"].as_str().expect("handle").starts_with('F'),
        "remember mints a Fact handle, got: {output}"
    );

    handle.abort();
    let _ = handle.await;
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn missing_auth_returns_401() -> Result<(), Box<dyn std::error::Error>> {
    let Some(db_name) = create_db().await? else {
        return Ok(());
    };
    let database_url = format!("postgres://proxima:proxima@localhost/{db_name}");
    let mut registry = FlavorRegistry::new();
    proxima_mcp_substrate::register(&mut registry);
    let server = McpToolHost::from_database_url(&database_url, nil_owner(), registry).await?;
    proxima_mcp_substrate::migrator().run(server.pool()).await?;
    let store = Arc::new(WakeTokenStore::new(Duration::from_mins(1)));
    let auth_store = Arc::new(McpAuthStore::new(store));
    let (handle, addr) = serve_streamable_http(
        SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0),
        server,
        default_allowlist(),
        auth_store,
    )
    .await?;

    let response = reqwest::Client::new()
        .post(format!("http://{addr}/mcp"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}))
        .send()
        .await?;
    assert_eq!(response.status().as_u16(), 401);

    handle.abort();
    let _ = handle.await;
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn disallowed_origin_returns_403_with_valid_token() -> Result<(), Box<dyn std::error::Error>>
{
    let Some(db_name) = create_db().await? else {
        return Ok(());
    };
    let database_url = format!("postgres://proxima:proxima@localhost/{db_name}");
    let mut registry = FlavorRegistry::new();
    proxima_mcp_substrate::register(&mut registry);
    let server = McpToolHost::from_database_url(&database_url, nil_owner(), registry).await?;
    proxima_mcp_substrate::migrator().run(server.pool()).await?;
    let store = Arc::new(WakeTokenStore::new(Duration::from_mins(1)));
    let auth_store = Arc::new(McpAuthStore::new(store));
    let token = uuid::Uuid::new_v4();
    auth_store
        .replace_local_master_token(token, nil_owner())
        .await;
    let (handle, addr) = serve_streamable_http(
        SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0),
        server,
        default_allowlist(),
        auth_store,
    )
    .await?;

    let response = reqwest::Client::new()
        .post(format!("http://{addr}/mcp"))
        .header("Origin", "https://example.invalid")
        .header("Authorization", format!("Bearer {token}"))
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
async fn local_master_token_lists_all_tools_without_origin()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(db_name) = create_db().await? else {
        return Ok(());
    };
    let database_url = format!("postgres://proxima:proxima@localhost/{db_name}");
    let mut registry = FlavorRegistry::new();
    proxima_mcp_substrate::register(&mut registry);
    let server = McpToolHost::from_database_url(&database_url, nil_owner(), registry).await?;
    proxima_mcp_substrate::migrator().run(server.pool()).await?;
    let store = Arc::new(WakeTokenStore::new(Duration::from_mins(1)));
    let auth_store = Arc::new(McpAuthStore::new(store));
    let token = uuid::Uuid::new_v4();
    auth_store
        .replace_local_master_token(token, nil_owner())
        .await;
    let (handle, addr) = serve_streamable_http(
        SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0),
        server,
        default_allowlist(),
        auth_store,
    )
    .await?;

    let client = reqwest::Client::new();
    let url = format!("http://{addr}/mcp");
    let bearer = format!("Bearer {token}");
    let session_id = initialize_without_origin(&client, &url, &bearer).await?;
    initialized_without_origin(&client, &url, &session_id, &bearer).await?;
    let list = post_rpc_without_origin(
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
    assert!(names.contains(&"proxima-mcp_proxima_remember"));
    assert!(!names.contains(&"core_fetch_memory"));

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
    let database_url = format!("postgres://proxima:proxima@localhost/{db_name}");
    let mut registry = FlavorRegistry::new();
    proxima_mcp_substrate::register(&mut registry);
    let server = McpToolHost::from_database_url(&database_url, nil_owner(), registry).await?;
    proxima_mcp_substrate::migrator().run(server.pool()).await?;
    let bind: SocketAddr = "0.0.0.0:0".parse()?;
    let store = Arc::new(WakeTokenStore::new(Duration::from_mins(1)));
    let auth_store = Arc::new(McpAuthStore::new(store));
    let err = serve_streamable_http(bind, server, default_allowlist(), auth_store)
        .await
        .expect_err("must refuse non-loopback");
    assert!(matches!(
        err,
        proxima_mcp_server::McpServerError::NonLoopbackBind(_)
    ));
    drop_db(&db_name).await?;
    Ok(())
}

use common::{make_token_ctx, nil_owner};

async fn initialize_without_origin(
    client: &reqwest::Client,
    url: &str,
    bearer: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let response = client
        .post(url)
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
    let _ = common::sse_json(response).await?;
    Ok(session_id)
}

async fn initialized_without_origin(
    client: &reqwest::Client,
    url: &str,
    session_id: &str,
    bearer: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let response = client
        .post(url)
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

async fn post_rpc_without_origin(
    client: &reqwest::Client,
    url: &str,
    session_id: Option<&str>,
    bearer: &str,
    body: serde_json::Value,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let mut request = client
        .post(url)
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
    common::sse_json(response).await
}

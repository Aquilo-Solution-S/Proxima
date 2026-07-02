//! `initialize` instructions + `proxima://how-to` resource, over the real
//! Streamable HTTP transport. Profile-awareness is exercised by layering a
//! deployment `ToolScope` (the same mechanism `PROXIMA_TOOL_PROFILE` drives)
//! and asserting the instructions track the resolved surface.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

mod common;

use common::{create_db, db_url, drop_db, initialized, post_rpc};
use proxima_core::{FlavorRegistry, ToolScope};
use proxima_mcp_server::{McpEdgeAuth, McpToolHost, default_allowlist, serve_streamable_http};
use serde_json::json;

/// `initialize` and capture the full JSON-RPC response (the shared helper
/// discards the body, but we need `result.instructions`).
async fn initialize_capture(
    client: &reqwest::Client,
    url: &str,
    bearer: &str,
) -> Result<(String, serde_json::Value), Box<dyn std::error::Error>> {
    let response = client
        .post(url)
        .header("Origin", "http://localhost")
        .header("Authorization", bearer)
        .header("X-Proxima-Owner", common::nil_owner_header())
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
    let body = common::sse_json(response).await?;
    Ok((session_id, body))
}

async fn start(
    auth_store: Arc<McpEdgeAuth>,
) -> Result<
    (
        tokio::task::JoinHandle<Result<(), proxima_mcp_server::McpServerError>>,
        SocketAddr,
        String,
    ),
    Box<dyn std::error::Error>,
> {
    let db_name = create_db().await?;
    let database_url = db_url(&db_name);
    let registry = FlavorRegistry::new();
    let server = McpToolHost::from_database_url(&database_url, registry).await?;
    let (handle, addr) = serve_streamable_http(
        SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0),
        server,
        default_allowlist(),
        auth_store,
    )
    .await?;
    Ok((handle, addr, db_name))
}

#[tokio::test]
async fn initialize_returns_instructions_and_how_to_resource()
-> Result<(), Box<dyn std::error::Error>> {
    let auth_store =
        Arc::new(McpEdgeAuth::headless().with_owner_access(Arc::new(common::NilOwnerAccess)));
    let token = uuid::Uuid::new_v4();
    auth_store
        .replace_local_master_token(token, common::nil_subject())
        .await;
    let (handle, addr, db_name) = start(auth_store).await?;

    let client = reqwest::Client::new();
    let url = format!("http://{addr}/mcp");
    let bearer = format!("Bearer pxm_{token}");

    // initialize → non-empty, contract-bearing instructions.
    let (session_id, init) = initialize_capture(&client, &url, &bearer).await?;
    let instructions = init["result"]["instructions"]
        .as_str()
        .expect("instructions present at initialize");
    assert!(!instructions.is_empty());
    assert!(
        instructions.contains("agent-authored `core_link` edges cannot use Facts as sources"),
        "got: {instructions}"
    );
    assert!(instructions.contains("`core_derive`"));
    assert!(instructions.contains("proxima://how-to"));
    // Full surface advertises goals.
    assert!(instructions.contains("core_goal"));
    // Server advertises the resources capability.
    assert!(init["result"]["capabilities"]["resources"].is_object());

    initialized(&client, &url, &session_id, &bearer).await?;

    // resources/list surfaces the how-to resource.
    let list = post_rpc(
        &client,
        &url,
        Some(&session_id),
        &bearer,
        json!({"jsonrpc": "2.0", "id": 2, "method": "resources/list", "params": {}}),
    )
    .await?;
    let uris: Vec<_> = list["result"]["resources"]
        .as_array()
        .expect("resources")
        .iter()
        .filter_map(|r| r["uri"].as_str())
        .collect();
    assert!(uris.contains(&"proxima://how-to"), "got {uris:?}");

    // resources/read returns the playbook markdown.
    let read = post_rpc(
        &client,
        &url,
        Some(&session_id),
        &bearer,
        json!({
            "jsonrpc": "2.0", "id": 3, "method": "resources/read",
            "params": {"uri": "proxima://how-to"}
        }),
    )
    .await?;
    let body = read["result"]["contents"][0]["text"]
        .as_str()
        .expect("resource text");
    assert!(body.contains("The one hard law for agent-authored links"));
    assert!(body.contains("derived-from"));
    assert!(body.contains("## Worked example"));

    handle.abort();
    let _ = handle.await;
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn memory_profile_instructions_omit_excluded_tools() -> Result<(), Box<dyn std::error::Error>>
{
    // A deployment scope that keeps authoring + retrieval but drops goal tools
    // (standing in for any code execution tool a `full` deployment would
    // carry). The deployment scope is intersected into every caller's
    // scope, so even the master token sees only this palette.
    let palette = ToolScope::Palette(
        [
            "core_remember",
            "core_derive",
            "core_link",
            "core_search_memories",
            "resource:memory",
        ]
        .into_iter()
        .map(String::from)
        .collect(),
    );
    let auth_store = Arc::new(
        McpEdgeAuth::headless()
            .with_owner_access(Arc::new(common::NilOwnerAccess))
            .with_tool_scope(palette),
    );
    let token = uuid::Uuid::new_v4();
    auth_store
        .replace_local_master_token(token, common::nil_subject())
        .await;
    let (handle, addr, db_name) = start(auth_store).await?;

    let client = reqwest::Client::new();
    let url = format!("http://{addr}/mcp");
    let bearer = format!("Bearer pxm_{token}");

    let (_session_id, init) = initialize_capture(&client, &url, &bearer).await?;
    let instructions = init["result"]["instructions"]
        .as_str()
        .expect("instructions present");
    // Core contract still taught.
    assert!(
        instructions.contains("agent-authored `core_link` edges cannot use Facts as sources"),
        "got: {instructions}"
    );
    assert!(instructions.contains("`core_remember`"));
    assert!(instructions.contains("proxima://memory/{id}"));
    // Excluded tools get no guidance.
    assert!(
        !instructions.contains("core_goal"),
        "memory profile leaked goal guidance: {instructions}"
    );
    assert!(!instructions.contains("goal"));
    assert!(!instructions.contains("proxima-code_"));

    handle.abort();
    let _ = handle.await;
    drop_db(&db_name).await?;
    Ok(())
}

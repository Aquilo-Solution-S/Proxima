use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

mod common;

use async_trait::async_trait;
use common::{create_db, db_url, drop_db, initialize, initialized, post_rpc};
use proxima_core::{
    AuthError, AuthPath, Authenticator, AuthzContext, Credentials, FlavorRegistry, Owner, OwnerRef,
    RevalidationConfig,
};
use proxima_mcp_server::{
    McpEdgeAuth, McpToolHost, default_allowlist, serve_streamable_http,
    serve_streamable_http_with_revalidation,
};
use serde_json::json;
use tokio::task::JoinHandle;

#[tokio::test]
async fn streamable_http_initialize_list_and_remember() -> Result<(), Box<dyn std::error::Error>> {
    let db_name = create_db().await?;
    let database_url = db_url(&db_name);
    let registry = FlavorRegistry::new();
    let server = McpToolHost::from_database_url(&database_url, registry).await?;
    let owner = common::nil_owner();
    let auth_store =
        Arc::new(McpEdgeAuth::headless().with_host(Arc::new(TestHostAuth::new(owner, None))));
    let (handle, addr) = serve_streamable_http(
        SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0),
        server,
        default_allowlist(),
        auth_store,
    )
    .await?;

    let client = reqwest::Client::new();
    let url = format!("http://{addr}/mcp");
    let bearer = "Bearer host-token";
    let session_id = initialize(&client, &url, bearer).await?;
    initialized(&client, &url, &session_id, bearer).await?;

    let list = post_rpc(
        &client,
        &url,
        Some(&session_id),
        bearer,
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
    )
    .await?;
    let names: Vec<_> = list["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert!(names.contains(&"core_remember"), "got {names:?}");

    let remembered = post_rpc(
        &client,
        &url,
        Some(&session_id),
        bearer,
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "core_remember",
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
    assert_prefixed_uuid(output["handle"].as_str().expect("handle"), "F");

    handle.abort();
    let _ = handle.await;
    drop_db(&db_name).await?;
    Ok(())
}

fn assert_prefixed_uuid(raw: &str, expected_prefix: &str) {
    let (prefix, uuid_part) = raw.split_once(':').expect("prefixed uuid");
    assert_eq!(prefix, expected_prefix);
    uuid::Uuid::parse_str(uuid_part).expect("uuid body");
}

#[tokio::test]
async fn missing_auth_returns_401() -> Result<(), Box<dyn std::error::Error>> {
    let db_name = create_db().await?;
    let database_url = db_url(&db_name);
    let registry = FlavorRegistry::new();
    let server = McpToolHost::from_database_url(&database_url, registry).await?;
    let auth_store = Arc::new(McpEdgeAuth::headless());
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
    let db_name = create_db().await?;
    let database_url = db_url(&db_name);
    let registry = FlavorRegistry::new();
    let server = McpToolHost::from_database_url(&database_url, registry).await?;
    let owner = common::nil_owner();
    let auth_store =
        Arc::new(McpEdgeAuth::headless().with_host(Arc::new(TestHostAuth::new(owner, None))));
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
        .header("Authorization", "Bearer host-token")
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
async fn host_bearer_lists_all_tools_without_origin() -> Result<(), Box<dyn std::error::Error>> {
    let db_name = create_db().await?;
    let database_url = db_url(&db_name);
    let registry = FlavorRegistry::new();
    let server = McpToolHost::from_database_url(&database_url, registry).await?;
    let owner = common::nil_owner();
    let auth_store =
        Arc::new(McpEdgeAuth::headless().with_host(Arc::new(TestHostAuth::new(owner, None))));
    let (handle, addr) = serve_streamable_http(
        SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0),
        server,
        default_allowlist(),
        auth_store,
    )
    .await?;

    let client = reqwest::Client::new();
    let url = format!("http://{addr}/mcp");
    let bearer = "Bearer host-token";
    let session_id = initialize_without_origin(&client, &url, bearer).await?;
    initialized_without_origin(&client, &url, &session_id, bearer).await?;
    let list = post_rpc_without_origin(
        &client,
        &url,
        Some(&session_id),
        bearer,
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
    )
    .await?;
    let names: Vec<_> = list["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert!(names.contains(&"core_remember"));
    assert!(!names.contains(&"core_get_memory"));
    assert!(!names.contains(&"core_walk_memory_lineage"));
    assert!(names.contains(&"core_search_memories"));
    assert!(names.contains(&"core_fact"));

    let resources = post_rpc_without_origin(
        &client,
        &url,
        Some(&session_id),
        bearer,
        json!({"jsonrpc": "2.0", "id": 3, "method": "resources/list", "params": {}}),
    )
    .await?;
    let resource_uris: Vec<_> = resources["result"]["resources"]
        .as_array()
        .expect("resources")
        .iter()
        .filter_map(|resource| resource["uri"].as_str())
        .collect();
    assert!(resource_uris.contains(&"proxima://edge-types"));

    let templates = post_rpc_without_origin(
        &client,
        &url,
        Some(&session_id),
        bearer,
        json!({"jsonrpc": "2.0", "id": 4, "method": "resources/templates/list", "params": {}}),
    )
    .await?;
    let template_items = templates["result"]
        .get("resourceTemplates")
        .or_else(|| templates["result"].get("resource_templates"))
        .and_then(serde_json::Value::as_array)
        .expect("resource templates");
    let template_uris: Vec<_> = template_items
        .iter()
        .filter_map(|template| {
            template
                .get("uriTemplate")
                .or_else(|| template.get("uri_template"))
                .and_then(serde_json::Value::as_str)
        })
        .collect();
    assert!(template_uris.contains(&"proxima://memory/{id}{?expand_neighbors}"));
    assert!(template_uris.contains(&"proxima://memory/{id}/lineage{?direction,depth,limit}"));

    handle.abort();
    let _ = handle.await;
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn non_loopback_bind_refused_immediately() -> Result<(), Box<dyn std::error::Error>> {
    let db_name = create_db().await?;
    let database_url = db_url(&db_name);
    let registry = FlavorRegistry::new();
    let server = McpToolHost::from_database_url(&database_url, registry).await?;
    let bind: SocketAddr = "0.0.0.0:0".parse()?;
    let auth_store = Arc::new(McpEdgeAuth::headless());
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

#[tokio::test]
async fn host_bearer_sse_get_closes_at_identity_expiry() -> Result<(), Box<dyn std::error::Error>> {
    let auth = Arc::new(TestHostAuth::new(nil_owner(), Some(Duration::from_secs(2))));
    let (handle, addr, db_name) =
        start_host_auth_server(auth, RevalidationConfig::default()).await?;
    let client = reqwest::Client::new();
    let url = format!("http://{addr}/mcp");
    let bearer = "Bearer host-token";
    let session_id = initialize(&client, &url, bearer).await?;
    initialized(&client, &url, &session_id, bearer).await?;

    let response = open_standalone_sse(&client, &url, &session_id, bearer).await?;
    tokio::time::timeout(Duration::from_secs(10), response.bytes())
        .await
        .expect("SSE stream closes after identity expiry")?;

    handle.abort();
    let _ = handle.await;
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn host_bearer_sse_get_closes_after_epoch_bump() -> Result<(), Box<dyn std::error::Error>> {
    let auth = Arc::new(TestHostAuth::new(nil_owner(), None));
    let config = RevalidationConfig {
        max_stream_lifetime: Duration::from_secs(30),
        epoch_check_interval: Duration::from_millis(200),
    };
    let (handle, addr, db_name) = start_host_auth_server(auth.clone(), config).await?;
    let client = reqwest::Client::new();
    let url = format!("http://{addr}/mcp");
    let bearer = "Bearer host-token";
    let session_id = initialize(&client, &url, bearer).await?;
    initialized(&client, &url, &session_id, bearer).await?;

    let response = open_standalone_sse(&client, &url, &session_id, bearer).await?;
    auth.bump_epoch();
    tokio::time::timeout(Duration::from_secs(10), response.bytes())
        .await
        .expect("SSE stream closes after auth epoch bump")?;

    handle.abort();
    let _ = handle.await;
    drop_db(&db_name).await?;
    Ok(())
}

use common::nil_owner;

#[derive(Debug)]
struct TestHostAuth {
    owner: Owner,
    ttl: Option<Duration>,
    epoch: AtomicU64,
}

impl TestHostAuth {
    fn new(owner: Owner, ttl: Option<Duration>) -> Self {
        Self {
            owner,
            ttl,
            epoch: AtomicU64::new(0),
        }
    }

    fn bump_epoch(&self) {
        self.epoch.fetch_add(1, Ordering::SeqCst);
    }

    fn authz(&self) -> Result<AuthzContext, AuthError> {
        let OwnerRef::Personal(subject) = self.owner else {
            return Err(AuthError::InvalidCredentials);
        };
        Ok(AuthzContext::for_subject(subject, AuthPath::HostBearer)
            .with_expires_at(self.ttl.map(|ttl| std::time::SystemTime::now() + ttl))
            .with_auth_epoch(self.epoch.load(Ordering::SeqCst)))
    }
}

#[async_trait]
impl Authenticator for TestHostAuth {
    async fn authenticate(&self, creds: &Credentials) -> Result<AuthzContext, AuthError> {
        match creds {
            Credentials::Bearer(token) if token == "host-token" => self.authz(),
            Credentials::Bearer(_) => Err(AuthError::InvalidCredentials),
        }
    }

    async fn current_auth_epoch(&self, _principal: &proxima_core::OwnerRef) -> u64 {
        self.epoch.load(Ordering::SeqCst)
    }
}

type HostServer = (
    JoinHandle<Result<(), proxima_mcp_server::McpServerError>>,
    SocketAddr,
    String,
);

async fn start_host_auth_server(
    authenticator: Arc<TestHostAuth>,
    revalidation: RevalidationConfig,
) -> Result<HostServer, Box<dyn std::error::Error>> {
    let db_name = create_db().await?;
    let database_url = db_url(&db_name);
    let registry = FlavorRegistry::new();
    let server = McpToolHost::from_database_url(&database_url, registry).await?;
    let auth_store = Arc::new(McpEdgeAuth::headless().with_host(authenticator));
    let (handle, addr) = serve_streamable_http_with_revalidation(
        SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0),
        server,
        default_allowlist(),
        auth_store,
        revalidation,
    )
    .await?;
    Ok((handle, addr, db_name))
}

async fn open_standalone_sse(
    client: &reqwest::Client,
    url: &str,
    session_id: &str,
    bearer: &str,
) -> Result<reqwest::Response, Box<dyn std::error::Error>> {
    let response = client
        .get(url)
        .header("Origin", "http://localhost")
        .header("Authorization", bearer)
        .header("X-Proxima-Owner", common::nil_owner_header())
        .header("MCP-Protocol-Version", "2025-03-26")
        .header("Mcp-Session-Id", session_id)
        .header("Accept", "text/event-stream")
        .send()
        .await?;
    assert!(response.status().is_success(), "{}", response.status());
    Ok(response)
}

async fn initialize_without_origin(
    client: &reqwest::Client,
    url: &str,
    bearer: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let response = client
        .post(url)
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
        .header("X-Proxima-Owner", common::nil_owner_header())
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
        .header("X-Proxima-Owner", common::nil_owner_header())
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

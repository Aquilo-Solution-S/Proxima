use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

mod common;

use async_trait::async_trait;
use common::{create_db, initialize, initialized, split_role_urls};
use proxiima_core::{
    AuthError, AuthPath, Authenticator, AuthzContext, Credentials, OwnerId, RevalidationConfig,
    Role, SpiiceRegistry, UserId,
};
use proxiima_mcp_server::{McpEdgeAuth, McpToolHost};
use serde_json::json;
use tokio::task::JoinHandle;

#[tokio::test]
async fn missing_auth_returns_401() -> Result<(), Box<dyn std::error::Error>> {
    let (handle, addr, db_name) = common::start_server(Arc::new(McpEdgeAuth::headless())).await?;

    let response = reqwest::Client::new()
        .post(format!("http://{addr}/mcp"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}))
        .send()
        .await?;
    assert_eq!(response.status().as_u16(), 401);

    common::stop_server(handle, &db_name).await?;
    Ok(())
}

#[tokio::test]
async fn allowed_browser_preflight_skips_bearer_auth() -> Result<(), Box<dyn std::error::Error>> {
    let (handle, addr, db_name) = common::start_server(Arc::new(McpEdgeAuth::headless())).await?;

    let response = reqwest::Client::new()
        .request(reqwest::Method::OPTIONS, format!("http://{addr}/mcp"))
        .header("Origin", "http://localhost:5173")
        .header("Access-Control-Request-Method", "POST")
        .header(
            "Access-Control-Request-Headers",
            "authorization,content-type,x-proxiima-owner",
        )
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
    assert_eq!(
        response
            .headers()
            .get("Access-Control-Allow-Origin")
            .and_then(|value| value.to_str().ok()),
        Some("http://localhost:5173")
    );
    assert_eq!(
        response
            .headers()
            .get("Access-Control-Allow-Headers")
            .and_then(|value| value.to_str().ok()),
        Some("authorization,content-type,x-proxiima-owner")
    );

    common::stop_server(handle, &db_name).await?;
    Ok(())
}

#[tokio::test]
async fn disallowed_origin_returns_403_with_valid_token() -> Result<(), Box<dyn std::error::Error>>
{
    let auth_store = Arc::new(
        McpEdgeAuth::headless().with_host(Arc::new(TestHostAuth::new(common::nil_owner(), None))),
    );
    let (handle, addr, db_name) = common::start_server(auth_store).await?;

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

    common::stop_server(handle, &db_name).await?;
    Ok(())
}

#[tokio::test]
async fn host_bearer_lists_all_tools_without_origin() -> Result<(), Box<dyn std::error::Error>> {
    let auth_store = Arc::new(
        McpEdgeAuth::headless().with_host(Arc::new(TestHostAuth::new(common::nil_owner(), None))),
    );
    let (handle, addr, db_name) = common::start_server(auth_store).await?;

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
    assert!(
        resource_uris.contains(&"proxiima://tools"),
        "got {resource_uris:?}"
    );

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
    assert!(template_uris.contains(&"proxiima://memory/{id}{?expand_neighbors,version}"));
    assert!(template_uris.contains(&"proxiima://memories{?ids}"));
    assert!(
        template_uris.contains(&"proxiima://memory/{id}/lineage{?direction,depth,limit,cursor}")
    );
    assert!(
        !template_uris
            .iter()
            .any(|uri| uri.contains("proxiima://edges")),
        "proxiima://edges is not a resource; catalog is expand_neighbors/lineage: {template_uris:?}"
    );

    common::stop_server(handle, &db_name).await?;
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

    common::stop_server(handle, &db_name).await?;
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

    common::stop_server(handle, &db_name).await?;
    Ok(())
}

use common::nil_owner;

#[derive(Debug)]
struct TestHostAuth {
    owner: OwnerId,
    subject: Option<UserId>,
    roles: Option<Vec<(OwnerId, Role)>>,
    ttl: Option<Duration>,
    epoch: AtomicU64,
}

impl TestHostAuth {
    fn new(owner: OwnerId, ttl: Option<Duration>) -> Self {
        Self {
            owner,
            subject: None,
            roles: None,
            ttl,
            epoch: AtomicU64::new(0),
        }
    }

    fn with_roles(owner: OwnerId, roles: Vec<(OwnerId, Role)>) -> Self {
        Self {
            owner,
            subject: None,
            roles: Some(roles),
            ttl: None,
            epoch: AtomicU64::new(0),
        }
    }

    fn with_subject_roles(owner: OwnerId, subject: UserId, roles: Vec<(OwnerId, Role)>) -> Self {
        Self {
            owner,
            subject: Some(subject),
            roles: Some(roles),
            ttl: None,
            epoch: AtomicU64::new(0),
        }
    }

    fn bump_epoch(&self) {
        self.epoch.fetch_add(1, Ordering::SeqCst);
    }

    fn authz(&self) -> AuthzContext {
        let subject = self.subject.unwrap_or_else(|| self.owner.as_user());
        let authz = self.roles.as_ref().map_or_else(
            || AuthzContext::for_subject(subject, AuthPath::HostBearer),
            |roles| {
                AuthzContext::for_subject_with_role(subject, roles.clone(), AuthPath::HostBearer)
            },
        );
        authz
            .with_default_owner(self.owner)
            .expect("test default owner has a role")
            .with_expires_at(self.ttl.map(|ttl| std::time::SystemTime::now() + ttl))
            .with_auth_epoch(self.epoch.load(Ordering::SeqCst))
    }
}

#[async_trait]
impl Authenticator for TestHostAuth {
    async fn authenticate(&self, creds: &Credentials) -> Result<AuthzContext, AuthError> {
        match creds {
            Credentials::Bearer(token) if token == "host-token" => Ok(self.authz()),
            Credentials::Bearer(_) => Err(AuthError::InvalidCredentials),
        }
    }

    async fn current_auth_epoch(&self, _principal: &proxiima_core::OwnerId) -> u64 {
        self.epoch.load(Ordering::SeqCst)
    }
}

type HostServer = (
    JoinHandle<Result<(), proxiima_mcp_server::McpServerError>>,
    SocketAddr,
    String,
);

async fn start_host_auth_server(
    authenticator: Arc<TestHostAuth>,
    revalidation: RevalidationConfig,
) -> Result<HostServer, Box<dyn std::error::Error>> {
    let db_name = create_db().await?;
    let (runtime_url, platform_url) = split_role_urls(&db_name).await?;
    let registry = SpiiceRegistry::new();
    let server = McpToolHost::from_database_urls(&runtime_url, &platform_url, registry, 0).await?;
    let auth_store = Arc::new(McpEdgeAuth::headless().with_host(authenticator));
    let (handle, addr) = common::serve_loopback(server, auth_store, revalidation).await?;
    Ok((handle, addr, db_name))
}

async fn start_host_auth_server_on_db(
    authenticator: Arc<TestHostAuth>,
    db_name: &str,
) -> Result<
    (
        JoinHandle<Result<(), proxiima_mcp_server::McpServerError>>,
        SocketAddr,
    ),
    Box<dyn std::error::Error>,
> {
    let (runtime_url, platform_url) = split_role_urls(db_name).await?;
    let server =
        McpToolHost::from_database_urls(&runtime_url, &platform_url, SpiiceRegistry::new(), 0)
            .await?;
    let auth_store = Arc::new(McpEdgeAuth::headless().with_host(authenticator));
    let (handle, addr) =
        common::serve_loopback(server, auth_store, RevalidationConfig::default()).await?;
    Ok((handle, addr))
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
        .header("X-Proxiima-Owner", common::nil_owner_header())
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
        .header("X-Proxiima-Owner", common::nil_owner_header())
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
        .header("X-Proxiima-Owner", common::nil_owner_header())
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
        .header("X-Proxiima-Owner", common::nil_owner_header())
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

async fn initialize_without_owner(
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
                "clientInfo": {"name": "issue-668", "version": "0"}
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
        .to_owned();
    let _ = common::sse_json(response).await?;
    Ok(session_id)
}

async fn post_rpc_without_owner(
    client: &reqwest::Client,
    url: &str,
    session_id: &str,
    bearer: &str,
    body: serde_json::Value,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let response = client
        .post(url)
        .header("Authorization", bearer)
        .header("MCP-Protocol-Version", "2025-03-26")
        .header("Mcp-Session-Id", session_id)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .json(&body)
        .send()
        .await?;
    assert!(response.status().is_success(), "{}", response.status());
    common::sse_json(response).await
}

async fn call_tool_raw(
    client: &reqwest::Client,
    url: &str,
    session_id: &str,
    bearer: &str,
    id: u64,
    name: &str,
    arguments: serde_json::Value,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    post_rpc_without_owner(
        client,
        url,
        session_id,
        bearer,
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments}
        }),
    )
    .await
}

async fn notify_without_owner(
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

async fn read_resource_without_owner(
    client: &reqwest::Client,
    url: &str,
    session_id: &str,
    bearer: &str,
    uri: &str,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    post_rpc_without_owner(
        client,
        url,
        session_id,
        bearer,
        json!({"jsonrpc": "2.0", "id": 71, "method": "resources/read", "params": {"uri": uri}}),
    )
    .await
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "the helper consumes each one-shot JSON-RPC response at the call site"
)]
fn tool_result(value: serde_json::Value) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    if let Some(error) = value.get("error") {
        return Err(format!("tool error: {error}").into());
    }
    if value["result"]["isError"] == true {
        return Err(format!("tool error result: {}", value["result"]).into());
    }
    let content = value["result"]["structuredContent"]
        .as_object()
        .ok_or("tool result missing object structuredContent")?;
    Ok(serde_json::Value::Object(content.clone()))
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "the helper consumes each one-shot JSON-RPC response at the call site"
)]
fn tool_error(value: serde_json::Value) -> String {
    value["error"]["message"]
        .as_str()
        .or_else(|| {
            value["result"]["isError"]
                .as_bool()
                .and_then(|_| value["result"]["content"][0]["text"].as_str())
        })
        .unwrap_or_default()
        .to_owned()
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one HTTP fixture exercises destination selection, all goal actions, lifecycle, and reader isolation"
)]
async fn multi_space_goal_and_memory_lifecycle_uses_one_http_session()
-> Result<(), Box<dyn std::error::Error>> {
    let db_name = create_db().await?;
    let default_owner = OwnerId::from(UserId::new(uuid::Uuid::now_v7()));
    let shared_owner = OwnerId::from(proxiima_core::GroupId::new(uuid::Uuid::now_v7()));
    let auth = Arc::new(TestHostAuth::with_roles(
        default_owner,
        vec![(shared_owner, Role::admin())],
    ));
    let (handle, addr) = start_host_auth_server_on_db(auth, &db_name).await?;
    let client = reqwest::Client::new();
    let url = format!("http://{addr}/mcp");
    let bearer = "Bearer host-token";
    let session = initialize_without_owner(&client, &url, bearer)
        .await
        .map_err(|error| format!("initialize without owner: {error}"))?;
    notify_without_owner(&client, &url, &session, bearer)
        .await
        .map_err(|error| format!("initialized notification: {error}"))?;

    let spaces = tool_result(
        call_tool_raw(
            &client,
            &url,
            &session,
            bearer,
            2,
            "core_memory_spaces",
            json!({}),
        )
        .await
        .map_err(|error| format!("memory spaces: {error}"))?,
    )?;
    let shared_key = shared_owner.to_string();
    assert!(
        spaces["spaces"]
            .as_array()
            .is_some_and(|items| items.iter().any(|space| space["key"] == shared_key))
    );

    let private = tool_result(
        call_tool_raw(
            &client,
            &url,
            &session,
            bearer,
            3,
            "core_remember",
            json!({"title": "private evidence", "body": "private evidence body", "idempotency_key": "issue-668-private"}),
        )
        .await
        .map_err(|error| format!("private remember: {error}"))?,
    )?;
    let private_id = private["handle"]
        .as_str()
        .ok_or("private handle")?
        .to_owned();
    let shared_memory = tool_result(
        call_tool_raw(
            &client,
            &url,
            &session,
            bearer,
            31,
            "core_remember",
            json!({"space": shared_key, "title": "shared lifecycle memory", "body": "shared lifecycle body", "idempotency_key": "issue-668-shared-memory"}),
        )
        .await?,
    )?;
    let shared_memory_id = shared_memory["handle"]
        .as_str()
        .ok_or("shared memory handle")?
        .to_owned();
    let perspective = tool_result(
        call_tool_raw(
            &client,
            &url,
            &session,
            bearer,
            32,
            "core_interpret",
            json!({"space": shared_key, "claim": "the shared lifecycle memory is useful evidence", "confidence": 90, "subjects": [shared_memory_id]}),
        )
        .await?,
    )?;
    let perspective_id = perspective["handle"]
        .as_str()
        .ok_or("perspective handle")?
        .to_owned();

    let goal = tool_result(
        call_tool_raw(
            &client,
            &url,
            &session,
            bearer,
            4,
            "core_goal",
            json!({"action": "set", "space": shared_key, "schema_id": "core/simple-text-v1", "title": "shared goal", "text": "shared goal body", "body": {}, "evidence": [private_id], "motivated_by": [perspective_id], "idempotency_key": "issue-668-goal"}),
        )
        .await
        .map_err(|error| format!("goal set: {error}"))?,
    )?;
    let goal_id = goal["handle"].as_str().ok_or("goal handle")?.to_owned();
    let default_goal = tool_result(
        call_tool_raw(
            &client,
            &url,
            &session,
            bearer,
            41,
            "core_goal",
            json!({"action": "set", "schema_id": "core/simple-text-v1", "title": "default goal", "text": "default goal body", "body": {}, "idempotency_key": "issue-668-default-goal"}),
        )
        .await?,
    )?;
    let default_goal_id = default_goal["handle"]
        .as_str()
        .ok_or("default goal handle")?
        .to_owned();

    let transition = tool_result(
        call_tool_raw(
            &client,
            &url,
            &session,
            bearer,
            5,
            "core_goal",
            json!({"action": "transition", "space": shared_key, "goal": goal_id, "transition": "pause", "pending": "keep", "idempotency_key": "issue-668-pause"}),
        )
        .await?,
    )?;
    assert!(transition["handle"].as_str().is_some());

    let resumed = tool_result(
        call_tool_raw(
            &client,
            &url,
            &session,
            bearer,
            51,
            "core_goal",
            json!({"action": "transition", "space": shared_key, "goal": transition["handle"], "transition": "resume", "idempotency_key": "issue-668-resume"}),
        )
        .await?,
    )?;
    let modified = tool_result(
        call_tool_raw(
            &client,
            &url,
            &session,
            bearer,
            52,
            "core_goal",
            json!({"action": "modify", "space": shared_key, "goal": resumed["handle"], "schema_id": "core/simple-text-v1", "title": "shared goal revised", "text": "shared goal revised body", "body": {}, "idempotency_key": "issue-668-modify"}),
        )
        .await?,
    )?;
    let achieved = tool_result(
        call_tool_raw(
            &client,
            &url,
            &session,
            bearer,
            53,
            "core_goal",
            json!({"action": "mark_achieved", "space": shared_key, "goal": modified["handle"], "evidence": [private_id], "idempotency_key": "issue-668-achieve"}),
        )
        .await?,
    )?;
    assert!(achieved["handle"].as_str().is_some());
    let parent = tool_result(
        call_tool_raw(
            &client,
            &url,
            &session,
            bearer,
            54,
            "core_goal",
            json!({"action": "set", "space": shared_key, "schema_id": "core/simple-text-v1", "title": "decomposition parent", "text": "decomposition parent body", "body": {}, "idempotency_key": "issue-668-parent"}),
        )
        .await?,
    )?;
    let decomposed = tool_result(
        call_tool_raw(
            &client,
            &url,
            &session,
            bearer,
            55,
            "core_goal",
            json!({"action": "decompose", "space": shared_key, "parent_goal": parent["handle"], "children": [{"schema_id": "core/simple-text-v1", "title": "child one", "text": "child one body", "body": {}}, {"schema_id": "core/simple-text-v1", "title": "child two", "text": "child two body", "body": {}}], "idempotency_key": "issue-668-decompose"}),
        )
        .await?,
    )?;
    assert_eq!(decomposed["children"].as_array().map(Vec::len), Some(2));

    for (id, action, arguments) in [
        (
            61,
            "transition",
            json!({"action": "transition", "space": default_owner.to_string(), "goal": parent["handle"], "transition": "pause", "pending": "keep", "idempotency_key": "issue-668-wrong-transition"}),
        ),
        (
            62,
            "modify",
            json!({"action": "modify", "space": default_owner.to_string(), "goal": parent["handle"], "schema_id": "core/simple-text-v1", "title": "wrong space", "text": "wrong space body", "body": {}, "idempotency_key": "issue-668-wrong-modify"}),
        ),
        (
            63,
            "mark_achieved",
            json!({"action": "mark_achieved", "space": default_owner.to_string(), "goal": parent["handle"], "idempotency_key": "issue-668-wrong-achieve"}),
        ),
        (
            64,
            "decompose",
            json!({"action": "decompose", "space": default_owner.to_string(), "parent_goal": parent["handle"], "children": [{"schema_id": "core/simple-text-v1", "title": "wrong child", "text": "wrong child body", "body": {}}], "idempotency_key": "issue-668-wrong-decompose"}),
        ),
    ] {
        let refused =
            call_tool_raw(&client, &url, &session, bearer, id, "core_goal", arguments).await?;
        assert!(
            tool_error(refused)
                .to_ascii_lowercase()
                .contains("not found"),
            "{action} against the wrong owner must refuse without a write"
        );
    }

    let invalid_space = post_rpc_without_owner(
        &client,
        &url,
        &session,
        bearer,
        json!({"jsonrpc": "2.0", "id": 6, "method": "tools/call", "params": {"name": "core_goal", "arguments": {"action": "transition", "space": "unknown-space", "goal": goal_id, "transition": "resume", "idempotency_key": "issue-668-invalid"}}}),
    )
    .await?;
    assert!(tool_error(invalid_space).contains("unknown memory space"));

    let forgotten = tool_result(
        post_rpc_without_owner(
            &client,
            &url,
            &session,
            bearer,
            json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": {"name": "core_forget", "arguments": {"space": shared_key, "memory": shared_memory_id}}}),
        )
        .await?,
    )?;
    assert_eq!(forgotten["ok"], true);
    let forgotten_read = read_resource_without_owner(
        &client,
        &url,
        &session,
        bearer,
        &format!("proxiima://memory/{shared_memory_id}"),
    )
    .await?;
    assert!(
        forgotten_read.get("error").is_some(),
        "forgotten selected-space memory must be hidden by id: {forgotten_read}"
    );

    let restored = tool_result(
        post_rpc_without_owner(
            &client,
            &url,
            &session,
            bearer,
            json!({"jsonrpc": "2.0", "id": 8, "method": "tools/call", "params": {"name": "core_restore", "arguments": {"space": shared_key, "memory": shared_memory_id}}}),
        )
        .await?,
    )?;
    assert_eq!(restored["ok"], true);
    let restored_read = read_resource_without_owner(
        &client,
        &url,
        &session,
        bearer,
        &format!("proxiima://memory/{shared_memory_id}"),
    )
    .await?;
    assert!(
        restored_read.get("error").is_none(),
        "restored selected-space memory must be readable by id: {restored_read}"
    );

    let admin_url = proxiima_pg_testkit::db_url(&db_name);
    let admin = sqlx::PgPool::connect(&admin_url).await?;
    let stored_owner: uuid::Uuid =
        sqlx::query_scalar("SELECT owner_id FROM proxiima_core.goal WHERE t = $1")
            .bind(uuid::Uuid::parse_str(&goal_id)?)
            .fetch_one(&admin)
            .await?;
    assert_eq!(stored_owner, shared_owner.into_inner());
    let default_stored_owner: uuid::Uuid =
        sqlx::query_scalar("SELECT owner_id FROM proxiima_core.goal WHERE t = $1")
            .bind(uuid::Uuid::parse_str(&default_goal_id)?)
            .fetch_one(&admin)
            .await?;
    assert_eq!(default_stored_owner, default_owner.into_inner());
    let motivation: (Option<uuid::Uuid>, Vec<uuid::Uuid>) =
        sqlx::query_as("SELECT assignment_t, motivated_by FROM proxiima_core.goal WHERE t = $1")
            .bind(uuid::Uuid::parse_str(&goal_id)?)
            .fetch_one(&admin)
            .await?;
    assert_eq!(
        motivation,
        (None, vec![uuid::Uuid::parse_str(&perspective_id)?]),
        "exact motivating Perspective is stored in the selected space"
    );
    let refused_writes: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM proxiima_core.goal WHERE request_id LIKE '%issue-668-wrong-%'",
    )
    .fetch_one(&admin)
    .await?;
    assert_eq!(refused_writes, 0, "wrong-space actions commit no goal rows");
    admin.close().await;

    handle.abort();
    let _ = handle.await;
    let reader_auth = Arc::new(TestHostAuth::with_subject_roles(
        shared_owner,
        UserId::new(uuid::Uuid::now_v7()),
        vec![(shared_owner, Role::viewer())],
    ));
    let (reader_handle, reader_addr) = start_host_auth_server_on_db(reader_auth, &db_name).await?;
    let reader_url = format!("http://{reader_addr}/mcp");
    let reader_session = initialize_without_owner(&client, &reader_url, bearer).await?;
    notify_without_owner(&client, &reader_url, &reader_session, bearer).await?;
    let goal_read = read_resource_without_owner(
        &client,
        &reader_url,
        &reader_session,
        bearer,
        &format!("proxiima://goal/{goal_id}"),
    )
    .await?;
    assert!(
        goal_read.get("error").is_none(),
        "shared-space reader can read the goal: {goal_read}"
    );
    let goal_payload: serde_json::Value = serde_json::from_str(
        goal_read["result"]["contents"][0]["text"]
            .as_str()
            .ok_or("goal resource content")?,
    )?;
    assert_eq!(
        goal_payload["evidence"],
        json!([private_id]),
        "shared-space reader sees the goal's declared private evidence UUID"
    );
    let private_read = read_resource_without_owner(
        &client,
        &reader_url,
        &reader_session,
        bearer,
        &format!("proxiima://memory/{private_id}"),
    )
    .await?;
    assert!(
        private_read.get("error").is_some(),
        "shared-space reader cannot read private evidence: {private_read}"
    );
    reader_handle.abort();
    let _ = reader_handle.await;
    let viewer_default = OwnerId::from(proxiima_core::GroupId::new(uuid::Uuid::now_v7()));
    let catalog_auth = Arc::new(TestHostAuth::with_subject_roles(
        viewer_default,
        UserId::new(uuid::Uuid::now_v7()),
        vec![
            (viewer_default, Role::viewer()),
            (shared_owner, Role::admin()),
        ],
    ));
    let (catalog_handle, catalog_addr) =
        start_host_auth_server_on_db(catalog_auth, &db_name).await?;
    let catalog_url = format!("http://{catalog_addr}/mcp");
    let catalog_session = initialize_without_owner(&client, &catalog_url, bearer).await?;
    notify_without_owner(&client, &catalog_url, &catalog_session, bearer).await?;
    let catalog = post_rpc_without_owner(
        &client,
        &catalog_url,
        &catalog_session,
        bearer,
        json!({"jsonrpc": "2.0", "id": 91, "method": "tools/list", "params": {}}),
    )
    .await?;
    let names = catalog["result"]["tools"]
        .as_array()
        .ok_or("tools/list array")?;
    assert!(
        names.iter().any(|tool| tool["name"] == "core_goal"),
        "a viewer of the default owner with Goal write elsewhere still sees core_goal"
    );
    let catalog_goal = call_tool_raw(
        &client,
        &catalog_url,
        &catalog_session,
        bearer,
        92,
        "core_goal",
        json!({"action": "set", "space": shared_owner.to_string(), "schema_id": "core/simple-text-v1", "title": "catalog shared goal", "text": "catalog shared goal body", "body": {}, "idempotency_key": "issue-668-catalog-shared"}),
    )
    .await?;
    assert!(tool_result(catalog_goal).is_ok());
    let default_refused = call_tool_raw(
        &client,
        &catalog_url,
        &catalog_session,
        bearer,
        93,
        "core_goal",
        json!({"action": "set", "schema_id": "core/simple-text-v1", "title": "catalog default goal", "text": "catalog default goal body", "body": {}, "idempotency_key": "issue-668-catalog-default"}),
    )
    .await?;
    assert!(
        tool_error(default_refused)
            .to_ascii_lowercase()
            .contains("not authorized"),
        "viewer default owner cannot write a Goal there"
    );
    common::stop_server(catalog_handle, &db_name).await?;
    Ok(())
}

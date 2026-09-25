//! The host-side MCP surface, end to end against a booted runtime: a host
//! tool listed and called over `/mcp` through the registry's request
//! behaviors, the call recorded under the verified subject, the resolved
//! edge serving `/mcp` behind bearer auth beside open host routes, and an
//! authenticator built from the platform scope.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use axum::routing::get;
use proxima::flavor::{FlavorBundle, FlavorRegistry, FlavorRegistryError, NamedMigrator};
use proxima::{
    AppInfo, AuthzContext, FlavorApp, McpHostTool, McpHostToolCall, McpHostTools, Next, Proxima,
    QueryRequest, RequestBehavior, ToolCall, ToolScope,
};
use proxima_core::verbs::mcp_call_history::McpCallHistoryRequest;
use proxima_core::{
    AuthError, AuthPath, Authenticator, Credentials, McpToolAnnotations, McpToolError, OwnerRef,
    OwnerRoles, UserId,
};
use proxima_pg_testkit::SplitRoleDb;
use serde_json::{Value, json};
use tower::util::ServiceExt;
use uuid::Uuid;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const TOKEN: &str = "host-token";
const HOST_TOOL: &str = "host_echo";

/// Every call name the flavor's request behavior saw, in order.
static SEEN: Mutex<Vec<String>> = Mutex::new(Vec::new());

#[derive(Debug)]
struct Seen;

#[async_trait]
impl RequestBehavior for Seen {
    async fn handle(&self, call: ToolCall, next: Next<'_>) -> Result<Value, McpToolError> {
        SEEN.lock().expect("seen").push(call.name.clone());
        next.run(call).await
    }
}

struct HostApp;

impl FlavorBundle for HostApp {
    fn register(registry: &mut FlavorRegistry) -> Result<(), FlavorRegistryError> {
        registry.add_request_behavior(Seen);
        Ok(())
    }

    fn migrators() -> Vec<NamedMigrator> {
        Vec::new()
    }
}

impl FlavorApp for HostApp {
    fn app_info() -> AppInfo {
        AppInfo {
            id: "mcp-host-surface",
            title: "mcp-host-surface",
            version: "0",
        }
    }
}

#[derive(Debug)]
struct EchoTools;

#[async_trait]
impl McpHostTools for EchoTools {
    fn list(&self, _auth: &proxima::McpAuthContext) -> Vec<McpHostTool> {
        vec![McpHostTool {
            name: HOST_TOOL.into(),
            description: "Echo the call back.".into(),
            args_schema: json!({"type": "object"}),
            output_schema: json!({"type": "object"}),
            annotations: McpToolAnnotations::new().read_only(true).open_world(false),
        }]
    }

    async fn call(&self, call: ToolCall) -> Result<Value, McpToolError> {
        let marker = call
            .ctx
            .services
            .get::<McpHostToolCall>()
            .ok_or_else(|| McpToolError::Other("no host-call marker".into()))?;
        Ok(json!({"tool": call.name, "marker": marker.name(), "args": call.args}))
    }
}

struct StubAuth {
    subject: UserId,
}

#[async_trait]
impl Authenticator for StubAuth {
    async fn authenticate(&self, creds: &Credentials) -> Result<AuthzContext, AuthError> {
        match creds {
            Credentials::Bearer(token) if token == TOKEN => Ok(AuthzContext::for_subject(
                self.subject,
                AuthPath::HostBearer,
            )),
            Credentials::Bearer(_) => Err(AuthError::InvalidCredentials),
        }
    }
}

fn app(db: &SplitRoleDb) -> Proxima<HostApp> {
    Proxima::<HostApp>::app()
        .database_url(db.runtime_url())
        .platform_database_url(db.platform_url())
        .with_mcp()
        .mcp_bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .tool_scope(ToolScope::All)
        .host_tools(Arc::new(EchoTools))
}

fn mcp_post(client: &reqwest::Client, base: &str, subject: UserId) -> reqwest::RequestBuilder {
    client
        .post(format!("{base}/mcp"))
        .header("Origin", "http://localhost")
        .header("Authorization", format!("Bearer {TOKEN}"))
        .header(
            "X-Proxima-Owner",
            proxima_mcp_server::owner_key(OwnerRef::Personal(subject)),
        )
        .header("MCP-Protocol-Version", "2025-03-26")
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
}

async fn sse_json(response: reqwest::Response) -> TestResult<Value> {
    let text = response.text().await?;
    text.lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .find_map(|data| serde_json::from_str(data.trim()).ok())
        .ok_or_else(|| format!("missing JSON SSE data in {text:?}").into())
}

async fn open_session(client: &reqwest::Client, base: &str, subject: UserId) -> TestResult<String> {
    let response = mcp_post(client, base, subject)
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": {"name": "host-surface-test", "version": "0"}
            }
        }))
        .send()
        .await?;
    assert!(response.status().is_success(), "{}", response.status());
    let session = response
        .headers()
        .get("Mcp-Session-Id")
        .ok_or("missing Mcp-Session-Id")?
        .to_str()?
        .to_owned();
    sse_json(response).await?;
    let ack = mcp_post(client, base, subject)
        .header("Mcp-Session-Id", &session)
        .json(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
        .send()
        .await?;
    assert!(ack.status().is_success(), "{}", ack.status());
    Ok(session)
}

async fn rpc(
    client: &reqwest::Client,
    base: &str,
    subject: UserId,
    session: &str,
    body: Value,
) -> TestResult<Value> {
    let response = mcp_post(client, base, subject)
        .header("Mcp-Session-Id", session)
        .json(&body)
        .send()
        .await?;
    assert!(response.status().is_success(), "{}", response.status());
    sse_json(response).await
}

/// Recorded off the request path: poll until the served calls land. The
/// history read filters on the actor, so a hit proves the actor is the
/// verified subject, not a claim from the request.
async fn assert_only_served_calls_recorded(
    engine: &proxima::Engine,
    subject: UserId,
) -> TestResult {
    let reader = proxima_core::test_fixtures::authenticated_context(AuthzContext::for_subject(
        subject,
        AuthPath::HostBearer,
    ));
    let request = McpCallHistoryRequest {
        owner: OwnerRef::Personal(subject),
        actor_oid: Some(subject.into_inner().to_string()),
        limit: 10,
        include_body: true,
        before: None,
    };
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let calls = proxima::read_mcp_call_history(engine, &reader, &request)
                .await
                .expect("history read")
                .calls;
            if calls.len() >= 2 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await?;
    // A late record for the unknown name would land by now.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let recorded = proxima::read_mcp_call_history(engine, &reader, &request)
        .await?
        .calls;
    assert_eq!(recorded.len(), 2, "{recorded:?}");
    assert!(recorded.iter().all(|call| call.tool_name == HOST_TOOL));
    let ok = recorded.iter().find(|call| call.ok).ok_or("no ok record")?;
    let refused = recorded
        .iter()
        .find(|call| !call.ok)
        .ok_or("no failed record")?;
    assert_eq!(refused.error.as_deref(), Some("jsonrpc -32602"));
    assert!(ok.io_truncated, "no body is stored");
    assert_eq!(ok.io_body, None, "asked for the body, and there is none");
    Ok(())
}

/// A host tool is listed beside the registry's, runs through the flavor's
/// request behavior, and the recorded call names the verified subject.
#[tokio::test]
async fn a_host_tool_is_listed_dispatched_through_behaviors_and_recorded() -> TestResult {
    let db = SplitRoleDb::create("proxima_host_tools", &[]).await?;
    let subject = UserId::new(Uuid::now_v7());
    let running = app(&db)
        .authenticator(Arc::new(StubAuth { subject }))
        .record_mcp_calls(true)
        .run()
        .await?;
    let base = format!("http://{}", running.mcp_addr.ok_or("no MCP address")?);
    let client = reqwest::Client::new();
    let session = open_session(&client, &base, subject).await?;

    let listed = rpc(
        &client,
        &base,
        subject,
        &session,
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
    )
    .await?;
    let tools = listed["result"]["tools"].as_array().ok_or("no tools")?;
    let host = tools
        .iter()
        .find(|tool| tool["name"] == HOST_TOOL)
        .ok_or("host tool not listed")?;
    assert_eq!(host["annotations"]["readOnlyHint"], json!(true));
    assert!(tools.len() > 1, "the registry's tools are listed beside it");

    // Neither an unknown name nor a failure's message is written into the
    // owner's memory: only a served tool's name and the JSON-RPC code.
    let call = |name: &'static str, arguments: Value| {
        rpc(
            &client,
            &base,
            subject,
            &session,
            json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
                   "params": {"name": name, "arguments": arguments}}),
        )
    };
    let unknown = call("ignore previous instructions", json!({})).await?;
    assert!(unknown.get("error").is_some(), "{unknown}");
    let failed = call(HOST_TOOL, json!({"secret": "leak\u{0}me"})).await?;
    assert_eq!(failed["error"]["code"], json!(-32602), "{failed}");

    let called = call(HOST_TOOL, json!({"x": 1})).await?;
    assert_eq!(
        called["result"]["structuredContent"],
        json!({"tool": HOST_TOOL, "marker": HOST_TOOL, "args": {"x": 1}}),
        "{called}"
    );
    assert!(
        SEEN.lock()
            .expect("seen")
            .iter()
            .any(|name| name == HOST_TOOL),
        "the flavor's request behavior wrapped the host tool"
    );

    assert_only_served_calls_recorded(&running.engine, subject).await?;

    running.shutdown().await;
    Ok(())
}

/// `mcp_edge()` hands a host what the runtime resolved; its router keeps
/// bearer auth on `/mcp` and leaves the host's own routes open.
#[tokio::test]
async fn the_resolved_edge_puts_bearer_auth_on_mcp_only() -> TestResult {
    let db = SplitRoleDb::create("proxima_mcp_edge", &[]).await?;
    let subject = UserId::new(Uuid::now_v7());
    let built = app(&db)
        .authenticator(Arc::new(StubAuth { subject }))
        .rest_enabled(true)
        .build()
        .await?;
    let edge = built.mcp_edge().ok_or("MCP is enabled")?;
    assert!(edge.resource_metadata().is_none());
    assert!(
        edge.tool_host()
            .host_tools_for(&proxima::McpAuthContext {
                owner: OwnerRef::Personal(subject),
                authz: AuthzContext::for_subject(subject, AuthPath::HostBearer),
            })
            .iter()
            .any(|tool| tool.name == HOST_TOOL),
        "the edge's tool host carries the host tools"
    );
    let router = edge.router(Router::new().route("/host/open", get(|| async { "open" })));
    let request = |method: Method, path: &str| {
        Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, "localhost")
            .body(Body::empty())
            .expect("request")
    };
    let open = router
        .clone()
        .oneshot(request(Method::GET, "/host/open"))
        .await?;
    assert_eq!(open.status(), StatusCode::OK, "host routes carry no bearer");
    let foreign = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/host/open")
                .header(header::HOST, "rebind.example")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(
        foreign.status(),
        StatusCode::FORBIDDEN,
        "the Host guard covers the host's routes too"
    );
    #[cfg(feature = "rest")]
    {
        let rest = router
            .clone()
            .oneshot(request(Method::GET, "/v1/openapi.json"))
            .await?;
        assert_eq!(
            rest.status(),
            StatusCode::UNAUTHORIZED,
            "/v1 keeps bearer auth"
        );
    }
    let mcp = router.oneshot(request(Method::POST, "/mcp")).await?;
    assert_eq!(
        mcp.status(),
        StatusCode::UNAUTHORIZED,
        "/mcp keeps bearer auth"
    );

    // No bearer at all: the runtime's witness mints a sealed System context.
    let system = AuthzContext::for_system(
        built.system_authority(),
        OwnerRoles::for_subject(subject, [])?,
    );
    assert_eq!(system.auth_path(), AuthPath::System);
    built
        .engine
        .query(&system, &QueryRequest::readable())
        .await
        .map_err(|err| format!("a System context reads: {err}"))?;
    built.shutdown();
    Ok(())
}

/// The factory runs at boot with the validated platform scope, and what it
/// returns is the authenticator `/mcp` uses.
#[tokio::test]
async fn the_authenticator_is_built_after_the_platform_scope() -> TestResult {
    let refused = Proxima::<HostApp>::app()
        .database_url("postgres://unused.invalid/none")
        .tool_scope(ToolScope::All)
        .authenticator_with_platform_scope(|_| Err(proxima::ProximaError::Config("unused".into())))
        .build()
        .await
        .expect_err("no platform URL, no platform scope");
    assert!(
        refused.to_string().contains("platform_database_url"),
        "refused before storage: {refused}"
    );

    let db = SplitRoleDb::create("proxima_platform_auth", &[]).await?;
    let subject = UserId::new(Uuid::now_v7());
    let built_with_scope = Arc::new(AtomicBool::new(false));
    let seen = Arc::clone(&built_with_scope);
    let running = app(&db)
        .authenticator_with_platform_scope(move |context| {
            let _scope = context.platform_scope;
            seen.store(true, Ordering::SeqCst);
            Ok(Arc::new(StubAuth { subject }))
        })
        .run()
        .await?;
    assert!(built_with_scope.load(Ordering::SeqCst));
    let base = format!("http://{}", running.mcp_addr.ok_or("no MCP address")?);
    open_session(&reqwest::Client::new(), &base, subject).await?;
    running.shutdown().await;
    Ok(())
}

const SLEEP_TOOL: &str = "host_sleep";

/// Sleeps `ms`, then answers — a tool call that sends nothing while it runs.
#[derive(Debug)]
struct SleepTools;

#[async_trait]
impl McpHostTools for SleepTools {
    fn list(&self, _auth: &proxima::McpAuthContext) -> Vec<McpHostTool> {
        vec![McpHostTool {
            name: SLEEP_TOOL.into(),
            description: "Sleep, then answer.".into(),
            args_schema: json!({"type": "object"}),
            output_schema: json!({"type": "object"}),
            annotations: McpToolAnnotations::new().read_only(true).open_world(false),
        }]
    }

    async fn call(&self, call: ToolCall) -> Result<Value, McpToolError> {
        let ms = call.args["ms"].as_u64().unwrap_or(0);
        tokio::time::sleep(Duration::from_millis(ms)).await;
        Ok(json!({"slept_ms": ms}))
    }
}

/// Every JSON message on an SSE response, in order.
async fn sse_messages(response: reqwest::Response) -> TestResult<Vec<Value>> {
    let text = response.text().await?;
    Ok(text
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .filter_map(|data| serde_json::from_str(data.trim()).ok())
        .collect())
}

/// rmcp closes a session after its idle timeout without session traffic,
/// and a tool call still running is none: its result was lost (issue #347).
/// A call that carries a progress token gets a heartbeat that is traffic;
/// one that does not needs the idle timeout raised or switched off.
#[tokio::test]
async fn a_tool_call_outlives_the_session_idle_timeout_on_a_heartbeat_or_without_one() -> TestResult
{
    const SLEEP_MS: u64 = 2_500;
    let db = SplitRoleDb::create("proxima_session_idle", &[]).await?;
    let subject = UserId::new(Uuid::now_v7());
    let serve = |idle: Option<Duration>| {
        Proxima::<HostApp>::app()
            .database_url(db.runtime_url())
            .platform_database_url(db.platform_url())
            .with_mcp()
            .mcp_bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .tool_scope(ToolScope::All)
            .host_tools(Arc::new(SleepTools))
            .mcp_transport(proxima_mcp_server::McpTransportConfig {
                session_idle_timeout: idle,
                ..proxima_mcp_server::McpTransportConfig::default()
            })
            .authenticator(Arc::new(StubAuth { subject }))
            .run()
    };
    let client = reqwest::Client::new();
    let sleep_call = |meta: Value| {
        json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call",
               "params": {"name": SLEEP_TOOL, "arguments": {"ms": SLEEP_MS}, "_meta": meta}})
    };

    let running = serve(Some(Duration::from_secs(1))).await?;
    let base = format!("http://{}", running.mcp_addr.ok_or("no MCP address")?);

    // With a progress token: beats every 500 ms keep the session, and the
    // result arrives after them.
    let session = open_session(&client, &base, subject).await?;
    let response = mcp_post(&client, &base, subject)
        .header("Mcp-Session-Id", &session)
        .json(&sleep_call(json!({"progressToken": "ingest-1"})))
        .send()
        .await?;
    let messages = sse_messages(response).await?;
    let beats: Vec<&Value> = messages
        .iter()
        .filter(|message| message["method"] == "notifications/progress")
        .collect();
    assert!(beats.len() >= 2, "{messages:?}");
    assert!(
        beats
            .iter()
            .all(|beat| beat["params"]["progressToken"] == "ingest-1")
    );
    let result = messages.last().ok_or("no messages")?;
    assert_eq!(result["id"], json!(7), "{messages:?}");
    assert_eq!(
        result["result"]["structuredContent"],
        json!({"slept_ms": SLEEP_MS})
    );

    // Without one, nothing crosses the session while the call runs: the
    // session closes under it and the result never arrives.
    let session = open_session(&client, &base, subject).await?;
    let response = mcp_post(&client, &base, subject)
        .header("Mcp-Session-Id", &session)
        .json(&sleep_call(json!({})))
        .send()
        .await?;
    let messages = sse_messages(response).await?;
    assert!(
        messages
            .iter()
            .all(|message| message.get("result").is_none()),
        "{messages:?}"
    );
    let after = mcp_post(&client, &base, subject)
        .header("Mcp-Session-Id", &session)
        .json(&json!({"jsonrpc": "2.0", "id": 8, "method": "tools/list", "params": {}}))
        .send()
        .await?;
    // rmcp answers 500 (worker gone) or 404 (handle gone): either way the
    // session is closed.
    assert!(
        !after.status().is_success(),
        "session closed: {}",
        after.status()
    );
    running.shutdown().await;

    // With the idle timeout off, the same call needs no token.
    let running = serve(None).await?;
    let base = format!("http://{}", running.mcp_addr.ok_or("no MCP address")?);
    let session = open_session(&client, &base, subject).await?;
    let response = mcp_post(&client, &base, subject)
        .header("Mcp-Session-Id", &session)
        .json(&sleep_call(json!({})))
        .send()
        .await?;
    let messages = sse_messages(response).await?;
    let result = messages.last().ok_or("no messages")?;
    assert_eq!(
        result["result"]["structuredContent"],
        json!({"slept_ms": SLEEP_MS}),
        "{messages:?}"
    );
    running.shutdown().await;
    Ok(())
}

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

    let called = rpc(
        &client,
        &base,
        subject,
        &session,
        json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {"name": HOST_TOOL, "arguments": {"x": 1}}
        }),
    )
    .await?;
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

    // Recorded off the request path: poll until it lands. The history read
    // filters on the actor, so a hit proves the actor is the verified
    // subject, not a claim from the request.
    let owner = OwnerRef::Personal(subject);
    let reader = proxima_core::test_fixtures::authenticated_context(AuthzContext::for_subject(
        subject,
        AuthPath::HostBearer,
    ));
    let request = McpCallHistoryRequest {
        owner,
        actor_oid: Some(subject.into_inner().to_string()),
        limit: 10,
        include_body: true,
        before: None,
    };
    let recorded = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let calls = proxima::read_mcp_call_history(&running.engine, &reader, &request)
                .await
                .expect("history read")
                .calls;
            if !calls.is_empty() {
                return calls;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await?;
    assert_eq!(recorded.len(), 1, "{recorded:?}");
    assert_eq!(recorded[0].tool_name, HOST_TOOL);
    assert!(recorded[0].ok);
    assert!(recorded[0].io_truncated, "no body is stored");
    assert_eq!(
        recorded[0].io_body, None,
        "asked for the body, and there is none"
    );

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

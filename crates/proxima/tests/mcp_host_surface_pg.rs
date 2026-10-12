//! The host-side MCP surface, end to end against a booted runtime: a host
//! tool listed and called over `/mcp` through the registry's request
//! behaviors, the call recorded under the verified subject, the resolved
//! edge serving `/mcp` behind bearer auth beside open host routes, and an
//! authenticator built from the platform scope.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
    QueryRequest, RequestBehavior, ToolCall, ToolContent, ToolReply, ToolScope,
};
use proxima_core::verbs::mcp_call_history::{McpCallHistoryRequest, McpCallRecord};
use proxima_core::{
    AuthError, AuthPath, Authenticator, Credentials, McpToolError, OwnerRef, OwnerRoles,
    ToolEffect, UserId,
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
    async fn handle(&self, call: ToolCall, next: Next<'_>) -> Result<ToolReply, McpToolError> {
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
    async fn list(
        &self,
        _auth: &proxima::McpAuthContext,
    ) -> Result<Vec<McpHostTool>, McpToolError> {
        Ok(vec![
            McpHostTool::new(
                HOST_TOOL,
                "Echo the call back.",
                json!({"type": "object"}),
                ToolEffect::ReadOnly,
            )
            .with_output_schema(json!({"type": "object"})),
        ])
    }

    async fn call(&self, call: ToolCall) -> Result<ToolReply, McpToolError> {
        let marker = call
            .ctx
            .services
            .get::<McpHostToolCall>()
            .ok_or_else(|| McpToolError::Other("no host-call marker".into()))?;
        Ok(ToolReply::Structured(
            json!({"tool": call.name, "marker": marker.name(), "args": call.args}),
        ))
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
    let base = format!("http://{}", running.mcp_addr().ok_or("no MCP address")?);
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

    assert_only_served_calls_recorded(running.host().engine(), subject).await?;

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
            .await?
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
        .host()
        .engine()
        .query(&system, &QueryRequest::readable())
        .await
        .map_err(|err| format!("a System context reads: {err}"))?;
    built.shutdown().await;
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
    let base = format!("http://{}", running.mcp_addr().ok_or("no MCP address")?);
    open_session(&reqwest::Client::new(), &base, subject).await?;
    running.shutdown().await;
    Ok(())
}

const FAILING_TOOL: &str = "host_fail";
const CONTENT_TOOL: &str = "host_content";
const STRUCTURED_FAILING_TOOL: &str = "host_fail_structured";

/// Three tools without an output schema: one answers content, one a tool
/// failure, one a structured failure. Counts how often a request reads the
/// catalog.
#[derive(Debug, Clone)]
struct ReplyTools {
    lists: Arc<AtomicUsize>,
}

#[async_trait]
impl McpHostTools for ReplyTools {
    async fn list(
        &self,
        _auth: &proxima::McpAuthContext,
    ) -> Result<Vec<McpHostTool>, McpToolError> {
        self.lists.fetch_add(1, Ordering::SeqCst);
        Ok([FAILING_TOOL, STRUCTURED_FAILING_TOOL, CONTENT_TOOL]
            .into_iter()
            .map(|name| {
                McpHostTool::new(
                    name,
                    "Answers a fixed reply.",
                    json!({"type": "object"}),
                    ToolEffect::ReadOnly,
                )
            })
            .collect())
    }

    async fn call(&self, call: ToolCall) -> Result<ToolReply, McpToolError> {
        match call.name.as_str() {
            FAILING_TOOL => Ok(ToolReply::Failure(vec![ToolContent::text(
                "upstream said no",
            )])),
            STRUCTURED_FAILING_TOOL => Ok(ToolReply::StructuredFailure {
                structured: serde_json::Map::from_iter([
                    ("code".to_owned(), json!("quota_exhausted")),
                    ("retry_after_s".to_owned(), json!(60)),
                ]),
                content: vec![ToolContent::text("quota exhausted; retry in 60s")],
            }),
            _ => Ok(ToolReply::Content(vec![
                ToolContent::text("hello"),
                ToolContent::image([0xfb, 0xff], "image/png"),
            ])),
        }
    }
}

/// The owner's recorded calls once at least `count` have landed: recording
/// runs off the request path.
async fn recorded_calls(
    engine: &proxima::Engine,
    subject: UserId,
    count: usize,
) -> TestResult<Vec<McpCallRecord>> {
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
    let calls = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let calls = proxima::read_mcp_call_history(engine, &reader, &request)
                .await
                .expect("history read")
                .calls;
            if calls.len() >= count {
                return calls;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await?;
    Ok(calls)
}

/// A tool failure result: `isError`, the one text block, and
/// `structuredContent` exactly when `structured` is given.
fn assert_failure_result(reply: &Value, text: &str, structured: Option<Value>) {
    let result = &reply["result"];
    assert_eq!(result["isError"], json!(true), "{reply}");
    assert_eq!(
        result["content"],
        json!([{"type": "text", "text": text}]),
        "{reply}"
    );
    match structured {
        Some(expected) => assert_eq!(result["structuredContent"], expected, "{reply}"),
        None => assert!(result.get("structuredContent").is_none(), "{reply}"),
    }
}

/// `tool`'s call record is a failure: not ok, the fixed error, no content.
fn assert_recorded_failure(recorded: &[McpCallRecord], tool: &str) {
    let record = recorded
        .iter()
        .find(|call| call.tool_name == tool)
        .unwrap_or_else(|| panic!("{tool} not recorded"));
    assert!(!record.ok, "{record:?}");
    assert_eq!(record.error.as_deref(), Some("tool failure"));
    assert_eq!(record.io_body, None, "the failure's content is not stored");
}

/// A host tool answers content blocks, a tool failure or a structured failure
/// (`isError`, still a JSON-RPC result), a failure is recorded as not ok
/// without its content, and no request reads the host catalog twice, the
/// recorder included.
#[tokio::test]
async fn host_content_and_failure_replies_are_served_and_recorded_with_one_catalog_read()
-> TestResult {
    let db = SplitRoleDb::create("proxima_host_replies", &[]).await?;
    let subject = UserId::new(Uuid::now_v7());
    let lists = Arc::new(AtomicUsize::new(0));
    let running = Proxima::<HostApp>::app()
        .database_url(db.runtime_url())
        .platform_database_url(db.platform_url())
        .mcp_bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .tool_scope(ToolScope::All)
        .host_tools(Arc::new(ReplyTools {
            lists: Arc::clone(&lists),
        }))
        .authenticator(Arc::new(StubAuth { subject }))
        .record_mcp_calls(true)
        .run()
        .await?;
    let base = format!("http://{}", running.mcp_addr().ok_or("no MCP address")?);
    let client = reqwest::Client::new();
    let session = open_session(&client, &base, subject).await?;
    let call = |id: u64, name: &'static str| {
        rpc(
            &client,
            &base,
            subject,
            &session,
            json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
                   "params": {"name": name, "arguments": {}}}),
        )
    };

    let before = lists.load(Ordering::SeqCst);
    let listed = rpc(
        &client,
        &base,
        subject,
        &session,
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
    )
    .await?;
    assert_eq!(
        lists.load(Ordering::SeqCst) - before,
        1,
        "one list per tools/list"
    );
    for name in [FAILING_TOOL, STRUCTURED_FAILING_TOOL, CONTENT_TOOL] {
        let tool = listed["result"]["tools"]
            .as_array()
            .and_then(|tools| tools.iter().find(|tool| tool["name"] == name))
            .ok_or("host tool not listed")?;
        assert!(tool.get("outputSchema").is_none(), "{tool}");
    }

    let failed = call(3, FAILING_TOOL).await?;
    assert_failure_result(&failed, "upstream said no", None);

    let content = call(4, CONTENT_TOOL).await?;
    assert_ne!(content["result"]["isError"], json!(true), "{content}");
    assert_eq!(
        content["result"]["content"][0]["text"],
        json!("hello"),
        "{content}"
    );
    assert_eq!(
        content["result"]["content"][1],
        json!({"type": "image", "data": "+/8=", "mimeType": "image/png"}),
        "{content}"
    );
    assert!(
        content["result"].get("structuredContent").is_none(),
        "{content}"
    );

    let structured_failed = call(5, STRUCTURED_FAILING_TOOL).await?;
    assert_failure_result(
        &structured_failed,
        "quota exhausted; retry in 60s",
        Some(json!({"code": "quota_exhausted", "retry_after_s": 60})),
    );

    let recorded = recorded_calls(running.host().engine(), subject, 3).await?;
    assert_recorded_failure(&recorded, FAILING_TOOL);
    assert_recorded_failure(&recorded, STRUCTURED_FAILING_TOOL);
    let served = recorded
        .iter()
        .find(|call| call.tool_name == CONTENT_TOOL)
        .ok_or("content call not recorded")?;
    assert!(served.ok, "{served:?}");
    assert_eq!(served.io_body, None, "no body is stored");
    // The tools/list and the three calls read the catalog once each; the
    // recorder, finished by now, read it zero times.
    assert_eq!(lists.load(Ordering::SeqCst) - before, 4);

    running.shutdown().await;
    Ok(())
}

/// `resources/read` of `uri`: the JSON the single content block carries.
async fn read_resource(
    client: &reqwest::Client,
    base: &str,
    subject: UserId,
    session: &str,
    uri: &str,
) -> TestResult<Value> {
    let reply = rpc(
        client,
        base,
        subject,
        session,
        json!({"jsonrpc": "2.0", "id": 9, "method": "resources/read", "params": {"uri": uri}}),
    )
    .await?;
    let content = &reply["result"]["contents"][0];
    assert_eq!(content["uri"], json!(uri), "{reply}");
    assert_eq!(content["mimeType"], json!("application/json"), "{reply}");
    let text = content["text"].as_str().ok_or("no text content")?;
    Ok(serde_json::from_str(text)?)
}

/// `proxima://memories/counts` is listed, answers all four kinds as one JSON
/// object over `/mcp`, and a resource read leaves no `mcp_call_logged_v1`
/// row, while a recorded tool call does (so the silence is the resource's,
/// not recording being off).
#[tokio::test]
async fn the_memory_counts_resource_is_read_over_mcp_and_leaves_no_call_record() -> TestResult {
    const URI: &str = "proxima://memories/counts";
    let db = SplitRoleDb::create("proxima_memory_counts", &[]).await?;
    let subject = UserId::new(Uuid::now_v7());
    let owner = OwnerRef::Personal(subject);
    let running = app(&db)
        .authenticator(Arc::new(StubAuth { subject }))
        .record_mcp_calls(true)
        .run()
        .await?;
    let engine = running.host().engine();
    let base = format!("http://{}", running.mcp_addr().ok_or("no MCP address")?);
    let client = reqwest::Client::new();
    let session = open_session(&client, &base, subject).await?;

    let listed = rpc(
        &client,
        &base,
        subject,
        &session,
        json!({"jsonrpc": "2.0", "id": 2, "method": "resources/list", "params": {}}),
    )
    .await?;
    let resources = listed["result"]["resources"]
        .as_array()
        .ok_or("no resources")?;
    assert!(
        resources.iter().any(|resource| resource["uri"] == URI),
        "{listed}"
    );

    let empty = read_resource(&client, &base, subject, &session, URI).await?;
    assert_eq!(
        empty,
        json!({"counts": {"Fact": 0, "Abstraction": 0, "Perspective": 0, "Goal": 0}}),
        "a caller with no memories gets every kind, at zero"
    );

    let writer = proxima_core::test_fixtures::authenticated_context(AuthzContext::for_subject(
        subject,
        AuthPath::HostBearer,
    ));
    engine
        .ingest_fact(
            &writer,
            proxima::FactWrite::new(
                owner,
                "test/memory-counts",
                &proxima_core::AgentNoteV1 {
                    note_id: Uuid::now_v7(),
                    title: "one current fact".to_owned(),
                    body: "counted once".to_owned(),
                    tags: Vec::new(),
                    idempotency_key: None,
                },
            ),
        )
        .await?;
    let one = read_resource(&client, &base, subject, &session, URI).await?;
    assert_eq!(
        one,
        json!({"counts": {"Fact": 1, "Abstraction": 0, "Perspective": 0, "Goal": 0}})
    );

    // Recording runs off the request path: a late row would land by now.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        recorded_calls(engine, subject, 0).await?.is_empty(),
        "reading a resource wrote a call record"
    );

    // A recorded tool call lands a row, and its ledger Fact is a current Fact.
    let called = rpc(
        &client,
        &base,
        subject,
        &session,
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
               "params": {"name": HOST_TOOL, "arguments": {}}}),
    )
    .await?;
    assert!(called["result"].is_object(), "{called}");
    assert_eq!(recorded_calls(engine, subject, 1).await?.len(), 1);
    let after = read_resource(&client, &base, subject, &session, URI).await?;
    assert_eq!(after["counts"]["Fact"], json!(2), "{after}");
    assert_eq!(
        after["counts"].as_object().map(serde_json::Map::len),
        Some(4)
    );

    running.shutdown().await;
    Ok(())
}

const SLEEP_TOOL: &str = "host_sleep";

/// Sleeps `ms`, then answers — a tool call that sends nothing while it runs.
#[derive(Debug)]
struct SleepTools;

#[async_trait]
impl McpHostTools for SleepTools {
    async fn list(
        &self,
        _auth: &proxima::McpAuthContext,
    ) -> Result<Vec<McpHostTool>, McpToolError> {
        Ok(vec![
            McpHostTool::new(
                SLEEP_TOOL,
                "Sleep, then answer.",
                json!({"type": "object"}),
                ToolEffect::ReadOnly,
            )
            .with_output_schema(json!({"type": "object"})),
        ])
    }

    async fn call(&self, call: ToolCall) -> Result<ToolReply, McpToolError> {
        let ms = call.args["ms"].as_u64().unwrap_or(0);
        tokio::time::sleep(Duration::from_millis(ms)).await;
        Ok(ToolReply::Structured(json!({"slept_ms": ms})))
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
    let base = format!("http://{}", running.mcp_addr().ok_or("no MCP address")?);

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
    let base = format!("http://{}", running.mcp_addr().ok_or("no MCP address")?);
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

//! Compare the actual MCP handler over HTTP with the real REST router.
//! The registered fixture tool has no storage or external provider effects.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::http::{Method, StatusCode, header};
use proxima_core::mcp::{McpTool, McpToolAnnotations, McpToolCtx, McpToolError};
use proxima_core::{AuthError, AuthPath, Authenticator, AuthzContext, Credentials};
use proxima_mcp_server::{McpEdgeAuth, McpToolHost, default_allowlist, serve_streamable_http};
use serde_json::{Value, json};

use super::{Answer, FlavorRegistry, FlavorServices, McpAuthContext, app, call};

#[path = "../common/mod.rs"]
mod common;

const TOOL: &str = "proxima-nul_echo";
static INVOCATIONS: AtomicUsize = AtomicUsize::new(0);
type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct EchoArgs {
    /// Any JSON value, so nested argument validation remains the shared seam's job.
    payload: Value,
}

struct EchoTool;

impl McpTool for EchoTool {
    const NAME: &'static str = TOOL;
    const DESCRIPTION: &'static str = "Return the fixture argument without storage access.";
    const ANNOTATIONS: Option<McpToolAnnotations> =
        Some(McpToolAnnotations::new().read_only(true).open_world(false));
    type Args = EchoArgs;
    type Output = Value;

    fn call(
        _ctx: McpToolCtx,
        args: EchoArgs,
    ) -> futures_util::future::BoxFuture<'static, Result<Value, McpToolError>> {
        Box::pin(async move {
            INVOCATIONS.fetch_add(1, Ordering::SeqCst);
            Ok(args.payload)
        })
    }
}

#[derive(Debug)]
struct FixtureAuth(AuthzContext);

#[async_trait::async_trait]
impl Authenticator for FixtureAuth {
    async fn authenticate(&self, credentials: &Credentials) -> Result<AuthzContext, AuthError> {
        match credentials {
            Credentials::Bearer(token) if token == "host-token" => Ok(self.0.clone()),
            Credentials::Bearer(_) => Err(AuthError::InvalidCredentials),
        }
    }
}

struct ServerGuard(Option<common::ServeHandle>);

impl Drop for ServerGuard {
    fn drop(&mut self) {
        if let Some(task) = &self.0 {
            task.abort();
        }
    }
}

impl ServerGuard {
    async fn stop(mut self) {
        let task = self.0.take().expect("server task");
        task.abort();
        let _ = task.await;
    }
}

struct Observation {
    name: &'static str,
    invalid: bool,
    payload: Value,
    mcp: Value,
    mcp_calls: usize,
    rest: Vec<(Method, Answer, usize)>,
}

async fn observe_transports() -> TestResult<Vec<Observation>> {
    let mut registry = FlavorRegistry::new();
    registry.add_mcp_tool_or_panic_for_tests::<EchoTool>("proxima-nul");
    let host = McpToolHost::from_parts(
        Arc::new(registry.freeze_or_panic_for_tests()),
        FlavorServices::default(),
    );
    let owner = common::nil_owner();
    let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
    let ctx = McpAuthContext {
        owner,
        authz: authz.clone(),
    };
    let router = app(host.clone());
    let auth = McpEdgeAuth::headless().with_host(Arc::new(FixtureAuth(authz)));
    let (task, address) = serve_streamable_http(
        "127.0.0.1:0".parse()?,
        host,
        default_allowlist(),
        Arc::new(auth),
    )
    .await?;
    let server = ServerGuard(Some(task));
    let result = async {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()?;
        let url = format!("http://{address}/mcp");
        let session = common::initialize(&client, &url, "Bearer host-token").await?;
        common::initialized(&client, &url, &session, "Bearer host-token").await?;
        let mut observations = Vec::new();
        for (index, (name, invalid, payload)) in [
            ("string", true, json!("before\0after")),
            (
                "array",
                true,
                json!(["ordinary", {"nested": ["bad\0value"]}]),
            ),
            (
                "object key",
                true,
                json!({"nested": {"bad\0key": "ordinary"}}),
            ),
            ("ordinary string", false, json!("ordinary é text \\u0000")),
            (
                "ordinary values",
                false,
                json!([true, false, 1, 1.5, null, {"nested": ["ok"]}]),
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let args = json!({"payload": payload});
            let before = INVOCATIONS.load(Ordering::SeqCst);
            let mcp = common::post_rpc(
                &client,
                &url,
                Some(&session),
                "Bearer host-token",
                json!({
                    "jsonrpc": "2.0", "id": index + 2, "method": "tools/call",
                    "params": {"name": TOOL, "arguments": args}
                }),
            )
            .await?;
            let mcp_calls = INVOCATIONS.load(Ordering::SeqCst) - before;
            let mut rest = Vec::new();
            for method in [Method::POST, Method::QUERY] {
                let before = INVOCATIONS.load(Ordering::SeqCst);
                let answer = call(
                    &router,
                    method.clone(),
                    &format!("/v1/tools/{TOOL}"),
                    &ctx,
                    Some(args.clone()),
                )
                .await;
                rest.push((method, answer, INVOCATIONS.load(Ordering::SeqCst) - before));
            }
            observations.push(Observation {
                name,
                invalid,
                payload,
                mcp,
                mcp_calls,
                rest,
            });
        }
        Ok(observations)
    }
    .await;
    server.stop().await;
    result
}

#[tokio::test]
async fn rest_and_mcp_reject_nul_arguments_before_tool_invocation() {
    let observations = tokio::time::timeout(Duration::from_secs(30), observe_transports())
        .await
        .expect("bounded transport fixture")
        .expect("actual MCP handshake and REST requests complete");
    // All observations and listener cleanup precede the expected RED assertion.
    for observed in &observations {
        let rest: Vec<_> = observed
            .rest
            .iter()
            .map(|(method, answer, calls)| (method.as_str(), answer.status.as_u16(), calls))
            .collect();
        eprintln!(
            "NUL parity {}: mcp={} mcp_calls={} rest={rest:?}",
            observed.name, observed.mcp, observed.mcp_calls
        );
        if observed.invalid {
            assert_eq!(
                observed.mcp["error"]["code"], -32602,
                "{} MCP class",
                observed.name
            );
            assert!(
                observed.mcp["error"]["message"]
                    .as_str()
                    .is_some_and(|message| message.contains("NUL (U+0000)"))
            );
            assert_eq!(observed.mcp_calls, 0, "MCP rejects before invocation");
        } else {
            assert!(observed.mcp.get("error").is_none(), "normal MCP input");
            assert_eq!(
                observed.mcp["result"]["structuredContent"],
                observed.payload
            );
            assert_eq!(observed.mcp_calls, 1);
            for (_, answer, calls) in &observed.rest {
                assert_eq!(answer.status, StatusCode::OK);
                assert_eq!(answer.json(), observed.payload);
                assert_eq!(*calls, 1);
            }
        }
    }
    for observed in observations.iter().filter(|observed| observed.invalid) {
        for (method, answer, calls) in &observed.rest {
            assert_eq!(
                answer.status,
                StatusCode::BAD_REQUEST,
                "{} {method}",
                observed.name
            );
            assert_eq!(
                answer.header(header::CONTENT_TYPE),
                Some("application/problem+json")
            );
            assert_eq!(
                answer.header(header::CACHE_CONTROL),
                Some("private, no-store")
            );
            assert!(
                answer.json()["detail"]
                    .as_str()
                    .is_some_and(|detail| detail.contains("NUL (U+0000)"))
            );
            assert_eq!(*calls, 0, "REST rejects before invocation");
        }
    }
}

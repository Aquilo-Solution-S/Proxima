//! rmcp 3.x entrypoint:
//! `rmcp::transport::streamable_http_server::StreamableHttpService`.
//!
//! The service is a Tower service composed into
//! `axum::Router::nest_service("/mcp", service)`. Proxima wraps it
//! with shared listener-level Host and Origin validation. rmcp retains its
//! own `/mcp` DNS-rebinding guard as defense in depth.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use http_body_util::Limited;
use proxima_core::RevalidationConfig;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService,
    session::local::{LocalSessionManager, SessionConfig},
};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::McpServerError;
use crate::auth::McpEdgeAuth;
use crate::handler::DynamicHandler;
use crate::security::{
    HostAllowlist, OriginAllowlist, assert_loopback, cors_layer, host_guard_layer,
    mcp_auth_layer_with_config,
};
use crate::server::McpToolHost;

/// The rmcp service `/mcp` is served by ([`streamable_http_service`]).
pub type McpStreamableService = StreamableHttpService<DynamicHandler, LocalSessionManager>;

/// `host_allowlist` is the same non-empty policy applied at the outer
/// listener. Passing it into rmcp keeps `/mcp` independently guarded as
/// defense in depth; [`HostAllowlist`] owns the loopback defaults so this
/// function has no empty-list special case.
#[must_use]
pub fn streamable_http_service(
    server: McpToolHost,
    allowlist: &OriginAllowlist,
    host_allowlist: &HostAllowlist,
    cancel: &CancellationToken,
) -> McpStreamableService {
    streamable_http_service_with_transport(
        server,
        allowlist,
        host_allowlist,
        cancel,
        &McpTransportConfig::default(),
    )
}

/// [`streamable_http_service`] with explicit rmcp transport settings.
///
/// The security-bearing fields — Host and Origin allowlists, cancellation —
/// always come from Proxima's own policy; `transport` only tunes the stream.
#[must_use]
pub fn streamable_http_service_with_transport(
    server: McpToolHost,
    allowlist: &OriginAllowlist,
    host_allowlist: &HostAllowlist,
    cancel: &CancellationToken,
    transport: &McpTransportConfig,
) -> McpStreamableService {
    let config = StreamableHttpServerConfig::default()
        .with_allowed_origins(allowlist.origins())
        .with_allowed_hosts(host_allowlist.hosts().iter().cloned())
        .with_cancellation_token(cancel.child_token())
        .with_sse_keep_alive(transport.sse_keep_alive)
        .with_sse_retry(transport.sse_retry)
        .with_legacy_session_mode(transport.legacy_session_mode)
        .with_json_response(transport.json_response)
        .with_max_request_body_bytes(transport.max_request_body_bytes);
    let mut sessions = LocalSessionManager::default();
    sessions.session_config.keep_alive = transport.session_idle_timeout;
    let progress_heartbeat = transport.progress_heartbeat();
    StreamableHttpService::new(
        move || Ok(DynamicHandler::new(server.clone()).with_progress_heartbeat(progress_heartbeat)),
        Arc::new(sessions),
        config,
    )
}

/// # Errors
///
/// Returns loopback validation, TCP bind, or HTTP server failures.
///
/// `auth` is required so each MCP request can be matched against the
/// host bearer path; unrecognized token prefixes fail closed before host auth.
pub async fn serve_streamable_http(
    addr: SocketAddr,
    server: McpToolHost,
    allowlist: OriginAllowlist,
    auth: Arc<McpEdgeAuth>,
) -> Result<(JoinHandle<Result<(), McpServerError>>, SocketAddr), McpServerError> {
    serve_streamable_http_with_revalidation(
        addr,
        server,
        allowlist,
        auth,
        RevalidationConfig::default(),
    )
    .await
}

/// # Errors
///
/// Returns loopback validation, TCP bind, or HTTP server failures.
pub async fn serve_streamable_http_with_revalidation(
    addr: SocketAddr,
    server: McpToolHost,
    allowlist: OriginAllowlist,
    auth: Arc<McpEdgeAuth>,
    revalidation: RevalidationConfig,
) -> Result<(JoinHandle<Result<(), McpServerError>>, SocketAddr), McpServerError> {
    assert_loopback(&addr)?;

    let cancellation_token = CancellationToken::new();
    // These helpers bind loopback only (asserted above), so the shared policy
    // contains exactly the three loopback authorities.
    let host_allowlist = HostAllowlist::default();
    let service = streamable_http_service(server, &allowlist, &host_allowlist, &cancellation_token);
    // Layer order is bottom-up (the last `.layer` is outermost): the
    // body-size guard runs first and 413s oversized requests before auth
    // or JSON parsing, then the shared Host guard, listener-wide CORS/Origin
    // guard, auth, and finally rmcp's own Host/Origin guard.
    // Native CLI clients commonly omit Origin and keep the bearer path.
    let app = axum::Router::new()
        .nest_service(crate::oauth::MCP_PATH, service)
        .layer(mcp_auth_layer_with_config(auth, revalidation))
        .layer(cors_layer(allowlist))
        .layer(host_guard_layer(host_allowlist))
        .layer(middleware::from_fn(enforce_body_limit));

    let listener = tokio::net::TcpListener::bind(addr).await?;
    let bound_addr = listener.local_addr()?;
    tracing::info!(addr = %bound_addr, "mcp listening");

    let handle = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                cancellation_token.cancelled_owned().await;
            })
            .await
            .map_err(|err| McpServerError::Axum(err.to_string()))
    });

    Ok((handle, bound_addr))
}

/// Default cap on the accepted request-body size. Sits generously above the
/// largest legitimate MCP request; anything larger is a client error or
/// abuse and is refused before auth or JSON parsing runs.
pub const MAX_REQUEST_BODY_BYTES: usize = 4 * 1024 * 1024;

/// rmcp Streamable HTTP tuning a host may set (`PROXIMA_MCP_*`).
///
/// Deliberately not rmcp's own config type: its Host/Origin allowlists and
/// cancellation are security policy Proxima derives itself, so a host
/// cannot hand in a config that widens them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct McpTransportConfig {
    /// SSE ping interval; `None` disables pings.
    pub sse_keep_alive: Option<Duration>,
    /// Close a session after this long without session traffic; `None`
    /// never closes it (rmcp's `SessionConfig::keep_alive`). SSE pings are
    /// not traffic and neither is a tool call still running: the progress
    /// heartbeat is, for a call that carries a progress token.
    pub session_idle_timeout: Option<Duration>,
    /// SSE priming-event retry interval; `None` sends none.
    pub sse_retry: Option<Duration>,
    /// Keep a server-side session per client that opens with `initialize`;
    /// per-request (`_meta`-versioned) calls are always stateless.
    pub legacy_session_mode: bool,
    /// Prefer `application/json` for simple request/response calls when
    /// sessions are off.
    pub json_response: bool,
    /// Largest accepted request body, in bytes. Enforced by the listener
    /// guard ([`body_limit_layer`]) and by rmcp; the facade feeds both from
    /// one value.
    pub max_request_body_bytes: usize,
}

impl Default for McpTransportConfig {
    /// rmcp's defaults, with Proxima's body cap.
    fn default() -> Self {
        Self {
            sse_keep_alive: Some(Duration::from_secs(15)),
            session_idle_timeout: Some(SessionConfig::DEFAULT_KEEP_ALIVE),
            sse_retry: Some(Duration::from_secs(3)),
            legacy_session_mode: true,
            json_response: false,
            max_request_body_bytes: MAX_REQUEST_BODY_BYTES,
        }
    }
}

impl McpTransportConfig {
    /// How often a running tool call that carries a progress token sends
    /// `notifications/progress`: [`DEFAULT_PROGRESS_HEARTBEAT`], or half the
    /// session idle timeout when that is shorter, so the session never sits
    /// idle for a whole timeout while the call runs.
    #[must_use]
    pub fn progress_heartbeat(&self) -> Duration {
        self.session_idle_timeout
            .map_or(DEFAULT_PROGRESS_HEARTBEAT, |idle| {
                DEFAULT_PROGRESS_HEARTBEAT.min(idle / 2)
            })
            .max(MIN_PROGRESS_HEARTBEAT)
    }
}

/// Progress heartbeat interval when the idle timeout allows it.
pub const DEFAULT_PROGRESS_HEARTBEAT: Duration = Duration::from_secs(30);

/// Floor for the heartbeat, so a tiny idle timeout cannot spin it.
const MIN_PROGRESS_HEARTBEAT: Duration = Duration::from_millis(100);

/// Outermost MCP guard: reject oversized bodies with 413 before auth or parsing.
/// A declared `Content-Length` over the cap is refused immediately; the body
/// is otherwise wrapped in [`Limited`] so a chunked or length-lying stream
/// errors past the cap instead of buffering unbounded memory.
///
/// Uses [`MAX_REQUEST_BODY_BYTES`]; [`body_limit_layer`] takes the cap as a
/// value.
pub async fn enforce_body_limit(request: Request<Body>, next: Next) -> Response {
    limit_body(MAX_REQUEST_BODY_BYTES, request, next).await
}

/// Concrete return type for [`body_limit_layer`].
pub type BodyLimitLayer = middleware::FromFnLayer<
    fn(
        State<usize>,
        Request<Body>,
        Next,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Response> + Send>>,
    usize,
    (State<usize>, Request<Body>),
>;

/// [`enforce_body_limit`] with a configured cap, in bytes.
#[must_use = "apply the returned layer to the complete HTTP listener"]
pub fn body_limit_layer(max_bytes: usize) -> BodyLimitLayer {
    fn dispatch(
        State(max_bytes): State<usize>,
        request: Request<Body>,
        next: Next,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Response> + Send>> {
        Box::pin(limit_body(max_bytes, request, next))
    }
    middleware::from_fn_with_state(max_bytes, dispatch as fn(_, _, _) -> _)
}

async fn limit_body(max_bytes: usize, request: Request<Body>, next: Next) -> Response {
    if let Some(len) = request
        .headers()
        .get(http::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|raw| raw.parse::<usize>().ok())
        && len > max_bytes
    {
        return payload_too_large();
    }
    let (parts, body) = request.into_parts();
    let limited = Body::new(Limited::new(body, max_bytes));
    next.run(Request::from_parts(parts, limited)).await
}

fn payload_too_large() -> Response {
    (StatusCode::PAYLOAD_TOO_LARGE, "request body too large").into_response()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::Router;
    use axum::body::{Body, Bytes};
    use axum::http::{Request, StatusCode};
    use axum::routing::any;
    use proxima_core::RevalidationConfig;
    use tower::ServiceExt;

    use super::{MAX_REQUEST_BODY_BYTES, enforce_body_limit};
    use crate::auth::McpEdgeAuth;
    use crate::security::{cors_layer, default_allowlist, mcp_auth_layer_with_config};

    /// Relevant production stack order: body-limit outermost, then CORS and
    /// auth over an OK `/mcp` stub. Auth is headless (rejects every bearer),
    /// so any request that reaches auth returns 401 — letting us prove the
    /// body guard runs first.
    fn guarded_app() -> Router {
        let allowlist = default_allowlist();
        Router::new()
            .route("/mcp", any(|| async { StatusCode::OK }))
            .layer(mcp_auth_layer_with_config(
                Arc::new(McpEdgeAuth::headless()),
                RevalidationConfig::default(),
            ))
            .layer(cors_layer(allowlist))
            .layer(axum::middleware::from_fn(enforce_body_limit))
    }

    // An over-cap declared Content-Length is 413'd before auth.
    #[tokio::test]
    async fn oversized_content_length_is_rejected_before_auth() {
        let app = guarded_app();
        // No Authorization header: if auth ran first this would be 401.
        let request = Request::builder()
            .uri("/mcp")
            .header("Content-Length", (MAX_REQUEST_BODY_BYTES + 1).to_string())
            .body(Body::from("x"))
            .unwrap();
        let status = app.oneshot(request).await.unwrap().status();
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    }

    // A configured cap replaces the default one.
    #[tokio::test]
    async fn configured_body_limit_is_enforced() {
        let app = Router::new()
            .route("/mcp", any(|| async { StatusCode::OK }))
            .layer(super::body_limit_layer(16));
        let over = Request::builder()
            .uri("/mcp")
            .header("Content-Length", "17")
            .body(Body::from("x"))
            .unwrap();
        assert_eq!(
            app.clone().oneshot(over).await.unwrap().status(),
            StatusCode::PAYLOAD_TOO_LARGE
        );
        let within = Request::builder()
            .uri("/mcp")
            .body(Body::from(vec![b'x'; 16]))
            .unwrap();
        assert_eq!(app.oneshot(within).await.unwrap().status(), StatusCode::OK);
    }

    // A small body flows through the guard and is then handled by auth.
    #[tokio::test]
    async fn in_limit_request_without_bearer_reaches_auth() {
        let app = guarded_app();
        let request = Request::builder()
            .uri("/mcp")
            .header("Origin", "http://localhost")
            .body(Body::from("{}"))
            .unwrap();
        let status = app.oneshot(request).await.unwrap().status();
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    /// The production `/mcp` service (rmcp defaults: server-side sessions
    /// for `initialize` clients, SSE answers) over `host`, with no listener
    /// layers: the tests below drive rmcp's lifecycles against
    /// [`crate::handler::DynamicHandler`], nothing in front of it.
    fn mcp_service_for(host: crate::server::McpToolHost) -> super::McpStreamableService {
        super::streamable_http_service(
            host,
            &default_allowlist(),
            &crate::security::HostAllowlist::default(),
            &tokio_util::sync::CancellationToken::new(),
        )
    }

    /// A host serving the core tools plus [`StubTool`].
    fn stub_host() -> crate::server::McpToolHost {
        let mut registry = proxima_core::FlavorRegistry::new();
        registry.add_mcp_tool_or_panic_for_tests::<StubTool>("proxima-stub");
        crate::server::McpToolHost::from_parts(
            Arc::new(registry.freeze_or_panic_for_tests()),
            proxima_core::FlavorServices::default(),
        )
    }

    fn mcp_service() -> super::McpStreamableService {
        mcp_service_for(stub_host())
    }

    /// What `mcp_auth_layer` binds for a verified personal owner.
    fn auth_for(owner: proxima_core::Owner) -> crate::McpAuthContext {
        crate::McpAuthContext {
            owner,
            authz: proxima_core::AuthzContext::single_owner(
                &owner,
                proxima_core::AuthPath::HostBearer,
            )
            .with_tool_scope(proxima_core::ToolScope::All),
        }
    }

    fn personal_owner() -> proxima_core::Owner {
        proxima_core::OwnerRef::Personal(proxima_core::UserId::new(uuid::Uuid::now_v7()))
    }

    fn rpc_request(version: &str, body: &serde_json::Value) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri("http://localhost/mcp")
            .header("Host", "localhost")
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .header("MCP-Protocol-Version", version)
            .header("Mcp-Method", body["method"].as_str().unwrap())
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    /// A request in the per-request lifecycle (no `initialize`, no
    /// session), naming `version` and the client in its `_meta` the way a
    /// `2026-07-28` client does.
    fn per_request_rpc(version: &str, method: &str, params: serde_json::Value) -> Request<Body> {
        let mut params = params;
        params["_meta"] = serde_json::json!({
            "io.modelcontextprotocol/protocolVersion": version,
            "io.modelcontextprotocol/clientCapabilities": {},
            "io.modelcontextprotocol/clientInfo": { "name": "claude-code", "version": "2.1.0" },
        });
        rpc_request(
            version,
            &serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params }),
        )
    }

    /// A flat read-only tool that answers with the client name its author
    /// context recorded.
    #[derive(Debug)]
    struct StubTool;

    #[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
    struct StubArgs {}

    impl proxima_core::mcp::McpTool for StubTool {
        const NAME: &'static str = "proxima-stub_ping";
        const DESCRIPTION: &'static str = "Answers with the calling client's name.";
        const ANNOTATIONS: Option<proxima_core::mcp::McpToolAnnotations> = Some(
            proxima_core::mcp::McpToolAnnotations::new()
                .read_only(true)
                .open_world(false),
        );
        type Args = StubArgs;
        type Output = String;
        fn call(
            ctx: proxima_core::mcp::McpToolCtx,
            _: Self::Args,
        ) -> futures_util::future::BoxFuture<'static, Result<String, proxima_core::mcp::McpToolError>>
        {
            Box::pin(async move { Ok(ctx.author.client_name) })
        }
    }

    /// The JSON-RPC messages of one response, read as they arrive, whether
    /// rmcp answered with plain JSON or an SSE stream that stays open.
    struct Messages {
        body: Body,
        text: String,
        seen: usize,
    }

    impl Messages {
        async fn of(service: super::McpStreamableService, request: Request<Body>) -> Self {
            let response = service.oneshot(request).await.unwrap();
            Self::from_response(response)
        }

        fn from_response<B>(response: http::Response<B>) -> Self
        where
            B: http_body::Body<Data = Bytes> + Send + 'static,
            B::Error: Into<axum::BoxError>,
        {
            Self {
                body: Body::new(response.into_body()),
                text: String::new(),
                seen: 0,
            }
        }

        /// The next message within `wait`, or `None` when none arrives.
        async fn next_within(&mut self, wait: std::time::Duration) -> Option<serde_json::Value> {
            use http_body_util::BodyExt;

            let read = async {
                loop {
                    let parsed: Vec<serde_json::Value> = self
                        .text
                        .lines()
                        .filter_map(|line| line.strip_prefix("data:"))
                        .filter_map(|data| serde_json::from_str(data.trim()).ok())
                        .collect();
                    if let Some(message) = parsed.get(self.seen) {
                        self.seen += 1;
                        return Some(message.clone());
                    }
                    if self.seen == 0
                        && let Ok(message) = serde_json::from_str::<serde_json::Value>(&self.text)
                    {
                        self.seen = 1;
                        return Some(message);
                    }
                    let frame = self.body.frame().await?.ok()?;
                    if let Ok(data) = frame.into_data() {
                        self.text.push_str(std::str::from_utf8(&data).unwrap());
                    }
                }
            };
            tokio::time::timeout(wait, read).await.ok().flatten()
        }

        async fn next(&mut self) -> serde_json::Value {
            self.next_within(std::time::Duration::from_secs(5))
                .await
                .unwrap_or_else(|| panic!("no JSON-RPC message within 5s: {:?}", self.text))
        }
    }

    async fn first_message(
        service: super::McpStreamableService,
        request: Request<Body>,
    ) -> serde_json::Value {
        Messages::of(service, request).await.next().await
    }

    /// `2026-07-28` has no handshake, so an `initialize` naming it settles
    /// on the newest revision that has one.
    #[tokio::test]
    async fn initialize_requesting_2026_07_28_is_answered_with_2025_11_25() {
        let request = rpc_request(
            "2026-07-28",
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2026-07-28",
                    "capabilities": {},
                    "clientInfo": { "name": "test", "version": "0" },
                },
            }),
        );
        let message = first_message(mcp_service(), request).await;
        assert_eq!(
            message["result"]["protocolVersion"], "2025-11-25",
            "{message}"
        );
    }

    /// Issue AQS/aquilo#9484: Claude Code on `2026-07-28` rejected
    /// `tools/list` for missing SEP-2549 cache hints. Every list carries
    /// them, on that revision and on the ones before it; each list is
    /// projected from the caller's token, so it is private and never fresh.
    #[tokio::test]
    async fn list_results_carry_private_zero_ttl_cache_hints() {
        for version in ["2026-07-28", "2025-11-25"] {
            for method in ["tools/list", "resources/list", "resources/templates/list"] {
                let request = per_request_rpc(version, method, serde_json::json!({}));
                let message = first_message(mcp_service(), request).await;
                let context = format!("{version} {method}: {message}");
                assert_eq!(message["result"]["ttlMs"], 0, "{context}");
                assert_eq!(message["result"]["cacheScope"], "private", "{context}");
            }
        }
    }

    /// A revision past the ceiling is refused with the ones served, so the
    /// client can fall back, never answered in a shape it cannot trust.
    #[tokio::test]
    async fn a_revision_newer_than_the_ceiling_is_refused() {
        let request = per_request_rpc("2027-01-01", "tools/list", serde_json::json!({}));
        let message = first_message(mcp_service(), request).await;
        assert_eq!(
            message["error"]["code"],
            rmcp::model::ErrorCode::UNSUPPORTED_PROTOCOL_VERSION.0,
            "{message}"
        );
        assert_eq!(
            message["error"]["data"]["supported"],
            serde_json::json!([
                "2024-11-05",
                "2025-03-26",
                "2025-06-18",
                "2025-11-25",
                "2026-07-28"
            ]),
            "{message}"
        );
    }

    /// `server/discover` replaces the handshake on `2026-07-28`: it lists
    /// the revisions served and carries the same per-caller instructions
    /// `initialize` does.
    #[tokio::test]
    async fn discover_serves_2026_07_28_with_instructions() {
        let request = per_request_rpc("2026-07-28", "server/discover", serde_json::json!({}));
        let message = first_message(mcp_service(), request).await;
        assert_eq!(
            message["result"]["supportedVersions"],
            serde_json::json!([
                "2024-11-05",
                "2025-03-26",
                "2025-06-18",
                "2025-11-25",
                "2026-07-28"
            ]),
            "{message}"
        );
        assert!(
            message["result"]["instructions"]
                .as_str()
                .is_some_and(|text| !text.is_empty()),
            "{message}"
        );
        assert_eq!(message["result"]["cacheScope"], "private", "{message}");
    }

    /// The call Centauri's direct forwarder sends a pack (aquilo
    /// `serve_forward/direct/mcp_wire.rs`, contract `serve-mcp-forward-v1`
    /// `mcpTransport`): one standalone `tools/call`, no `initialize`,
    /// `MCP-Protocol-Version: 2026-07-28`, SEP-2243 headers and the two
    /// required `_meta` keys. rmcp checks that `_meta` version against the
    /// handler's supported revisions, so a ceiling below `2026-07-28` would
    /// refuse every such call with -32022.
    #[tokio::test]
    async fn standalone_2026_07_28_tools_call_is_answered() {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": "attempt-1",
            "method": "tools/call",
            "params": {
                "name": "proxima-stub_ping",
                "arguments": {},
                "_meta": {
                    "io.modelcontextprotocol/clientCapabilities": {},
                    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                },
            },
        });
        let mut request = rpc_request("2026-07-28", &body);
        request
            .headers_mut()
            .insert("Mcp-Name", "proxima-stub_ping".parse().unwrap());
        request.extensions_mut().insert(auth_for(personal_owner()));
        let message = first_message(mcp_service(), request).await;
        assert_eq!(message["id"], "attempt-1", "{message}");
        assert_eq!(message["result"]["isError"], false, "{message}");
        // The forwarder names no client; nothing is invented for it.
        assert_eq!(
            message["result"]["structuredContent"], "unknown",
            "{message}"
        );
    }

    /// A per-request caller has no handshake to take its name from; the
    /// author context reads it from the request's own `_meta`.
    #[tokio::test]
    async fn a_per_request_callers_client_info_reaches_the_author_context() {
        let mut request = per_request_rpc(
            "2026-07-28",
            "tools/call",
            serde_json::json!({ "name": "proxima-stub_ping", "arguments": {} }),
        );
        request
            .headers_mut()
            .insert("Mcp-Name", "proxima-stub_ping".parse().unwrap());
        request.extensions_mut().insert(auth_for(personal_owner()));
        let message = first_message(mcp_service(), request).await;
        assert_eq!(
            message["result"]["structuredContent"], "claude-code",
            "{message}"
        );
    }

    /// `tools.listChanged` is a promise to notify, so it is advertised only
    /// by a host that attached a notifier.
    #[tokio::test]
    async fn list_changed_is_advertised_only_with_a_notifier() {
        let discover = || per_request_rpc("2026-07-28", "server/discover", serde_json::json!({}));
        let without = first_message(mcp_service(), discover()).await;
        assert!(
            without["result"]["capabilities"]["tools"]
                .get("listChanged")
                .is_none(),
            "{without}"
        );
        let host = stub_host().with_tool_list_notifier(crate::ToolListNotifier::new());
        let with = first_message(mcp_service_for(host), discover()).await;
        assert_eq!(
            with["result"]["capabilities"]["tools"]["listChanged"], true,
            "{with}"
        );
    }

    /// Opens an `initialize` session for `owner` and its server-to-client
    /// stream, as a `2025-11-25` client does.
    async fn open_session(
        service: &super::McpStreamableService,
        owner: proxima_core::Owner,
    ) -> Messages {
        let mut initialize = rpc_request(
            "2025-11-25",
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": { "name": "test", "version": "0" },
                },
            }),
        );
        initialize.extensions_mut().insert(auth_for(owner));
        let response = service.clone().oneshot(initialize).await.unwrap();
        let session = response.headers()["mcp-session-id"].clone();
        let answer = Messages::from_response(response).next().await;
        assert_eq!(
            answer["result"]["protocolVersion"], "2025-11-25",
            "{answer}"
        );

        let mut initialized = rpc_request(
            "2025-11-25",
            &serde_json::json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        );
        initialized
            .headers_mut()
            .insert("mcp-session-id", session.clone());
        let status = service.clone().oneshot(initialized).await.unwrap().status();
        assert_eq!(status, StatusCode::ACCEPTED);

        let stream = Request::builder()
            .method("GET")
            .uri("http://localhost/mcp")
            .header("Host", "localhost")
            .header("Accept", "text/event-stream")
            .header("MCP-Protocol-Version", "2025-11-25")
            .header("mcp-session-id", session)
            .body(Body::empty())
            .unwrap();
        Messages::from_response(service.clone().oneshot(stream).await.unwrap())
    }

    /// Opens a `2026-07-28` `subscriptions/listen` stream for `owner` and
    /// waits for its acknowledgment.
    async fn open_subscription(
        service: &super::McpStreamableService,
        owner: proxima_core::Owner,
    ) -> Messages {
        let mut listen = per_request_rpc(
            "2026-07-28",
            "subscriptions/listen",
            serde_json::json!({ "notifications": { "toolsListChanged": true } }),
        );
        listen.extensions_mut().insert(auth_for(owner));
        let mut stream = Messages::of(service.clone(), listen).await;
        let ack = stream.next().await;
        assert_eq!(
            ack["method"], "notifications/subscriptions/acknowledged",
            "{ack}"
        );
        assert_eq!(
            ack["params"]["notifications"]["toolsListChanged"], true,
            "{ack}"
        );
        stream
    }

    /// `notify` until `owner`'s listener is registered: a subscription is
    /// acknowledged a moment before `listen` registers it.
    async fn notify_registered(notifier: &crate::ToolListNotifier, owner: &proxima_core::Owner) {
        for _ in 0..100 {
            if notifier.notify(owner).await == 1 {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("no listener registered for the owner");
    }

    const QUIET: std::time::Duration = std::time::Duration::from_millis(300);

    /// A tool-list change reaches the owner's `initialize` session and no
    /// session of another owner: which tools exist is tenant information.
    #[tokio::test]
    async fn a_session_hears_only_its_owners_tool_list_change() {
        let notifier = crate::ToolListNotifier::new();
        let service = mcp_service_for(stub_host().with_tool_list_notifier(notifier.clone()));
        let (owner, other) = (personal_owner(), personal_owner());
        let mut session = open_session(&service, owner).await;
        let mut other_session = open_session(&service, other).await;

        assert_eq!(notifier.notify(&owner).await, 1);
        let heard = session.next().await;
        assert_eq!(
            heard["method"], "notifications/tools/list_changed",
            "{heard}"
        );
        assert_eq!(other_session.next_within(QUIET).await, None);
    }

    /// The `2026-07-28` path: a `subscriptions/listen` stream that asked for
    /// `toolsListChanged` hears its owner's change, tagged with its
    /// subscription, and another owner's stream hears nothing.
    #[tokio::test]
    async fn a_subscription_hears_only_its_owners_tool_list_change() {
        let notifier = crate::ToolListNotifier::new();
        let service = mcp_service_for(stub_host().with_tool_list_notifier(notifier.clone()));
        let (owner, other) = (personal_owner(), personal_owner());
        let mut stream = open_subscription(&service, owner).await;
        let mut other_stream = open_subscription(&service, other).await;

        notify_registered(&notifier, &owner).await;
        let heard = stream.next().await;
        assert_eq!(
            heard["method"], "notifications/tools/list_changed",
            "{heard}"
        );
        assert_eq!(
            heard["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"], 1,
            "{heard}"
        );
        assert_eq!(other_stream.next_within(QUIET).await, None);

        // Closing the stream ends the subscription and its registration.
        drop(stream);
        for _ in 0..100 {
            if notifier.notify(&owner).await == 0 {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("a closed subscription is still registered");
    }

    // A stream with no Content-Length that exceeds the cap errors when
    // read, rather than buffering unbounded memory.
    #[tokio::test]
    async fn oversized_streamed_body_is_truncated_with_error() {
        let app = Router::new()
            .route(
                "/mcp",
                any(|request: Request<Body>| async move {
                    match axum::body::to_bytes(request.into_body(), usize::MAX).await {
                        Ok(_) => StatusCode::OK,
                        Err(_) => StatusCode::PAYLOAD_TOO_LARGE,
                    }
                }),
            )
            .layer(axum::middleware::from_fn(enforce_body_limit));

        let oversized = vec![0u8; MAX_REQUEST_BODY_BYTES + 1];
        let stream =
            futures_util::stream::once(
                async move { Ok::<_, std::io::Error>(Bytes::from(oversized)) },
            );
        let request = Request::builder()
            .uri("/mcp")
            .body(Body::from_stream(stream))
            .unwrap();
        let status = app.oneshot(request).await.unwrap().status();
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    }
}

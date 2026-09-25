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
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
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
    StreamableHttpService::new(
        move || {
            Ok(DynamicHandler {
                server: server.clone(),
            })
        },
        Arc::default(),
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
            sse_retry: Some(Duration::from_secs(3)),
            legacy_session_mode: true,
            json_response: false,
            max_request_body_bytes: MAX_REQUEST_BODY_BYTES,
        }
    }
}

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

    /// The production `/mcp` service over an empty registry, with no listener
    /// layers: the tests below exercise rmcp's version negotiation against
    /// [`crate::handler::DynamicHandler`], nothing in front of it.
    fn mcp_service() -> super::McpStreamableService {
        let registry = Arc::new(proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests());
        super::streamable_http_service(
            crate::server::McpToolHost::from_parts(
                registry,
                proxima_core::FlavorServices::default(),
            ),
            &default_allowlist(),
            &crate::security::HostAllowlist::default(),
            &tokio_util::sync::CancellationToken::new(),
        )
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

    /// A request in the per-request lifecycle (no `initialize`, no session),
    /// naming `version` in its `_meta` the way a 2026-07-28 client does.
    fn per_request_rpc(version: &str, method: &str) -> Request<Body> {
        rpc_request(
            version,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": method,
                "params": { "_meta": {
                    "io.modelcontextprotocol/protocolVersion": version,
                    "io.modelcontextprotocol/clientCapabilities": {},
                    "io.modelcontextprotocol/clientInfo": { "name": "test", "version": "0" },
                }},
            }),
        )
    }

    /// The first JSON-RPC message of a response, whether rmcp answered with
    /// plain JSON or an SSE stream (which may stay open after it).
    async fn first_message(
        service: super::McpStreamableService,
        request: Request<Body>,
    ) -> serde_json::Value {
        use http_body_util::BodyExt;

        let response = service.oneshot(request).await.unwrap();
        let mut body = response.into_body();
        let mut text = String::new();
        let read = async {
            while let Some(frame) = body.frame().await {
                if let Ok(data) = frame.unwrap().into_data() {
                    text.push_str(std::str::from_utf8(&data).unwrap());
                }
                let event = text
                    .lines()
                    .filter_map(|line| line.strip_prefix("data:"))
                    .find_map(|data| serde_json::from_str::<serde_json::Value>(data.trim()).ok());
                if event.is_some() {
                    return event;
                }
            }
            serde_json::from_str(&text).ok()
        };
        tokio::time::timeout(std::time::Duration::from_secs(5), read)
            .await
            .expect("rmcp answered within 5s")
            .unwrap_or_else(|| panic!("no JSON-RPC message in response body: {text:?}"))
    }

    /// Issue #9484 (Aquilo): Claude Code asked for 2026-07-28 and was
    /// answered with it; the handshake must settle on a revision Proxima
    /// implements instead.
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

    /// The path the incident took: a per-request 2026-07-28 `tools/list`.
    /// rmcp's default admitted it and served a list without the cache
    /// fields that revision requires; now it is refused with the versions
    /// Proxima does serve, so the client can fall back.
    #[tokio::test]
    async fn per_request_2026_07_28_is_refused_with_the_supported_versions() {
        let message =
            first_message(mcp_service(), per_request_rpc("2026-07-28", "tools/list")).await;
        assert_eq!(
            message["error"]["code"],
            rmcp::model::ErrorCode::UNSUPPORTED_PROTOCOL_VERSION.0,
            "{message}"
        );
        assert_eq!(
            message["error"]["data"]["supported"],
            serde_json::json!(["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"]),
            "{message}"
        );
    }

    /// Tool and resource lists are projected from the caller's token, so
    /// they carry SEP-2549 hints that forbid a shared or stale cache.
    #[tokio::test]
    async fn list_results_carry_private_zero_ttl_cache_hints() {
        for method in ["tools/list", "resources/list", "resources/templates/list"] {
            let message = first_message(mcp_service(), per_request_rpc("2025-11-25", method)).await;
            assert_eq!(message["result"]["ttlMs"], 0, "{method}: {message}");
            assert_eq!(
                message["result"]["cacheScope"], "private",
                "{method}: {message}"
            );
        }
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

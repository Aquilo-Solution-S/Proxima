//! rmcp 1.6 entrypoint:
//! `rmcp::transport::streamable_http_server::StreamableHttpService`.
//!
//! The service is a Tower service composed into
//! `axum::Router::nest_service("/mcp", service)`. Proxima wraps it
//! with stricter Origin validation because rmcp permits missing
//! Origin headers when `allowed_origins` is set.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::middleware::{self, Next};
use axum::response::Response;
use http::{HeaderMap, StatusCode};
use proxima_core::wake::token_store::WakeTokenStore;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::McpServerError;
use crate::handler::DynamicHandler;
use crate::security::{OriginAllowlist, assert_loopback, wake_token_auth_layer};
use crate::server::DevMcpServer;

/// # Errors
///
/// Returns loopback validation, TCP bind, or HTTP server failures.
///
/// `wake_token_store` is required so each MCP request can be matched
/// against an in-flight wake invocation; the bearer-auth layer rejects
/// requests without a valid `Authorization: Bearer <token>` header. The
/// engine constructs the store at startup (Task 5) and passes the same
/// `Arc` here and into the dispatcher that mints tokens.
pub async fn serve_streamable_http(
    addr: SocketAddr,
    server: DevMcpServer,
    allowlist: OriginAllowlist,
    wake_token_store: Arc<WakeTokenStore>,
) -> Result<(JoinHandle<Result<(), McpServerError>>, SocketAddr), McpServerError> {
    assert_loopback(&addr)?;

    let cancellation_token = CancellationToken::new();
    let config = StreamableHttpServerConfig::default()
        .with_allowed_origins(allowlist.origins())
        .with_cancellation_token(cancellation_token.child_token());
    let service: StreamableHttpService<DynamicHandler, LocalSessionManager> =
        StreamableHttpService::new(
            move || {
                Ok(DynamicHandler {
                    server: server.clone(),
                })
            },
            Arc::default(),
            config,
        );
    // Layer order is bottom-up: origin_guard runs first (cheapest reject),
    // then wake-token auth, then perf recording, then the rmcp service.
    // Putting auth after origin_guard means a forbidden origin is rejected
    // before we ever touch the token store.
    let app = axum::Router::new()
        .nest_service("/mcp", service)
        .layer(middleware::from_fn(perf_recorder))
        .layer(wake_token_auth_layer(wake_token_store))
        .layer(middleware::from_fn_with_state(allowlist, origin_guard));

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

/// Dev-time per-request recorder. Active when `PROXIMA_PERF_SESSION_DIR`
/// names an existing directory; appends one NDJSON row per request to
/// `<dir>/mcp.json`. No-op otherwise. SSE responses report 0 `resp_bytes`
/// (no Content-Length); a streaming-aware counter is a future v2.
async fn perf_recorder(request: Request<Body>, next: Next) -> Response {
    let Some(dir) = perf_session_dir() else {
        return next.run(request).await;
    };
    let started = Instant::now();
    let method = request.method().clone();
    let route = request.uri().path().to_string();
    let req_bytes = request
        .headers()
        .get(http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);

    let resp = next.run(request).await;
    let status = resp.status().as_u16();
    let resp_bytes = resp
        .headers()
        .get(http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);

    let line = serde_json::json!({
        "ts_ms": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| millis_u64(d.as_millis())),
        "method": method.as_str(),
        "route": route,
        "status": status,
        "req_bytes": req_bytes,
        "resp_bytes": resp_bytes,
        "dur_ms": millis_u64(started.elapsed().as_millis()),
    });
    let path = dir.join("mcp.json");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        use std::io::Write;
        let _ = writeln!(f, "{line}");
    }
    resp
}

fn millis_u64(value: u128) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn perf_session_dir() -> Option<&'static PathBuf> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        std::env::var_os("PROXIMA_PERF_SESSION_DIR")
            .map(PathBuf::from)
            .filter(|p| p.exists())
    })
    .as_ref()
}

async fn origin_guard(
    State(allowlist): State<OriginAllowlist>,
    headers: HeaderMap,
    request: Request<Body>,
    next: Next,
) -> Response {
    if !allowlist.allows(&headers) {
        return Response::builder()
            .status(StatusCode::FORBIDDEN)
            .body(Body::from("origin not allowed"))
            .expect("static response");
    }
    next.run(request).await
}

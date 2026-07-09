//! rmcp 2.x entrypoint:
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
use axum::extract::Request;
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
use crate::security::{OriginAllowlist, assert_loopback, mcp_auth_layer_with_config};
use crate::server::McpToolHost;

/// `allowed_hosts` is the inbound `Host`/`:authority` allowlist for
/// rmcp's DNS-rebinding guard (bare hostnames or `host:port`). An empty
/// slice keeps rmcp's loopback-only default, which is correct for
/// loopback binds; network-exposed deployments must pass their own
/// public host(s) here or every non-loopback `Host` is rejected with
/// 403 before auth runs. The facade resolves this from
/// `PROXIMA_ALLOWED_HOSTS` / `PROXIMA_PUBLIC_URL` / allowed origins.
#[must_use]
pub fn streamable_http_service(
    server: McpToolHost,
    allowlist: &OriginAllowlist,
    allowed_hosts: &[String],
    cancel: &CancellationToken,
) -> StreamableHttpService<DynamicHandler, LocalSessionManager> {
    let mut config = StreamableHttpServerConfig::default()
        .with_allowed_origins(allowlist.origins())
        .with_cancellation_token(cancel.child_token());
    if !allowed_hosts.is_empty() {
        config = config.with_allowed_hosts(allowed_hosts.iter().cloned());
    }
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
/// host bearer path; retired local-token prefixes fail closed before host auth.
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
    // These helpers bind loopback only (asserted above), so rmcp's
    // loopback-only Host default is exactly right — no extra hosts.
    let service = streamable_http_service(server, &allowlist, &[], &cancellation_token);
    // Layer order is bottom-up (the last `.layer` is outermost): the
    // body-size guard runs first and 413s oversized requests before auth
    // or JSON parsing, then auth, then perf recording, then the rmcp
    // service. The auth guard also validates any present Origin; native
    // CLI clients commonly omit Origin, which is allowed after a valid
    // bearer token.
    let app = axum::Router::new()
        .nest_service("/mcp", service)
        .layer(middleware::from_fn(perf_recorder))
        .layer(mcp_auth_layer_with_config(auth, allowlist, revalidation))
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

/// Cap on the accepted request-body size. Sits generously above the
/// largest legitimate MCP request; anything larger is a client error or
/// abuse and is refused before auth or JSON parsing runs.
const MAX_REQUEST_BODY_BYTES: usize = 4 * 1024 * 1024;

/// Outermost guard: reject oversized request bodies with 413 before auth
/// or JSON parsing. A declared `Content-Length` over the cap is refused
/// immediately; the body is otherwise wrapped in [`Limited`] so a chunked
/// or length-lying stream errors past the cap instead of buffering
/// unbounded memory.
async fn enforce_body_limit(request: Request<Body>, next: Next) -> Response {
    if let Some(len) = request
        .headers()
        .get(http::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|raw| raw.parse::<usize>().ok())
        && len > MAX_REQUEST_BODY_BYTES
    {
        return payload_too_large();
    }
    let (parts, body) = request.into_parts();
    let limited = Body::new(Limited::new(body, MAX_REQUEST_BODY_BYTES));
    next.run(Request::from_parts(parts, limited)).await
}

fn payload_too_large() -> Response {
    (StatusCode::PAYLOAD_TOO_LARGE, "request body too large").into_response()
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
    use crate::security::{default_allowlist, mcp_auth_layer_with_config};

    /// Production stack order: body-limit outermost, then auth over an OK
    /// `/mcp` stub. Auth is headless (rejects every bearer), so any request
    /// that reaches auth returns 401 — letting us prove the body guard runs
    /// first.
    fn guarded_app() -> Router {
        Router::new()
            .route("/mcp", any(|| async { StatusCode::OK }))
            .layer(mcp_auth_layer_with_config(
                Arc::new(McpEdgeAuth::headless()),
                default_allowlist(),
                RevalidationConfig::default(),
            ))
            .layer(axum::middleware::from_fn(enforce_body_limit))
    }

    // S3: an over-cap declared Content-Length is 413'd before auth.
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

    // S3: a stream with no Content-Length that exceeds the cap errors when
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

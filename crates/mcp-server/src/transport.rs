//! rmcp 1.6 entrypoint:
//! `rmcp::transport::streamable_http_server::StreamableHttpService`.
//!
//! The service is a Tower service composed into
//! `axum::Router::nest_service("/mcp", service)`. Proxima wraps it
//! with stricter Origin validation because rmcp permits missing
//! Origin headers when `allowed_origins` is set.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::middleware::{self, Next};
use axum::response::Response;
use http::{HeaderMap, StatusCode};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::McpServerError;
use crate::handler::DynamicHandler;
use crate::security::{OriginAllowlist, assert_loopback};
use crate::server::DevMcpServer;

/// # Errors
///
/// Returns loopback validation, TCP bind, or HTTP server failures.
pub async fn serve_streamable_http(
    addr: SocketAddr,
    server: DevMcpServer,
    allowlist: OriginAllowlist,
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
    let app = axum::Router::new()
        .nest_service("/mcp", service)
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

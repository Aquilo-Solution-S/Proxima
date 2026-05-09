//! Adapter wiring [`crate::serve_streamable_http`] to
//! [`proxima_core::engine::EngineMcpListener`].
//!
//! Lives in this crate (which depends on `proxima-core`) so the engine
//! can stay listener-agnostic and dependency-free of `rmcp`. Hosts
//! build the adapter once (with their `DevMcpServer` + allowlist) and
//! hand it to the engine via `Engine::with_mcp_listener`.

use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use proxima_core::Engine;
use proxima_core::engine::{EngineMcpListener, RunningMcpListener};
use proxima_core::error::ProtocolError;
use proxima_core::wake::token_store::WakeTokenStore;

use crate::auth::McpAuthStore;
use crate::security::OriginAllowlist;
use crate::server::DevMcpServer;
use crate::transport::serve_streamable_http;

/// Concrete [`EngineMcpListener`] backed by Streamable HTTP. The
/// adapter clones the inner `DevMcpServer` per `start` invocation
/// (same pattern the headless `proxima-mcp` binary uses).
#[derive(Debug)]
pub struct EngineHostedMcpListener {
    server: DevMcpServer,
    allowlist: OriginAllowlist,
    auth_store: Option<Arc<McpAuthStore>>,
}

impl EngineHostedMcpListener {
    #[must_use]
    pub fn new(server: DevMcpServer, allowlist: OriginAllowlist) -> Self {
        Self {
            server,
            allowlist,
            auth_store: None,
        }
    }

    #[must_use]
    pub fn with_auth_store(
        server: DevMcpServer,
        allowlist: OriginAllowlist,
        auth_store: Arc<McpAuthStore>,
    ) -> Self {
        Self {
            server,
            allowlist,
            auth_store: Some(auth_store),
        }
    }
}

#[async_trait]
impl EngineMcpListener for EngineHostedMcpListener {
    async fn start(
        &self,
        addr: SocketAddr,
        wake_token_store: Arc<WakeTokenStore>,
        engine: Arc<Engine>,
    ) -> Result<RunningMcpListener, ProtocolError> {
        let auth_store = self
            .auth_store
            .clone()
            .unwrap_or_else(|| Arc::new(McpAuthStore::new(wake_token_store)));
        let (join, bound_addr) = serve_streamable_http(
            addr,
            self.server.clone().with_engine(engine),
            self.allowlist.clone(),
            auth_store,
        )
        .await
        .map_err(|e| ProtocolError::internal(format!("mcp listener start: {e}")))?;

        // The transport's join surfaces the server's terminal Result,
        // but the engine's lifecycle treats the task as fire-and-abort.
        // Adapt by spawning a thin wrapper that logs any non-cancelled
        // error and resolves to `()`.
        let join = tokio::spawn(async move {
            match join.await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => tracing::warn!(error = %e, "mcp transport exited with error"),
                Err(e) if e.is_cancelled() => {}
                Err(e) => tracing::warn!(error = %e, "mcp transport join failed"),
            }
        });

        Ok(RunningMcpListener { bound_addr, join })
    }
}

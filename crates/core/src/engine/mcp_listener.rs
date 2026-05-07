//! Engine-side trait for hosting an MCP HTTP/SSE listener.
//!
//! `proxima-core` cannot depend on `proxima-mcp-server` (the latter
//! depends on core, so the reverse would form a cycle). Instead, the
//! host (Tauri shell, headless CLI, integration tests) wires a
//! concrete listener that knows how to bind/serve, and hands it to
//! the engine via [`crate::engine::Engine::with_mcp_listener`].
//!
//! [`Engine::start`](crate::engine::Engine::start) calls
//! [`EngineMcpListener::start`] with the engine's [`WakeTokenStore`]
//! so the listener's auth layer matches the same store the dispatcher
//! mints into. Without a listener attached, `Engine::start` skips
//! the MCP step and `mcp_url()` stays `None`.

use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::task::JoinHandle;

use crate::error::ProtocolError;
use crate::wake::token_store::WakeTokenStore;

/// A bound, serving MCP listener owned by the engine.
///
/// `bound_addr` is the socket the kernel actually picked (especially
/// useful when the host requested `127.0.0.1:0`). `join` is the
/// transport task; `Engine::stop` aborts it.
pub struct RunningMcpListener {
    pub bound_addr: SocketAddr,
    pub join: JoinHandle<()>,
}

#[async_trait]
pub trait EngineMcpListener: Send + Sync {
    /// Bind to `addr`, mount the MCP service, and return a handle
    /// describing the bound address plus the transport task.
    async fn start(
        &self,
        addr: SocketAddr,
        wake_token_store: Arc<WakeTokenStore>,
    ) -> Result<RunningMcpListener, ProtocolError>;
}

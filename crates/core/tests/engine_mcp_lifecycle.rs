//! Phase 1d: `Engine::start` exposes the bound MCP URL after start;
//! `mcp_url()` is `None` before start; `Engine::stop` aborts the MCP
//! listener task.
//!
//! The MCP listener is wired via the `EngineMcpListener` trait so
//! `proxima-core` need not depend on `proxima-mcp-server` (that
//! would be a cycle). The test stub binds a `127.0.0.1:0` TCP
//! listener and parks until aborted — close enough to exercise the
//! lifecycle plumbing without pulling the rmcp transport into the
//! core test crate.

use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use proxima_core::engine::{Engine, EngineMcpListener, RunningMcpListener};
use proxima_core::error::ProtocolError;
use proxima_core::verbs::query::MemoryStore;
use proxima_core::verbs::schema::FlavorRegistryFrozen;

/// Test stub: bind a loopback TCP listener so the OS hands us a real
/// ephemeral port, spawn an accept loop that ignores connections,
/// and wrap the join in `RunningMcpListener`. This is enough to
/// exercise `Engine::start`/`stop`'s lifecycle wiring.
struct StubListener;

#[async_trait]
impl EngineMcpListener for StubListener {
    async fn start(
        &self,
        addr: SocketAddr,
        _engine: Arc<Engine>,
    ) -> Result<RunningMcpListener, ProtocolError> {
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .map_err(|e| ProtocolError::internal(format!("bind: {e}")))?;
        let bound_addr = listener
            .local_addr()
            .map_err(|e| ProtocolError::internal(format!("local_addr: {e}")))?;
        let join = tokio::spawn(async move {
            // Hold the listener until the task is aborted.
            loop {
                let _ = listener.accept().await;
            }
        });
        Ok(RunningMcpListener { bound_addr, join })
    }
}

fn make_test_engine() -> Engine {
    Engine::new(FlavorRegistryFrozen::new(), MemoryStore::new())
        .with_mcp_listener(Arc::new(StubListener))
}

#[tokio::test]
async fn engine_url_unset_before_start() {
    let engine = make_test_engine();
    assert!(engine.mcp_url().is_none(), "mcp_url None before start");
}

#[tokio::test]
async fn engine_start_exposes_mcp_url_then_stop_cancels() {
    let engine = Arc::new(make_test_engine());
    let handle = engine.clone().start().await.expect("start");
    let url = engine.mcp_url().expect("mcp url present after start");
    assert!(
        url.starts_with("http://127.0.0.1:"),
        "url {url} should be loopback"
    );
    assert!(url.ends_with("/mcp"), "url {url} should end with /mcp");

    engine.stop(handle);
}

#[tokio::test]
async fn engine_start_without_listener_leaves_url_none() {
    let engine = Arc::new(Engine::new(FlavorRegistryFrozen::new(), MemoryStore::new()));

    let handle = engine.clone().start().await.expect("start");
    assert!(engine.mcp_url().is_none(), "no listener -> no url");
    engine.stop(handle);
}

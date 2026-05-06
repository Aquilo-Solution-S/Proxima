//! Tauri-managed MCP listener lifetime.

use std::sync::Mutex;

use proxima_mcp_server::McpServerError;
use tokio::task::JoinHandle;

pub(crate) struct McpListenerHandle(Mutex<Option<JoinHandle<Result<(), McpServerError>>>>);

impl McpListenerHandle {
    pub(crate) fn new(handle: JoinHandle<Result<(), McpServerError>>) -> Self {
        Self(Mutex::new(Some(handle)))
    }
}

impl Drop for McpListenerHandle {
    fn drop(&mut self) {
        if let Ok(mut handle) = self.0.lock()
            && let Some(handle) = handle.take()
        {
            handle.abort();
        }
    }
}

impl std::fmt::Debug for McpListenerHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpListenerHandle").finish_non_exhaustive()
    }
}

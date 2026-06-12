//! Proxima MCP server transport layer.
//!
//! Owns the shared MCP handler plus Streamable HTTP serving used by
//! the headless binary and the embedded Tauri Shell listener.

mod auth;
mod engine_listener;
mod error;
mod handler;
pub mod security;
mod server;
mod transport;

pub use auth::{MASTER_TOKEN_PREFIX, McpAuthContext, McpEdgeAuth, WAKE_TOKEN_PREFIX};
pub use engine_listener::EngineHostedMcpListener;
pub use error::McpServerError;
pub use handler::DynamicHandler;
pub use security::{
    McpAuthLayer, OriginAllowlist, assert_loopback, default_allowlist, mcp_auth_layer,
    mcp_auth_layer_with_config,
};
pub use server::{McpToolHost, ToolInvocationError};
pub use transport::{
    serve_streamable_http, serve_streamable_http_with_revalidation, streamable_http_service,
};

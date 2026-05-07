//! Proxima MCP server transport layer.
//!
//! Owns the shared MCP handler plus Streamable HTTP serving used by
//! the headless binary and the embedded Tauri Shell listener.

mod engine_listener;
mod error;
mod handler;
pub mod security;
mod server;
mod transport;

pub use engine_listener::EngineHostedMcpListener;
pub use error::McpServerError;
pub use handler::DynamicHandler;
pub use security::{OriginAllowlist, assert_loopback, default_allowlist, wake_token_auth_layer};
pub use server::{DevMcpServer, ToolInvocationError};
pub use transport::serve_streamable_http;

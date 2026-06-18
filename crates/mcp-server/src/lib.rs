//! Proxima MCP server transport layer.
//!
//! Owns the shared MCP handler plus Streamable HTTP serving used by
//! the headless binary and the embedded Tauri Shell listener.

mod auth;
mod error;
mod handler;
pub mod oauth;
pub mod security;
mod server;
mod transport;

pub use auth::{MASTER_TOKEN_PREFIX, McpAuthContext, McpEdgeAuth};
pub use error::McpServerError;
pub use handler::DynamicHandler;
pub use oauth::{
    PROTECTED_RESOURCE_METADATA_PATH, ResourceServerMetadata, protected_resource_router,
};
pub use security::{
    McpAuthLayer, OriginAllowlist, assert_loopback, default_allowlist, mcp_auth_layer_with_config,
    mcp_auth_layer_with_metadata,
};
pub use server::{McpToolHost, ToolInvocationError};
pub use transport::{
    serve_streamable_http, serve_streamable_http_with_revalidation, streamable_http_service,
};

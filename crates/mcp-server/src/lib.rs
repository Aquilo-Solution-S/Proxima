//! Proxima MCP server transport layer.
//!
//! Owns the shared MCP handler plus Streamable HTTP serving used by
//! the headless binary and the embedded Tauri Shell listener.

mod auth;
mod error;
mod handler;
pub mod oauth;
#[cfg(feature = "rest")]
pub mod rest;
pub mod security;
pub mod selfdoc;
mod server;
mod session;
mod transport;

pub use auth::{McpAuthContext, McpEdgeAuth};
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
pub use session::{McpSessionBindings, owner_key, parse_owner_key};
pub use transport::{
    enforce_body_limit, serve_streamable_http, serve_streamable_http_with_revalidation,
    streamable_http_service,
};

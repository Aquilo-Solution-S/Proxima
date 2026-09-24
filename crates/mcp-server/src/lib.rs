//! Proxima MCP server transport layer.
//!
//! Owns the shared MCP handler plus Streamable HTTP serving used by
//! the headless binary and the embedded Tauri Shell listener.

mod auth;
mod error;
mod handler;
pub mod oauth;
mod request_scope;
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
    MCP_PATH, MCP_PROTECTED_RESOURCE_METADATA_PATH, PROTECTED_RESOURCE_METADATA_PATH,
    ProtectedResource, ResourceServerMetadata, SCOPES_SUPPORTED, protected_resource_router,
};
pub use request_scope::RequestHeaderAllowlist;
pub use security::{
    CorsLayer, HostAllowlist, HostGuardLayer, McpAuthLayer, OriginAllowlist, assert_loopback,
    cors_layer, default_allowlist, host_guard_layer, mcp_auth_layer_with_config,
    mcp_auth_layer_with_metadata,
};
pub use server::{McpToolHost, ToolInvocationError};
pub use session::{McpSessionBindings, owner_key, parse_owner_key};
pub use transport::{
    BodyLimitLayer, MAX_REQUEST_BODY_BYTES, McpTransportConfig, body_limit_layer,
    enforce_body_limit, serve_streamable_http, serve_streamable_http_with_revalidation,
    streamable_http_service, streamable_http_service_with_transport,
};

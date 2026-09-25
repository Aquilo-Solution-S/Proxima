//! Proxima MCP server transport layer.
//!
//! Owns the shared MCP handler plus Streamable HTTP serving used by
//! the headless binary and the embedded Tauri Shell listener.

mod auth;
mod error;
mod handler;
mod host_tools;
pub mod oauth;
mod request_scope;
#[cfg(feature = "rest")]
pub mod rest;
pub mod security;
pub mod selfdoc;
mod server;
mod session;
mod tool_list;
mod transport;

pub use auth::{McpAuthContext, McpEdgeAuth};
pub use error::McpServerError;
pub use handler::{
    DynamicHandler, auth_context, author_from_args, mcp_tool_error_to_error_data,
    peer_implementation, strip_call_context_args, tool_invocation_error_to_error_data,
};
pub use host_tools::{McpHostTool, McpHostTools};
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
pub use server::{MAX_HOST_TOOL_NAME_CHARS, McpToolHost, ToolInvocationError, reject_nul_in_args};
pub use session::{McpSessionBindings, owner_key, parse_owner_key};
pub use tool_list::ToolListNotifier;
pub use transport::{
    BodyLimitLayer, MAX_REQUEST_BODY_BYTES, McpStreamableService, McpTransportConfig,
    body_limit_layer, enforce_body_limit, serve_streamable_http,
    serve_streamable_http_with_revalidation, streamable_http_service,
    streamable_http_service_with_transport,
};

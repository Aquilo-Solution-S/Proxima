//! Build-time MCP tool registration for local development adapters.
//!
//! Composite binaries register tools through flavor crates at startup;
//! there is no runtime registration path.

pub mod behavior;
pub mod core_tools;
pub mod cursor;
pub mod handles;
pub(crate) mod schema;

mod context;
mod error;
mod ids;
mod manifest;
mod names;
mod presentation;
mod tool;
mod types;

#[cfg(test)]
mod tests;

pub use behavior::{Next, RequestBehavior, ScopeGateBehavior, TerminalDispatch, ToolCall};
pub use core_tools::list_substrate_tools::scope_permits_action;
pub use error::{McpToolError, McpToolErrorKind};
pub use handles::{
    MemoryHandleClass, PrefixedUuidClass, PrefixedUuidError, format_prefixed_uuid,
    parse_prefixed_uuid,
};
pub use manifest::{
    CORE_RESOURCES, CoreActionMeta, CoreResourceMeta, McpToolAnnotations, all_core_actions,
    all_core_resources, core_action_meta, core_tool_annotations, core_tool_has_actions,
};
pub use names::{provider_safe_tool_name, tool_name_matches};
pub use presentation::{McpPresentationExt, McpToolCaller, McpToolPresentation};
pub use tool::{McpActionArgSpec, McpCallFn, McpTool, McpToolDescriptor, McpToolOrigin};
pub(crate) use tool::{prepare_flat_tool_args, validate_action_args};
pub use types::{McpAuthorContext, McpToolCtx, McpToolExtensions};

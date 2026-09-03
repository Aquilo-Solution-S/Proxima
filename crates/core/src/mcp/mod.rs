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
// The one enforcement point for the reserved `model_id` operator label.
// Published because a flavor tool that accepts `model_id` must resolve it
// the same way the core authoring tools do — the transport edges only see a
// top-level field, so re-implementing this is how a nested one gets through.
pub use core_tools::memory::util::operator_label;
pub use error::{McpToolError, McpToolErrorKind};
pub use handles::{
    MemoryHandleClass, PrefixedUuidClass, PrefixedUuidError, format_prefixed_uuid,
    parse_prefixed_uuid,
};
pub use manifest::{
    CoreActionMeta, McpToolAnnotations, all_core_actions, all_core_resources, canonical_scope_keys,
    canonical_scope_keys_excluding, core_action_meta, core_tool_annotations,
};
pub use names::{provider_safe_tool_name, tool_name_matches};
pub use presentation::{McpPresentationExt, McpToolPresentation};
pub use schema::schema_bound_mismatches;
pub use tool::{
    McpActionArgSpec, McpArgvActionSpec, McpCallFn, McpTool, McpToolAudience, McpToolDescriptor,
    McpToolOrigin,
};
pub(crate) use tool::{prepare_flat_tool_args, resolve_argv_action, validate_action_args};
pub use types::{
    MAX_OPERATOR_LABEL_CHARS, McpAuthorContext, McpToolCtx, OperatorLabelConflict,
    UNKNOWN_OPERATOR_LABEL, resolve_operator_label,
};

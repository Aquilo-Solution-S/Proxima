//! Host-served MCP tools: tools a host lists and runs beside the frozen
//! flavor registry, through the same scope gate and request behaviors.

use async_trait::async_trait;
use proxima_core::mcp::{McpToolAnnotations, McpToolError, ToolCall};

use crate::auth::McpAuthContext;

/// One tool a host serves. Flat: its palette key is its name, and the
/// scope gate classifies it by `annotations.read_only` (silence is a
/// write).
#[derive(Debug, Clone, PartialEq)]
pub struct McpHostTool {
    /// Canonical and wire name: 1..=[`MAX_HOST_TOOL_NAME_CHARS`] characters
    /// of `[A-Za-z0-9_.-]`, so it is never a `tool:action` leaf or a
    /// `resource:` scope key. A name the flavor registry serves (canonical or
    /// wire), a repeat, or a malformed name is not served; see
    /// [`McpToolHost::host_tools_for`](crate::McpToolHost::host_tools_for).
    ///
    /// [`MAX_HOST_TOOL_NAME_CHARS`]: crate::MAX_HOST_TOOL_NAME_CHARS
    pub name: String,
    pub description: String,
    pub args_schema: serde_json::Value,
    pub output_schema: serde_json::Value,
    pub annotations: McpToolAnnotations,
}

/// A host's tool source, registered on the runtime builder
/// (`RuntimeBuilder::host_tools`) or on [`McpToolHost::with_host_tools`].
///
/// `tools/list` shows what [`Self::list`] returns for the caller, filtered
/// by the caller's palette and owner role like every registry tool.
/// `tools/call` runs a listed tool through the registry's request behaviors
/// (scope gate first) with [`Self::call`] as the terminal; the call's
/// services carry an [`McpHostToolCall`](proxima_core::McpHostToolCall).
///
/// [`McpToolHost::with_host_tools`]: crate::McpToolHost::with_host_tools
#[async_trait]
pub trait McpHostTools: Send + Sync + std::fmt::Debug {
    /// The tools `auth` may see. Called on every list and every call; a
    /// tool it does not return for this caller is not callable by them.
    fn list(&self, auth: &McpAuthContext) -> Vec<McpHostTool>;

    /// Run one listed tool. `call.name` is the canonical name; `call.ctx`
    /// is the caller's tool context (owner, authorization, services,
    /// engine).
    ///
    /// # Errors
    ///
    /// Any [`McpToolError`]; it maps to the same JSON-RPC error a registry
    /// tool's would.
    async fn call(&self, call: ToolCall) -> Result<serde_json::Value, McpToolError>;
}

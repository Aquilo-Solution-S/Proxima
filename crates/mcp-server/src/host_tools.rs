//! Host-served MCP tools: tools a host lists and runs beside the frozen
//! flavor registry, through the same scope gate and request behaviors.

use async_trait::async_trait;
use proxima_core::FlavorServices;
use proxima_core::mcp::{McpToolError, ToolCall, ToolEffect, ToolReply};

use crate::auth::McpAuthContext;

/// Longest host `instructions` contribution served, in characters. A longer
/// one is omitted with a warning, as an invalid host tool is.
pub const MAX_HOST_INSTRUCTIONS_CHARS: usize = 4096;

/// One tool a host serves. Flat: its palette key is its name, and the
/// scope gate classifies it by its `effect`.
///
/// Built with [`Self::new`] and the `with_*` setters; the struct is
/// `#[non_exhaustive]`, so a field added later does not break a host.
///
/// ```compile_fail
/// let _tool = proxima_mcp_server::McpHostTool {
///     name: "host_tool".into(),
///     description: "a tool".into(),
///     args_schema: serde_json::json!({"type": "object"}),
///     output_schema: None,
///     effect: proxima_core::ToolEffect::ReadOnly,
///     meta: None,
///     open_world: None,
///     services: proxima_core::FlavorServices::default(),
/// };
/// ```
#[derive(Debug, Clone)]
#[non_exhaustive]
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
    /// Object schema; unions of object branches are normalized on `tools/list`.
    /// Invalid output schemas are omitted from the list with a warning.
    /// A tool that declares one answers [`ToolReply::Structured`] or
    /// [`ToolReply::Failure`]; a [`ToolReply::Content`] or
    /// [`ToolReply::StructuredFailure`] reply from it is an internal error.
    /// `None` for a tool that answers with content or a structured failure.
    pub output_schema: Option<serde_json::Value>,
    /// What the tool does; its MCP hints are
    /// [`McpToolAnnotations::host`](proxima_core::McpToolAnnotations::host),
    /// the only source of `readOnlyHint`, `destructiveHint` and
    /// `idempotentHint`.
    pub effect: ToolEffect,
    /// The tool's `_meta` on `tools/list`.
    pub meta: Option<serde_json::Map<String, serde_json::Value>>,
    /// `openWorldHint`, which [`McpToolAnnotations::host`] leaves unset
    /// because [`ToolEffect`] says nothing about it.
    ///
    /// [`McpToolAnnotations::host`]: proxima_core::McpToolAnnotations::host
    pub open_world: Option<bool>,
    /// Values a call resolved to this entry carries: they join the call's
    /// request services beside
    /// [`McpHostToolCall`](proxima_core::McpHostToolCall), taken from the list
    /// the call was resolved against, so [`McpHostTools::call`] and every
    /// request behavior read the entry the scope gate judged without listing
    /// again. Proxima neither reads them nor lists them. A type the call's
    /// services already hold (`McpHostToolCall`, a request or boot service) is
    /// an internal error.
    pub services: FlavorServices,
}

impl McpHostTool {
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        args_schema: serde_json::Value,
        effect: ToolEffect,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            args_schema,
            output_schema: None,
            effect,
            meta: None,
            open_world: None,
            services: FlavorServices::default(),
        }
    }

    /// Declare an output schema (see [`Self::output_schema`]).
    #[must_use]
    pub fn with_output_schema(mut self, output_schema: serde_json::Value) -> Self {
        self.output_schema = Some(output_schema);
        self
    }

    /// Set the tool's `_meta`.
    #[must_use]
    pub fn with_meta(mut self, meta: serde_json::Map<String, serde_json::Value>) -> Self {
        self.meta = Some(meta);
        self
    }

    /// Set `openWorldHint`.
    #[must_use]
    pub const fn with_open_world(mut self, open_world: bool) -> Self {
        self.open_world = Some(open_world);
        self
    }

    /// Set the services a call resolved to this entry carries (see [`Self::services`]).
    #[must_use]
    pub fn with_services(mut self, services: FlavorServices) -> Self {
        self.services = services;
        self
    }
}

/// A host's tool source, registered on the runtime builder
/// (`RuntimeBuilder::host_tools`) or on [`McpToolHost::with_host_tools`].
///
/// `tools/list` shows what [`Self::list`] returns for the caller, filtered
/// by the caller's palette, owner role and every request behavior's
/// `visible`, like every registry tool. `tools/call` runs a listed tool
/// through the registry's request behaviors (scope gate first) with
/// [`Self::call`] as the terminal; the call's services carry an
/// [`McpHostToolCall`](proxima_core::McpHostToolCall) and the resolved entry's
/// [`services`](McpHostTool::services). Each call resolves its name once.
///
/// [`McpToolHost::with_host_tools`]: crate::McpToolHost::with_host_tools
#[async_trait]
pub trait McpHostTools: Send + Sync + std::fmt::Debug {
    /// The tools `auth` may see. Called once per `tools/list`, and by the
    /// default [`Self::resolve`]; a tool it does not return for this caller
    /// is not callable by them.
    ///
    /// # Errors
    ///
    /// Any [`McpToolError`]; it fails the `tools/list`, or the `tools/call`
    /// that needed the catalog, with the JSON-RPC error a registry tool's
    /// would map to. Proxima never degrades to an empty list: a host that
    /// wants one on a failed read returns `Ok(Vec::new())`.
    async fn list(&self, auth: &McpAuthContext) -> Result<Vec<McpHostTool>, McpToolError>;

    /// The tool named `name` that [`Self::list`] returns for `auth`, if any.
    /// Called once per `tools/call` of a name the registry does not serve. A
    /// host whose listing reads per-tool state answers the one name here, so
    /// a call costs what it calls. Default: [`Self::list`], then its entry
    /// of that name.
    ///
    /// # Errors
    ///
    /// Any [`McpToolError`]; it fails the `tools/call`, as a failed
    /// [`Self::list`] does.
    async fn resolve(
        &self,
        auth: &McpAuthContext,
        name: &str,
    ) -> Result<Option<McpHostTool>, McpToolError> {
        Ok(self
            .list(auth)
            .await?
            .into_iter()
            .find(|tool| tool.name == name))
    }

    /// Run one listed tool. `call.name` is the canonical name; `call.ctx`
    /// is the caller's tool context (owner, authorization, services,
    /// engine).
    ///
    /// A tool that ran answers with a [`ToolReply`]: [`ToolReply::Failure`]
    /// for a failure the model should read and retry, or
    /// [`ToolReply::StructuredFailure`] when the failure also carries an
    /// object a program reads. The `Err` side is for a call that cannot be
    /// answered at all, and maps to a JSON-RPC error. The one exception is
    /// [`McpToolError::Failure`], answered as a [`ToolReply::Failure`] of its
    /// text, as a registry tool's is.
    ///
    /// # Errors
    ///
    /// Any [`McpToolError`]; it maps to the same JSON-RPC error a registry
    /// tool's would. A [`ToolReply::Structured`] value must be a JSON
    /// object, and a [`ToolReply::Content`] or
    /// [`ToolReply::StructuredFailure`] reply is refused from a tool that
    /// declared an output schema; either is logged and returned as a
    /// redacted internal error.
    async fn call(&self, call: ToolCall) -> Result<ToolReply, McpToolError>;

    /// Text this host adds to the server `instructions` `initialize` and
    /// `server/discover` return for `auth`, after Proxima's generated text
    /// (alone when that is empty). Longer than
    /// [`MAX_HOST_INSTRUCTIONS_CHARS`] it is omitted with a warning.
    /// Default: none.
    async fn instructions(&self, _auth: &McpAuthContext) -> Option<String> {
        None
    }
}

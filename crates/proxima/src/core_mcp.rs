use std::sync::Arc;

use proxima_core::mcp::McpToolExtensions;
use proxima_core::{
    AuthzContext, Engine, FlavorRegistryFrozen, McpAuthorContext, McpToolDescriptor,
    McpToolErrorKind, Owner, ToolScope, core_tool_annotations, provider_safe_tool_name,
    tool_name_matches,
};
use proxima_mcp_server::{McpAuthContext, McpToolHost, ToolInvocationError};

/// Facade handle for listing and dispatching the composed engine MCP tools
/// from an embedding host's own authenticated endpoint.
#[derive(Clone, Debug)]
pub struct CoreMcpTools {
    host: McpToolHost,
}

/// MCP tool descriptor projected without rmcp transport types.
#[derive(Debug, Clone, PartialEq)]
pub struct CoreToolInfo {
    pub name: String,
    pub description: String,
    pub args_schema: serde_json::Value,
    pub read_only: Option<bool>,
    pub destructive: Option<bool>,
    pub idempotent: Option<bool>,
    pub open_world: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CoreMcpErrorKind {
    #[error("not authorized")]
    NotAuthorized,
    #[error("not found")]
    NotFound,
    #[error("invalid input")]
    InvalidInput,
    #[error("invalid request")]
    InvalidRequest,
    #[error("internal")]
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CoreMcpError {
    #[error("tool not authorized: {0}")]
    NotAuthorized(String),
    #[error("tool not found: {0}")]
    NotFound(String),
    #[error("tool error ({kind:?}): {message}")]
    Tool {
        kind: McpToolErrorKind,
        message: String,
    },
}

impl CoreMcpError {
    #[must_use]
    pub const fn kind(&self) -> CoreMcpErrorKind {
        match self {
            Self::NotAuthorized(_) => CoreMcpErrorKind::NotAuthorized,
            Self::NotFound(_) => CoreMcpErrorKind::NotFound,
            Self::Tool { kind, .. } => match kind {
                McpToolErrorKind::InvalidInput => CoreMcpErrorKind::InvalidInput,
                McpToolErrorKind::InvalidRequest => CoreMcpErrorKind::InvalidRequest,
                McpToolErrorKind::Internal => CoreMcpErrorKind::Internal,
            },
        }
    }

    fn from_invocation_error(err: ToolInvocationError) -> Self {
        match err {
            ToolInvocationError::NotAuthorized(tool) => Self::NotAuthorized(tool),
            ToolInvocationError::ToolNotFound(tool) => Self::NotFound(tool),
            ToolInvocationError::Tool(err) => Self::Tool {
                kind: err.kind(),
                message: err.to_string(),
            },
        }
    }
}

impl CoreMcpTools {
    #[must_use]
    pub fn new(
        registry: Arc<FlavorRegistryFrozen>,
        engine: Arc<Engine>,
        extensions: McpToolExtensions,
    ) -> Self {
        Self {
            host: McpToolHost::from_parts(registry, extensions).with_engine(engine),
        }
    }

    /// List all build-time registered MCP tools from the frozen registry.
    ///
    /// This intentionally does not filter by `ToolScope`: the embedding
    /// host owns presentation/gating on its unified endpoint, and
    /// [`Self::call_core_tool`] runs the host behavior chain before dispatch.
    #[must_use]
    pub fn list_core_tools(&self) -> Vec<CoreToolInfo> {
        self.host
            .registry()
            .list_mcp_tools()
            .iter()
            .map(tool_info_from_descriptor)
            .collect::<Vec<_>>()
    }

    /// List registered MCP tools visible under `scope`.
    #[must_use]
    pub fn list_core_tools_for_scope(&self, scope: &ToolScope) -> Vec<CoreToolInfo> {
        self.host
            .registry()
            .list_mcp_tools()
            .iter()
            .filter(|descriptor| scope.allows_group_advertisement(descriptor.name))
            .map(tool_info_from_descriptor)
            .collect()
    }

    /// Dispatch one registered core/flavor MCP tool under caller-supplied
    /// host authz and storage owner.
    ///
    /// Direct facade dispatch supplies host authz and optional caller
    /// Perspective metadata only.
    ///
    /// # Errors
    ///
    /// Returns `NotAuthorized` when the host behavior chain rejects `name`,
    /// `NotFound` for an unknown registry tool, or `Tool` for validation,
    /// storage, or role errors.
    pub async fn call_core_tool(
        &self,
        authz: AuthzContext,
        owner: Owner,
        model_id: Option<String>,
        name: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, CoreMcpError> {
        let Some(descriptor) = self.find_tool(name) else {
            return Err(CoreMcpError::NotFound(name.to_string()));
        };
        let canonical_name = descriptor.name;

        let author = McpAuthorContext {
            model_id: model_id.clone().unwrap_or_else(|| "unknown".to_string()),
            client_name: "host".into(),
            client_version: "0".into(),
            caller_self_perspective: None,
        };
        let auth = McpAuthContext {
            owner,
            authz,
            model_id,
        };

        self.host
            .call_tool(canonical_name, args, author, Some(auth))
            .await
            .map_err(CoreMcpError::from_invocation_error)
    }

    /// Read one core MCP resource through the same host authz boundary as
    /// direct tool dispatch.
    ///
    /// # Errors
    ///
    /// Returns `NotAuthorized` when the caller's resource scope rejects the
    /// URI, or `Tool` for resource validation/storage errors.
    pub async fn read_core_resource(
        &self,
        authz: AuthzContext,
        owner: Owner,
        model_id: Option<String>,
        uri: &str,
    ) -> Result<serde_json::Value, CoreMcpError> {
        let author = McpAuthorContext {
            model_id: model_id.clone().unwrap_or_else(|| "unknown".to_string()),
            client_name: "host".into(),
            client_version: "0".into(),
            caller_self_perspective: None,
        };
        let auth = McpAuthContext {
            owner,
            authz,
            model_id,
        };

        self.host
            .read_resource(uri, author, Some(auth))
            .await
            .map_err(CoreMcpError::from_invocation_error)
    }

    fn find_tool(&self, name: &str) -> Option<&McpToolDescriptor> {
        find_tool_descriptor(self.host.registry().list_mcp_tools(), name)
    }
}

fn find_tool_descriptor<'a>(
    descriptors: &'a [McpToolDescriptor],
    name: &str,
) -> Option<&'a McpToolDescriptor> {
    descriptors
        .iter()
        .find(|descriptor| tool_name_matches(descriptor.name, name))
}

fn tool_info_from_descriptor(descriptor: &McpToolDescriptor) -> CoreToolInfo {
    let annotations = core_tool_annotations(descriptor.name).unwrap_or_default();
    CoreToolInfo {
        name: provider_safe_tool_name(descriptor.name),
        description: descriptor.description.to_string(),
        args_schema: descriptor.args_schema.clone(),
        read_only: annotations.read_only,
        destructive: annotations.destructive,
        idempotent: annotations.idempotent,
        open_world: annotations.open_world,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::FutureExt as _;
    use proxima_core::{McpToolCtx, McpToolError, StorageError};

    #[test]
    fn facade_error_mapping_preserves_invalid_input_kind() {
        let err = CoreMcpError::from_invocation_error(ToolInvocationError::Tool(
            McpToolError::InvalidInput("bad params".into()),
        ));

        assert_eq!(err.kind(), CoreMcpErrorKind::InvalidInput);
        assert!(matches!(
            err,
            CoreMcpError::Tool {
                kind: McpToolErrorKind::InvalidInput,
                ..
            }
        ));
    }

    #[test]
    fn facade_error_mapping_preserves_internal_storage_kind() {
        let err = CoreMcpError::from_invocation_error(ToolInvocationError::Tool(
            McpToolError::Storage(StorageError::Internal("boom".into())),
        ));

        assert_eq!(err.kind(), CoreMcpErrorKind::Internal);
        assert!(matches!(
            err,
            CoreMcpError::Tool {
                kind: McpToolErrorKind::Internal,
                ..
            }
        ));
    }

    #[test]
    fn facade_tool_lookup_accepts_canonical_and_provider_safe_names() {
        fn call(
            _ctx: McpToolCtx,
            _args: serde_json::Value,
        ) -> futures::future::BoxFuture<'static, Result<serde_json::Value, McpToolError>> {
            async { Ok(serde_json::json!({})) }.boxed()
        }

        let descriptors = vec![McpToolDescriptor {
            name: "provider/slashed_name",
            description: "test",
            origin: proxima_core::McpToolOrigin::Flavor("provider".into()),
            produces_schema_ids: &[],
            args_schema: serde_json::json!({ "type": "object" }),
            action_arg_specs: &[],
            call,
        }];

        assert_eq!(
            find_tool_descriptor(&descriptors, "provider/slashed_name").map(|tool| tool.name),
            Some("provider/slashed_name")
        );
        assert_eq!(
            find_tool_descriptor(&descriptors, "provider_slashed_name").map(|tool| tool.name),
            Some("provider/slashed_name")
        );
    }
}

use std::sync::Arc;

use proxima_core::{AuthzContext, Engine, FlavorRegistryFrozen, McpAuthorContext, Owner};
use proxima_mcp_server::{McpAuthContext, McpToolHost, ToolInvocationError};
use sqlx::PgPool;

const FACT_RETENTION_TOOL_NAMES: [&str; 4] = [
    "core/get_fact_retention",
    "core/set_fact_retention",
    "core/clear_fact_retention",
    "core/cleanup_facts",
];

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
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CoreMcpError {
    #[error("tool not authorized: {0}")]
    NotAuthorized(String),
    #[error("tool not found: {0}")]
    NotFound(String),
    #[error("tool error: {0}")]
    Tool(String),
}

impl CoreMcpTools {
    #[must_use]
    pub fn new(
        pool: PgPool,
        company_owner: Owner,
        registry: Arc<FlavorRegistryFrozen>,
        engine: Arc<Engine>,
    ) -> Self {
        Self {
            host: McpToolHost::from_pool(pool, company_owner, registry).with_engine(engine),
        }
    }

    /// List all build-time registered MCP tools from the frozen registry.
    ///
    /// This intentionally does not filter by `ToolScope`: the embedding
    /// host owns presentation/gating on its unified endpoint, and
    /// [`Self::call_core_tool`] still enforces per-call scope before
    /// dispatch.
    #[must_use]
    pub fn list_core_tools(&self) -> Vec<CoreToolInfo> {
        let tools = self
            .host
            .registry()
            .list_mcp_tools()
            .iter()
            .map(|descriptor| CoreToolInfo {
                name: descriptor.name.to_string(),
                description: descriptor.description.to_string(),
                args_schema: descriptor.args_schema.clone(),
            })
            .collect::<Vec<_>>();
        debug_assert!(
            FACT_RETENTION_TOOL_NAMES
                .iter()
                .all(|name| tools.iter().any(|tool| tool.name == *name)),
            "fact-retention tools must be present in CoreMcpTools"
        );
        tools
    }

    /// Dispatch one registered core/flavor MCP tool under caller-supplied
    /// host authz and storage owner.
    ///
    /// Tools that require a `caller_self_perspective` for audit-emitting
    /// config mutations may log a non-fatal audit emission failure under
    /// host dispatch because no personality author is supplied; the tool
    /// call itself still succeeds or fails on its own verb checks.
    ///
    /// # Errors
    ///
    /// Returns `NotAuthorized` when the caller's tool scope rejects `name`,
    /// `NotFound` for an unknown registry tool, or `Tool` for the tool's own
    /// validation/storage/role errors.
    pub async fn call_core_tool(
        &self,
        authz: AuthzContext,
        owner: Owner,
        model_id: Option<String>,
        name: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, CoreMcpError> {
        if !authz.capabilities.tool_scope.allows(name) {
            return Err(CoreMcpError::NotAuthorized(name.to_string()));
        }

        let author = McpAuthorContext {
            model_id: model_id.clone().unwrap_or_else(|| "unknown".to_string()),
            client_name: "host".into(),
            client_version: "0".into(),
            personality_instance_id: None,
            caller_self_perspective: None,
        };
        let auth = McpAuthContext {
            owner: owner.clone(),
            authz,
            model_id,
            master_token_id: None,
        };

        self.host
            .call_tool(name, args, author, Some(auth))
            .await
            .map_err(|err| match err {
                ToolInvocationError::ToolNotFound(tool) => CoreMcpError::NotFound(tool),
                ToolInvocationError::Tool(err) => CoreMcpError::Tool(err.to_string()),
            })
    }
}

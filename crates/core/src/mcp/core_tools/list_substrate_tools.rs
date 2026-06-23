//! `core/list_substrate_tools` — dispatchable substrate and flavor MCP tools.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::mcp::{McpToolCtx, McpToolError};

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ListSubstrateToolsArgs {}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SubstrateToolItem {
    pub tool_id: String,
    pub source: String,
    pub description: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ListSubstrateToolsOutput {
    pub tools: Vec<SubstrateToolItem>,
}

#[allow(clippy::unused_async)]
/// # Errors
///
/// This projection is infallible today; the `Result` shape matches the tool
/// dispatch contract.
pub async fn list_substrate_tools(
    ctx: McpToolCtx,
    _args: ListSubstrateToolsArgs,
) -> Result<ListSubstrateToolsOutput, McpToolError> {
    let mut tools = Vec::new();
    for desc in ctx.registry.list_mcp_tools() {
        if !ctx
            .authz
            .capabilities
            .tool_scope
            .allows_group_advertisement(desc.name)
        {
            continue;
        }
        let source = if desc.name.starts_with("core/") {
            "substrate".into()
        } else {
            let flavor = desc.name.split('/').next().unwrap_or("flavor");
            format!("flavor:{flavor}")
        };
        tools.push(SubstrateToolItem {
            tool_id: desc.name.to_string(),
            source,
            description: desc.description.to_string(),
        });
    }
    Ok(ListSubstrateToolsOutput { tools })
}

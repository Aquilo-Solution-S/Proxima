//! `core/list_workspace_tools` — workspace tool catalog.

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::McpTool;
use crate::mcp::{McpToolCtx, McpToolError};

#[derive(Debug, Default)]
pub struct ListWorkspaceToolsTool;

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ListWorkspaceToolsArgs {}

#[derive(Debug, Serialize, JsonSchema)]
pub struct WorkspaceToolItem {
    pub tool_id: String,
    pub description: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ListWorkspaceToolsOutput {
    pub tools: Vec<WorkspaceToolItem>,
}

impl McpTool for ListWorkspaceToolsTool {
    const NAME: &'static str = "core/list_workspace_tools";
    const DESCRIPTION: &'static str =
        "List tool ids accepted in WakeEntryDraftInput.workspace_tool_palette.";
    type Args = ListWorkspaceToolsArgs;
    type Output = ListWorkspaceToolsOutput;

    fn call(
        _ctx: McpToolCtx,
        _args: ListWorkspaceToolsArgs,
    ) -> BoxFuture<'static, Result<ListWorkspaceToolsOutput, McpToolError>> {
        Box::pin(async move {
            let tools = crate::personality::WORKSPACE_TOOL_CATALOG
                .iter()
                .map(|(id, desc)| WorkspaceToolItem {
                    tool_id: (*id).to_string(),
                    description: (*desc).to_string(),
                })
                .collect();
            Ok(ListWorkspaceToolsOutput { tools })
        })
    }
}

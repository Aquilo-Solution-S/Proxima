//! `core/list_substrate_tools` — substrate-pack tools + flavor MCP tools.

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::McpTool;
use crate::mcp::{McpToolCtx, McpToolError};
use crate::personality::{EMIT_ABSTRACTION_TOOL_ID, EMIT_PERSPECTIVE_TOOL_ID, scoped_emit_tool_id};
use crate::verbs::schema::PayloadKind;

#[derive(Debug, Default)]
pub struct ListSubstrateToolsTool;

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

impl McpTool for ListSubstrateToolsTool {
    const NAME: &'static str = "core/list_substrate_tools";
    const DESCRIPTION: &'static str =
        "List tool ids accepted in WakeEntryDraftInput.substrate_tool_palette.";
    type Args = ListSubstrateToolsArgs;
    type Output = ListSubstrateToolsOutput;

    fn call(
        ctx: McpToolCtx,
        _args: ListSubstrateToolsArgs,
    ) -> BoxFuture<'static, Result<ListSubstrateToolsOutput, McpToolError>> {
        Box::pin(async move {
            let mut tools = Vec::new();
            for tool in crate::personality::substrate_pack() {
                tools.push(SubstrateToolItem {
                    tool_id: tool.tool_id().to_string(),
                    source: "substrate".into(),
                    description: tool.description().to_string(),
                });
            }
            for desc in ctx.registry.list_mcp_tools() {
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
            for schema in ctx.registry.list() {
                let Some(base_tool_id) = scoped_emit_base_tool(schema.kind) else {
                    continue;
                };
                let tool_id = scoped_emit_tool_id(
                    base_tool_id,
                    schema.schema_id.as_str(),
                    schema.schema_version.into_inner(),
                );
                tools.push(SubstrateToolItem {
                    tool_id,
                    source: "substrate".into(),
                    description: format!(
                        "Emit one {:?} memory with schema {} v{}.",
                        schema.kind,
                        schema.schema_id.as_str(),
                        schema.schema_version.into_inner()
                    ),
                });
            }
            Ok(ListSubstrateToolsOutput { tools })
        })
    }
}

fn scoped_emit_base_tool(kind: PayloadKind) -> Option<&'static str> {
    match kind {
        PayloadKind::Abstraction => Some(EMIT_ABSTRACTION_TOOL_ID),
        PayloadKind::Perspective => Some(EMIT_PERSPECTIVE_TOOL_ID),
        PayloadKind::Fact
        | PayloadKind::Goal
        | PayloadKind::Edge
        | PayloadKind::CitedObject
        | PayloadKind::CitationMapping => None,
    }
}

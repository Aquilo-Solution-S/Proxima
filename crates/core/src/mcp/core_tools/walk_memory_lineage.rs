//! `core/walk_memory_lineage` — wire-facing memory lineage walk.

use std::collections::HashMap;

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::mcp::{McpToolCtx, McpToolError};
use crate::verbs::query::{MemoryLineageDirection, MemoryLineageRequest};
use crate::{EdgeId, McpTool, MemoryHandleClass, MemoryId};

use super::get_memory::memory_class;

#[derive(Debug, Default)]
pub struct WalkMemoryLineageTool;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WalkMemoryLineageArgs {
    /// `F:<uuid>`, `A:<uuid>`, or `P:<uuid>` memory id.
    pub memory: String,
    #[serde(default = "default_direction")]
    pub direction: WalkMemoryLineageDirectionArg,
    #[serde(default = "default_depth")]
    pub depth: u8,
    #[serde(default = "default_limit")]
    pub limit: u32,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum WalkMemoryLineageDirectionArg {
    Ancestors,
    Descendants,
}

impl From<WalkMemoryLineageDirectionArg> for MemoryLineageDirection {
    fn from(value: WalkMemoryLineageDirectionArg) -> Self {
        match value {
            WalkMemoryLineageDirectionArg::Ancestors => Self::Ancestors,
            WalkMemoryLineageDirectionArg::Descendants => Self::Descendants,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct WalkMemoryLineageOutput {
    pub start: String,
    pub direction: String,
    pub nodes: Vec<LineageNodeOutput>,
    pub edges: Vec<LineageEdgeOutput>,
    pub truncated: bool,
}

#[derive(Debug, Serialize)]
pub struct LineageNodeOutput {
    pub memory: String,
    pub kind: String,
    pub schema_id: String,
    pub snippet: String,
    pub wake_chain_depth: u16,
    pub distance: u8,
}

#[derive(Debug, Serialize)]
pub struct LineageEdgeOutput {
    pub edge: String,
    pub relation: String,
    pub relation_class: String,
    pub source: String,
    pub target: String,
    pub distance: u8,
}

fn default_direction() -> WalkMemoryLineageDirectionArg {
    WalkMemoryLineageDirectionArg::Ancestors
}

fn default_depth() -> u8 {
    3
}

fn default_limit() -> u32 {
    50
}

impl McpTool for WalkMemoryLineageTool {
    const NAME: &'static str = "core/walk_memory_lineage";
    const DESCRIPTION: &'static str =
        "Walk owner-scoped Provenance/Supersession memory lineage from a prefixed memory id.";
    type Args = WalkMemoryLineageArgs;
    type Output = WalkMemoryLineageOutput;

    fn call(
        ctx: McpToolCtx,
        args: WalkMemoryLineageArgs,
    ) -> BoxFuture<'static, Result<WalkMemoryLineageOutput, McpToolError>> {
        Box::pin(async move {
            let start = ctx.resolve_memory(&args.memory)?;
            let direction = MemoryLineageDirection::from(args.direction);
            let storage = ctx
                .storage()
                .ok_or_else(|| McpToolError::Other("engine storage unavailable".into()))?;
            let response = storage
                .walk_memory_lineage(&MemoryLineageRequest {
                    principal: ctx.owner.clone(),
                    start_memory_id: start,
                    direction,
                    depth: args.depth.clamp(1, 8),
                    limit: args.limit.clamp(1, 200),
                    reader_personality_instance_id: None,
                })
                .await?;

            let mut classes = HashMap::new();
            let nodes = response
                .nodes
                .into_iter()
                .map(|node| {
                    let kind = format!("{:?}", node.kind);
                    let class = memory_class(&kind)?;
                    classes.insert(node.memory_id, class);
                    Ok(LineageNodeOutput {
                        memory: ctx.format_memory_with_class(node.memory_id, class),
                        kind,
                        schema_id: node.schema_id.as_str().to_string(),
                        snippet: node.snippet,
                        wake_chain_depth: node.wake_chain_depth.into_inner(),
                        distance: node.distance,
                    })
                })
                .collect::<Result<Vec<_>, McpToolError>>()?;

            let edges = response
                .edges
                .into_iter()
                .map(|edge| LineageEdgeOutput {
                    edge: ctx.format_edge(EdgeId::new(edge.edge_id)),
                    relation: edge.relation,
                    relation_class: edge.relation_class,
                    source: format_lineage_memory(&ctx, &classes, edge.source_memory_id),
                    target: format_lineage_memory(&ctx, &classes, edge.target_memory_id),
                    distance: edge.distance,
                })
                .collect();

            Ok(WalkMemoryLineageOutput {
                start: args.memory,
                direction: format!("{direction:?}").to_lowercase(),
                nodes,
                edges,
                truncated: response.truncated,
            })
        })
    }
}

fn format_lineage_memory(
    ctx: &McpToolCtx,
    classes: &HashMap<MemoryId, MemoryHandleClass>,
    memory_id: MemoryId,
) -> String {
    let class = classes
        .get(&memory_id)
        .copied()
        .unwrap_or(MemoryHandleClass::Fact);
    ctx.format_memory_with_class(memory_id, class)
}

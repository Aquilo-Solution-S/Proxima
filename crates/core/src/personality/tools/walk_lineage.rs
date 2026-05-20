//! `core/walk_lineage` substrate tool — walk memory-only
//! Provenance/Supersession lineage from a starting memory.

use std::collections::HashMap;
use std::sync::OnceLock;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::MemoryId;
use crate::error::ProtocolError;
use crate::mcp::MemoryHandleClass;
use crate::personality::{PersonalityTool, PersonalityToolContext, PersonalityToolResult};
use crate::verbs::query::{MemoryLineageDirection, MemoryLineageRequest};

#[derive(Debug, Default)]
pub struct WalkLineageTool;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WalkLineageArgs {
    /// Handle of the memory to start walking from (e.g., `F1`, `A1`, or `P1`).
    #[schemars(
        description = "`F...`, `A...`, or `P...` memory handle to start lineage walking from."
    )]
    pub memory: String,
    #[serde(default = "default_direction")]
    #[schemars(
        description = "Lineage direction: ancestors or descendants. Defaults to ancestors."
    )]
    pub direction: WalkLineageDirectionArg,
    #[serde(default = "default_depth")]
    #[schemars(description = "Maximum lineage depth. Defaults to 3; values are clamped to 1..=8.")]
    pub depth: u8,
    #[serde(default = "default_limit")]
    #[schemars(
        description = "Maximum number of lineage nodes to return. Defaults to 50; values are clamped to 1..=200."
    )]
    pub limit: u32,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum WalkLineageDirectionArg {
    Ancestors,
    Descendants,
}

impl From<WalkLineageDirectionArg> for MemoryLineageDirection {
    fn from(value: WalkLineageDirectionArg) -> Self {
        match value {
            WalkLineageDirectionArg::Ancestors => Self::Ancestors,
            WalkLineageDirectionArg::Descendants => Self::Descendants,
        }
    }
}

fn default_direction() -> WalkLineageDirectionArg {
    WalkLineageDirectionArg::Ancestors
}

fn default_depth() -> u8 {
    3
}

fn default_limit() -> u32 {
    50
}

fn args_schema_value() -> &'static serde_json::Value {
    static SCHEMA: OnceLock<serde_json::Value> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        serde_json::to_value(schemars::schema_for!(WalkLineageArgs))
            .expect("WalkLineageArgs schema serializes")
    })
}

#[async_trait]
impl PersonalityTool for WalkLineageTool {
    fn tool_id(&self) -> &'static str {
        "core/walk_lineage"
    }

    fn description(&self) -> &'static str {
        "Walk memory-only Provenance and Supersession lineage from a \
         memory. Returns memory handles, edge endpoints, distances, and \
         snippets."
    }

    fn args_schema(&self) -> serde_json::Value {
        args_schema_value().clone()
    }

    async fn invoke(
        &self,
        ctx: &PersonalityToolContext<'_>,
        args: serde_json::Value,
    ) -> Result<PersonalityToolResult, ProtocolError> {
        let parsed: WalkLineageArgs = match serde_json::from_value(args) {
            Ok(v) => v,
            Err(e) => {
                return Ok(PersonalityToolResult::error(serde_json::json!({
                    "error": format!("invalid args: {e}"),
                })));
            }
        };
        let start = match ctx.handles.resolve_memory(&parsed.memory) {
            Ok(id) => id,
            Err(e) => {
                return Ok(PersonalityToolResult::error(serde_json::json!({
                    "error": e.to_string(),
                })));
            }
        };

        let direction = MemoryLineageDirection::from(parsed.direction);
        let req = MemoryLineageRequest {
            owner: ctx.owner.clone(),
            start_memory_id: start,
            direction,
            depth: parsed.depth.clamp(1, 8),
            limit: parsed.limit.clamp(1, 200),
            reader_personality_instance_id: Some(ctx.instance_id),
        };
        let response = ctx
            .engine
            .storage()
            .walk_memory_lineage(&req)
            .await
            .map_err(|e| ProtocolError::internal(format!("walk_memory_lineage: {e}")))?;

        ctx.record_read(
            response
                .nodes
                .iter()
                .map(|node| (node.memory_id, node.wake_chain_depth)),
        )
        .await;

        let mut node_classes = HashMap::new();
        for node in &response.nodes {
            if let Some(class) = MemoryHandleClass::from_memory_kind(&format!("{:?}", node.kind)) {
                node_classes.insert(node.memory_id, class);
            }
        }

        let nodes: Vec<_> = response
            .nodes
            .into_iter()
            .map(|node| {
                let handle = ctx
                    .handles
                    .assign_memory_kind(node.memory_id, &format!("{:?}", node.kind));
                serde_json::json!({
                    "memory": handle.as_str(),
                    "kind": format!("{:?}", node.kind),
                    "schema_id": node.schema_id.as_str(),
                    "snippet": node.snippet,
                    "wake_chain_depth": node.wake_chain_depth.into_inner(),
                    "distance": node.distance,
                })
            })
            .collect();
        let edges: Vec<_> = response
            .edges
            .into_iter()
            .map(|edge| {
                let handle = ctx.handles.assign_edge(crate::EdgeId::new(edge.edge_id));
                let source = lineage_memory_handle(ctx, &node_classes, edge.source_memory_id);
                let target = lineage_memory_handle(ctx, &node_classes, edge.target_memory_id);
                serde_json::json!({
                    "edge": handle.as_str(),
                    "relation": edge.relation,
                    "relation_class": edge.relation_class,
                    "source": source.as_str(),
                    "target": target.as_str(),
                    "distance": edge.distance,
                })
            })
            .collect();

        Ok(PersonalityToolResult::ok(serde_json::json!({
            "start": parsed.memory,
            "direction": format!("{:?}", direction).to_lowercase(),
            "nodes": nodes,
            "edges": edges,
            "truncated": response.truncated,
        })))
    }
}

fn lineage_memory_handle(
    ctx: &PersonalityToolContext<'_>,
    node_classes: &HashMap<MemoryId, MemoryHandleClass>,
    memory_id: MemoryId,
) -> crate::mcp::Handle {
    if let Some(handle) = ctx.handles.memory_handle(memory_id) {
        return handle;
    }
    let class = node_classes
        .get(&memory_id)
        .copied()
        .unwrap_or(MemoryHandleClass::Fact);
    ctx.handles.assign_memory_with_class(memory_id, class)
}

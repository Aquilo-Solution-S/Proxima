//! `core/walk_lineage` substrate tool — walk memory-only
//! Provenance/Supersession lineage from a starting memory.

use std::sync::OnceLock;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::error::ProtocolError;
use crate::personality::{PersonalityTool, PersonalityToolContext, PersonalityToolResult};
use crate::verbs::query::{MemoryLineageDirection, MemoryLineageRequest};

#[derive(Debug, Default)]
pub struct WalkLineageTool;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WalkLineageArgs {
    /// Handle of the memory to start walking from (e.g., `N1`).
    pub memory: String,
    #[serde(default = "default_direction")]
    pub direction: WalkLineageDirectionArg,
    #[serde(default = "default_depth")]
    pub depth: u8,
    #[serde(default = "default_limit")]
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

        let nodes: Vec<_> = response
            .nodes
            .into_iter()
            .map(|node| {
                let handle = ctx.handles.assign_memory(node.memory_id);
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
                let source = ctx.handles.assign_memory(edge.source_memory_id);
                let target = ctx.handles.assign_memory(edge.target_memory_id);
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

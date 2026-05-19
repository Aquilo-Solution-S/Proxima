//! `core/get_graph` — single-shot read of the owner's full personality
//! graph plus the static catalogs that wake-entry config references.
//!
//! Composes the data that would otherwise require eight round trips
//! (list_personalities + get_personality per P, list_schemas,
//! list_edge_types, list_substrate_tools, list_workspace_tools,
//! list_inference_targets, list_inference_tier_bindings) into one atomic
//! response. The personality projection mirrors `get_personality` and
//! the catalog projections mirror their respective `list_*` tools so the
//! shapes already familiar to the frontend stay intact.

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::McpTool;
use crate::mcp::{McpToolCtx, McpToolError};
use crate::verbs::schema::PayloadKind;

use super::get_personality::{GetPersonalityOutput, GetPersonalityWakeEntry};
use super::list_edge_types::EdgeTypeItem;
use super::list_inference_targets::InferenceTargetItem;
use super::list_inference_tier_bindings::InferenceTierBindingItem;
use super::list_schemas::SchemaItem;
use super::list_substrate_tools::SubstrateToolItem;
use super::list_workspace_tools::WorkspaceToolItem;

#[derive(Debug, Default)]
pub struct GetGraphTool;

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct GetGraphArgs {
    /// Include tombstoned personalities. Default: false.
    #[serde(default)]
    pub include_tombstoned: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct GetGraphOutput {
    /// Every personality the owner owns, fully expanded — same shape as
    /// `core/get_personality` output.
    pub personalities: Vec<GetPersonalityOutput>,
    /// Static schema catalog from the frozen FlavorRegistry.
    pub schemas: Vec<SchemaItem>,
    /// Static edge-type catalog from the frozen FlavorRegistry.
    pub edge_types: Vec<EdgeTypeItem>,
    /// Substrate-pack and flavor-registered MCP tool ids.
    pub substrate_tools: Vec<SubstrateToolItem>,
    /// Workspace tool catalog (palette ids for workspace-mode wake entries).
    pub workspace_tools: Vec<WorkspaceToolItem>,
    /// Inference targets registered for this owner.
    pub inference_targets: Vec<InferenceTargetItem>,
    /// Tier→target bindings for this owner.
    pub inference_tier_bindings: Vec<InferenceTierBindingItem>,
}

fn kind_str(k: PayloadKind) -> &'static str {
    match k {
        PayloadKind::Fact => "Fact",
        PayloadKind::Abstraction => "Abstraction",
        PayloadKind::Perspective => "Perspective",
        PayloadKind::Goal => "Goal",
        PayloadKind::Edge => "Edge",
        PayloadKind::CitedObject => "CitedObject",
        PayloadKind::CitationMapping => "CitationMapping",
    }
}

impl McpTool for GetGraphTool {
    const NAME: &'static str = "core/get_graph";
    const DESCRIPTION: &'static str = "Single-shot read of the owner's full personality graph plus the catalogs that wake-entry \
         config references (schemas, edge types, substrate tools, workspace tools, inference \
         targets, tier bindings). Use this in place of eight separate list_/get_ round trips when \
         rendering a graph view. Args: `{\"include_tombstoned\": false}` (default).";
    type Args = GetGraphArgs;
    type Output = GetGraphOutput;

    fn call(
        ctx: McpToolCtx,
        args: GetGraphArgs,
    ) -> BoxFuture<'static, Result<GetGraphOutput, McpToolError>> {
        Box::pin(async move {
            let storage = ctx
                .storage()
                .ok_or_else(|| McpToolError::Other("engine storage unavailable".into()))?;

            let personality_rows = storage
                .list_personality_instances(&ctx.owner, args.include_tombstoned)
                .await
                .map_err(McpToolError::Storage)?;
            let personalities = personality_rows
                .into_iter()
                .map(|row| {
                    let personality = ctx.format_personality(row.personality_instance_id);
                    let root_perspective =
                        ctx.format_memory(row.current_root_perspective_memory_id);
                    let wake_entries = row
                        .wake_entries
                        .into_iter()
                        .map(|e| GetPersonalityWakeEntry {
                            wake_entry: ctx.format_wake_entry(e.wake_entry_id),
                            trigger_kind: format!("{:?}", e.trigger_kind),
                            trigger_id: e.trigger_id,
                            label: e.label,
                            enabled: e.enabled,
                            instructions: e.instructions,
                            model_tier: format!("{:?}", e.model_tier),
                            inference_target_ref: e.inference_target_ref,
                            substrate_tool_palette: e.substrate_tool_palette,
                            workspace_tool_palette: e.workspace_tool_palette,
                            execution_mode: format!("{:?}", e.execution_mode),
                            authored_by: format!("{:?}", e.authored_by),
                            probability_promille: e.probability_promille,
                            goal_scope: e.goal_scope.as_str().to_string(),
                            max_rounds: e.max_rounds,
                            intervention_policy: e.intervention_policy,
                            disabled_reason: e.disabled_reason,
                        })
                        .collect();
                    GetPersonalityOutput {
                        personality,
                        display_name: row.display_name,
                        status: row.status,
                        root_perspective,
                        wake_entries,
                    }
                })
                .collect();

            let schemas = ctx
                .registry
                .list()
                .into_iter()
                .map(|info| SchemaItem {
                    schema_id: info.schema_id.as_str().to_string(),
                    schema_version: info.schema_version.into_inner(),
                    kind: kind_str(info.kind).to_string(),
                })
                .collect();

            let edge_types = ctx
                .registry
                .list_relations()
                .iter()
                .map(|rel| EdgeTypeItem {
                    edge_type: rel.relation.clone(),
                    class: rel.class.as_str().to_string(),
                })
                .collect();

            let mut substrate_tools = Vec::new();
            for tool in crate::personality::substrate_pack() {
                substrate_tools.push(SubstrateToolItem {
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
                substrate_tools.push(SubstrateToolItem {
                    tool_id: desc.name.to_string(),
                    source,
                    description: desc.description.to_string(),
                });
            }

            let workspace_tools = crate::personality::WORKSPACE_TOOL_CATALOG
                .iter()
                .map(|(id, desc)| WorkspaceToolItem {
                    tool_id: (*id).to_string(),
                    description: (*desc).to_string(),
                })
                .collect();

            let target_rows = storage
                .list_inference_targets(&ctx.owner)
                .await
                .map_err(McpToolError::Storage)?;
            let inference_targets = target_rows
                .into_iter()
                .map(|row| InferenceTargetItem {
                    target_ref: row.target_ref,
                    config: serde_json::to_value(&row.config).unwrap_or(serde_json::Value::Null),
                })
                .collect();

            let binding_rows = storage
                .list_inference_tier_bindings(&ctx.owner)
                .await
                .map_err(McpToolError::Storage)?;
            let inference_tier_bindings = binding_rows
                .into_iter()
                .map(|row| InferenceTierBindingItem {
                    tier: format!("{:?}", row.tier),
                    target_ref: row.target_ref,
                })
                .collect();

            Ok(GetGraphOutput {
                personalities,
                schemas,
                edge_types,
                substrate_tools,
                workspace_tools,
                inference_targets,
                inference_tier_bindings,
            })
        })
    }
}

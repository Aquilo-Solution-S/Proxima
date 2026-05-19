//! Generated per-invocation wake contract.
//!
//! This is derived from the stored `WakeEntryRow` at fire time. It is
//! prompt context only; it does not alter tool schemas or provider
//! strictness.

use serde::Serialize;

use crate::mcp::provider_safe_tool_name;
use crate::personality::{WORKSPACE_TOOL_CATALOG, WakeEntryRow, substrate_pack};
use crate::verbs::schema::FlavorRegistryFrozen;

#[derive(Debug, Clone, Serialize)]
pub struct WakeContract {
    pub wake_entry_id: uuid::Uuid,
    pub label: String,
    pub trigger_kind: String,
    pub trigger_id: String,
    pub trigger_schema_id: String,
    pub execution_mode: String,
    pub authored_by: String,
    pub goal_scope: String,
    pub max_rounds: u16,
    pub tool_palettes: WakeContractToolPalettes,
    pub resolved_tools: WakeContractResolvedTools,
}

#[derive(Debug, Clone, Serialize)]
pub struct WakeContractToolPalettes {
    pub substrate_tool_palette: Vec<String>,
    pub workspace_tool_palette: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WakeContractResolvedTools {
    pub substrate: Vec<WakeContractTool>,
    pub workspace: Vec<WakeContractTool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WakeContractTool {
    pub palette_id: String,
    pub canonical_name: String,
    pub provider_name: String,
    pub description: String,
}

#[must_use]
pub fn build_wake_contract(
    registry: &FlavorRegistryFrozen,
    wake_entry: &WakeEntryRow,
) -> WakeContract {
    WakeContract {
        wake_entry_id: wake_entry.wake_entry_id,
        label: wake_entry.label.clone(),
        trigger_kind: wake_entry.trigger_kind.as_str().to_string(),
        trigger_id: wake_entry.trigger_id.clone(),
        trigger_schema_id: wake_entry.trigger_id.clone(),
        execution_mode: wake_entry.execution_mode.as_str().to_string(),
        authored_by: wake_entry.authored_by.as_str().to_string(),
        goal_scope: wake_entry.goal_scope.as_str().to_string(),
        max_rounds: wake_entry.max_rounds,
        tool_palettes: WakeContractToolPalettes {
            substrate_tool_palette: wake_entry.substrate_tool_palette.clone(),
            workspace_tool_palette: wake_entry.workspace_tool_palette.clone(),
        },
        resolved_tools: WakeContractResolvedTools {
            substrate: resolve_substrate_tools(registry, &wake_entry.substrate_tool_palette),
            workspace: resolve_workspace_tools(&wake_entry.workspace_tool_palette),
        },
    }
}

fn resolve_substrate_tools(
    registry: &FlavorRegistryFrozen,
    palette: &[String],
) -> Vec<WakeContractTool> {
    palette
        .iter()
        .map(|tool_id| {
            let description = substrate_pack()
                .iter()
                .find(|tool| tool.tool_id() == tool_id)
                .map(|tool| tool.description().to_string())
                .or_else(|| {
                    registry
                        .list_mcp_tools()
                        .iter()
                        .find(|tool| tool.name == tool_id)
                        .map(|tool| tool.description.to_string())
                })
                .unwrap_or_default();
            WakeContractTool {
                palette_id: tool_id.clone(),
                canonical_name: tool_id.clone(),
                provider_name: provider_safe_tool_name(tool_id),
                description,
            }
        })
        .collect()
}

fn resolve_workspace_tools(palette: &[String]) -> Vec<WakeContractTool> {
    palette
        .iter()
        .map(|tool_id| {
            let canonical_name = workspace_provider_tool_name(tool_id)
                .unwrap_or(tool_id.as_str())
                .to_string();
            let description = WORKSPACE_TOOL_CATALOG
                .iter()
                .find(|(id, _)| id == tool_id)
                .map(|(_, description)| (*description).to_string())
                .unwrap_or_default();
            WakeContractTool {
                palette_id: tool_id.clone(),
                provider_name: provider_safe_tool_name(&canonical_name),
                canonical_name,
                description,
            }
        })
        .collect()
}

fn workspace_provider_tool_name(tool_id: &str) -> Option<&'static str> {
    match tool_id {
        "proxima-workspace/shell" => Some("workspace_shell"),
        "proxima-workspace/text_editor" => Some("workspace_text_editor"),
        "proxima-workspace/list_files" => Some("workspace_list_files"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use crate::{
        FlavorRegistry, ModelTier, WakeEntryAuthoredBy, WakeEntryExecutionMode, WakeEntryGoalScope,
        WakeEntryRow, WakeEntryTriggerKind,
    };

    use super::build_wake_contract;

    #[test]
    fn contract_reflects_wake_entry_and_tool_palettes() {
        let wake_entry_id = Uuid::now_v7();
        let wake_entry = WakeEntryRow {
            wake_entry_id,
            trigger_kind: WakeEntryTriggerKind::OnMemory,
            trigger_id: "proxima-test/fact-v1".into(),
            label: "Planner child-goal demo wake".into(),
            enabled: true,
            execution_mode: WakeEntryExecutionMode::Workspace,
            authored_by: WakeEntryAuthoredBy::Other,
            probability_promille: 1000,
            goal_scope: WakeEntryGoalScope::TriggerGoalAssigned,
            instructions: "temporary compatibility text".into(),
            model_tier: ModelTier::Standard,
            inference_target_ref: None,
            substrate_tool_palette: vec!["core/fetch_memory".into()],
            workspace_tool_palette: vec!["proxima-workspace/shell".into()],
            max_rounds: 4,
            intervention_policy: None,
            disabled_reason: None,
        };

        let contract = build_wake_contract(&FlavorRegistry::new().freeze(), &wake_entry);

        assert_eq!(contract.wake_entry_id, wake_entry_id);
        assert_eq!(contract.label, "Planner child-goal demo wake");
        assert_eq!(contract.trigger_id, "proxima-test/fact-v1");
        assert_eq!(contract.trigger_schema_id, "proxima-test/fact-v1");
        assert_eq!(contract.execution_mode, "workspace");
        assert_eq!(contract.authored_by, "other");
        assert_eq!(contract.goal_scope, "trigger_goal_assigned");
        assert_eq!(contract.max_rounds, 4);
        assert_eq!(
            contract.tool_palettes.substrate_tool_palette,
            vec!["core/fetch_memory"]
        );
        assert_eq!(
            contract.tool_palettes.workspace_tool_palette,
            vec!["proxima-workspace/shell"]
        );
        assert_eq!(
            contract.resolved_tools.substrate[0].provider_name,
            "core_fetch_memory"
        );
        assert_eq!(
            contract.resolved_tools.workspace[0].canonical_name,
            "workspace_shell"
        );
    }
}

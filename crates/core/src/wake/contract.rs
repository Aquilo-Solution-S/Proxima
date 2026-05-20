//! Generated per-invocation wake contract.
//!
//! This is derived from the stored `WakeEntryRow` at fire time. It is
//! prompt context only; the provider-visible substrate tools are rendered from
//! the same projection handed to the harness.

use serde::Serialize;

use crate::harness::HarnessToolProjection;
use crate::mcp::{HandleTable, provider_safe_tool_name};
use crate::personality::{WORKSPACE_TOOL_CATALOG, WakeEntryRow};

#[derive(Debug, Clone, Serialize)]
pub struct WakeContract {
    pub wake_entry: String,
    pub label: String,
    pub trigger_kind: String,
    pub trigger_id: String,
    pub trigger_schema_id: String,
    pub execution_mode: String,
    pub authored_by: String,
    pub goal_scope: String,
    pub max_rounds: u16,
    pub handle_domains: WakeContractHandleDomains,
    pub tool_palettes: WakeContractToolPalettes,
    pub resolved_tools: WakeContractResolvedTools,
}

#[derive(Debug, Clone, Serialize)]
pub struct WakeContractHandleDomains {
    pub memory: String,
    pub goal: String,
    pub personality: String,
    pub edge: String,
    pub wake_entry: String,
    pub flavor_object: String,
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
    wake_entry: &WakeEntryRow,
    tool_projection: &[HarnessToolProjection],
    handles: &HandleTable,
) -> WakeContract {
    WakeContract {
        wake_entry: handles
            .assign_wake_entry(wake_entry.wake_entry_id)
            .as_str()
            .to_string(),
        label: wake_entry.label.clone(),
        trigger_kind: wake_entry.trigger_kind.as_str().to_string(),
        trigger_id: wake_entry.trigger_id.clone(),
        trigger_schema_id: wake_entry.trigger_id.clone(),
        execution_mode: wake_entry.execution_mode.as_str().to_string(),
        authored_by: wake_entry.authored_by.as_str().to_string(),
        goal_scope: wake_entry.goal_scope.as_str().to_string(),
        max_rounds: wake_entry.max_rounds,
        handle_domains: WakeContractHandleDomains {
            memory: "F*: Fact memory, A*: Abstraction memory, P*: Perspective memory; use the class required by each memory argument"
                .to_string(),
            goal: "G*: Goal handle; use in goal arguments".to_string(),
            personality: "I*: Personality handle; use in target_personality/personality arguments"
                .to_string(),
            edge: "E*: Edge handle; use in edge arguments".to_string(),
            wake_entry: "W*: Wake-entry handle; use in wake-entry arguments".to_string(),
            flavor_object:
                "Other uppercase prefixes are flavor object handles such as repo handles"
                    .to_string(),
        },
        tool_palettes: WakeContractToolPalettes {
            substrate_tool_palette: wake_entry.substrate_tool_palette.clone(),
            workspace_tool_palette: wake_entry.workspace_tool_palette.clone(),
        },
        resolved_tools: WakeContractResolvedTools {
            substrate: resolve_substrate_tools(tool_projection),
            workspace: resolve_workspace_tools(&wake_entry.workspace_tool_palette),
        },
    }
}

fn resolve_substrate_tools(tool_projection: &[HarnessToolProjection]) -> Vec<WakeContractTool> {
    tool_projection
        .iter()
        .map(|tool| WakeContractTool {
            palette_id: tool.palette_id.clone(),
            canonical_name: tool.canonical_name.clone(),
            provider_name: tool.provider_name.clone(),
            description: tool.description.clone(),
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
        ModelTier, WakeEntryAuthoredBy, WakeEntryExecutionMode, WakeEntryGoalScope, WakeEntryRow,
        WakeEntryTriggerKind,
        harness::{HarnessToolDispatch, HarnessToolProjection},
        mcp::HandleTable,
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

        let projection = vec![HarnessToolProjection {
            palette_id: "core/fetch_memory".into(),
            canonical_name: "core/fetch_memory".into(),
            provider_name: "core_fetch_memory".into(),
            description: "Fetch a memory".into(),
            input_schema: serde_json::json!({ "type": "object", "properties": {} }),
            dispatch: HarnessToolDispatch::DirectSubstrate {
                internal_canonical_name: "core/fetch_memory".into(),
            },
        }];

        let handles = HandleTable::new();
        let contract = build_wake_contract(&wake_entry, &projection, &handles);

        assert_eq!(contract.wake_entry, "W1");
        assert_eq!(
            handles
                .resolve_wake_entry(&contract.wake_entry)
                .expect("wake handle"),
            wake_entry_id
        );
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
        assert!(contract.handle_domains.personality.contains("I*"));
    }
}

//! `HarnessProgram` -> provider conversation + tool name maps.

use std::collections::HashMap;
use std::fmt::Write as _;

use proxima_core::harness::{HarnessProgram, SubstrateToolBinding};
use proxima_core::mcp::provider_safe_tool_name;

use crate::conversation::{Conversation, ToolSpec};
use crate::tools::{ToolBinding, workspace::WorkspaceToolName};

#[derive(Debug)]
pub struct ResolvedProgram {
    pub conversation: Conversation,
    pub tools: Vec<ToolSpec>,
    /// Provider-safe name -> canonical name. The loop driver uses
    /// this to resolve `function.name` values from provider responses.
    pub reverse_map: HashMap<String, String>,
    /// Canonical name -> dispatch binding.
    pub bindings: HashMap<String, ToolBinding>,
}

#[must_use]
pub fn resolve(
    program: HarnessProgram,
    substrate_tools: &[SubstrateToolBinding],
) -> ResolvedProgram {
    let user_seed = build_user_seed(&program);
    let mut tools = Vec::with_capacity(substrate_tools.len() + 3);
    let mut reverse_map = HashMap::new();
    let mut bindings = HashMap::new();

    for substrate in substrate_tools {
        let provider_safe = provider_safe_tool_name(&substrate.canonical_name);
        tools.push(ToolSpec {
            canonical: substrate.canonical_name.clone(),
            provider_safe: provider_safe.clone(),
            description: substrate.description.clone(),
            input_schema: substrate.args_schema.clone(),
        });
        reverse_map.insert(provider_safe, substrate.canonical_name.clone());
        bindings.insert(
            substrate.canonical_name.clone(),
            ToolBinding::Substrate(substrate.clone()),
        );
    }

    if program.workspace_root.is_some() {
        for name in [
            WorkspaceToolName::Shell,
            WorkspaceToolName::TextEditor,
            WorkspaceToolName::ListFiles,
        ] {
            let canonical = name.canonical().to_string();
            let provider_safe = provider_safe_tool_name(&canonical);
            tools.push(ToolSpec {
                canonical: canonical.clone(),
                provider_safe: provider_safe.clone(),
                description: name.description().to_string(),
                input_schema: name.input_schema(),
            });
            reverse_map.insert(provider_safe, canonical.clone());
            bindings.insert(canonical, ToolBinding::Workspace(name));
        }
    }

    ResolvedProgram {
        conversation: Conversation {
            system_prompt: program.system_prompt,
            user_seed,
            turns: Vec::new(),
        },
        tools,
        reverse_map,
        bindings,
    }
}

fn build_user_seed(program: &HarnessProgram) -> String {
    let mut seed = String::new();
    if !program.instructions.is_empty() {
        seed.push_str(&program.instructions);
        seed.push_str("\n\n");
    }

    for key in [
        "root_perspective",
        "active_goals",
        "trigger_event",
        "triggering_memory",
        "wake_contract",
        "coordination_context",
        "continuation",
        "workspace_context",
    ] {
        if let Some(value) = program.context_params.get(key) {
            let _ = write!(
                seed,
                "{}:\n{}\n\n",
                snake_to_title(key),
                serde_json::to_string_pretty(value).unwrap_or_default()
            );
        }
    }

    seed.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use proxima_core::harness::ProviderTarget;
    use serde_json::json;

    use super::build_user_seed;

    #[test]
    fn user_seed_renders_wake_contract_and_coordination_context() {
        let program = proxima_core::harness::HarnessProgram {
            system_prompt: "sys".into(),
            instructions: "do the wake".into(),
            context_params: HashMap::from([
                (
                    "wake_contract".into(),
                    json!({
                        "label": "Planner child-goal demo wake",
                        "trigger_id": "proxima-goal/goal-activated-v1",
                        "execution_mode": "substrate_only",
                        "tool_palettes": {
                            "substrate_tool_palette": ["proxima-goal/goal_decompose"],
                            "workspace_tool_palette": []
                        }
                    }),
                ),
                (
                    "coordination_context".into(),
                    json!({
                        "wake_path": {
                            "current": {
                                "wake_entry_label": "Planner child-goal demo wake"
                            }
                        }
                    }),
                ),
            ]),
            substrate_tool_palette: Vec::new(),
            workspace_root: None,
            max_rounds: 1,
            provider: ProviderTarget::MistralChat {
                base_url: "http://127.0.0.1:1".into(),
                model_id: "test".into(),
                api_key: "test".into(),
                temperature: None,
                max_completion_tokens: None,
            },
        };

        let seed = build_user_seed(&program);

        assert!(seed.contains("Wake Contract:"));
        assert!(seed.contains("Coordination Context:"));
        assert!(seed.contains("Planner child-goal demo wake"));
        assert!(seed.contains("proxima-goal/goal_decompose"));
    }
}

fn snake_to_title(s: &str) -> String {
    s.split('_')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect::<String>(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

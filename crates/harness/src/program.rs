//! `HarnessProgram` -> provider conversation + tool name maps.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use proxima_core::harness::{HarnessProgram, SubstrateToolBinding};
use proxima_core::mcp::provider_safe_tool_name;
use proxima_core::verbs::schema::PayloadKind;
use serde_json::{Map, Value};

use crate::conversation::{Conversation, ToolSpec};
use crate::tools::{ToolBinding, workspace::WorkspaceToolName};

const EMIT_ABSTRACTION: &str = "core/emit_abstraction";
const EMIT_PERSPECTIVE: &str = "core/emit_perspective";

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
        let wrappers = typed_emit_wrappers(&program, substrate);
        if !wrappers.is_empty() {
            for wrapper in wrappers {
                tools.push(ToolSpec {
                    canonical: wrapper.canonical.clone(),
                    provider_safe: wrapper.provider_safe.clone(),
                    description: wrapper.description,
                    input_schema: wrapper.input_schema,
                });
                reverse_map.insert(wrapper.provider_safe, wrapper.canonical.clone());
                bindings.insert(
                    wrapper.canonical,
                    ToolBinding::TypedEmit {
                        internal: substrate.clone(),
                        schema_id: wrapper.schema_id,
                        schema_version: wrapper.schema_version,
                        kind: wrapper.kind,
                    },
                );
            }
            continue;
        }

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

struct TypedEmitWrapper {
    canonical: String,
    provider_safe: String,
    description: String,
    input_schema: Value,
    schema_id: String,
    schema_version: u32,
    kind: PayloadKind,
}

fn typed_emit_wrappers(
    program: &HarnessProgram,
    substrate: &SubstrateToolBinding,
) -> Vec<TypedEmitWrapper> {
    let Some(kind) = typed_emit_kind(&substrate.canonical_name) else {
        return Vec::new();
    };
    let writeable_schema_ids = current_produces_schema_ids(program);
    if writeable_schema_ids.is_empty() {
        return Vec::new();
    }

    emit_schema_branches(&substrate.args_schema)
        .into_iter()
        .filter_map(|branch| {
            let (schema_id, schema_version, payload_schema) = emit_branch_parts(branch)?;
            if !writeable_schema_ids.contains(&schema_id) {
                return None;
            }
            let canonical = format!("{}::{schema_id}", substrate.canonical_name);
            Some(TypedEmitWrapper {
                provider_safe: provider_safe_tool_name(&canonical),
                description: typed_emit_description(kind, &schema_id),
                input_schema: typed_emit_input_schema(&payload_schema, kind, &schema_id),
                canonical,
                schema_id,
                schema_version,
                kind,
            })
        })
        .collect()
}

fn typed_emit_kind(canonical_name: &str) -> Option<PayloadKind> {
    match canonical_name {
        EMIT_ABSTRACTION => Some(PayloadKind::Abstraction),
        EMIT_PERSPECTIVE => Some(PayloadKind::Perspective),
        _ => None,
    }
}

fn current_produces_schema_ids(program: &HarnessProgram) -> HashSet<String> {
    program
        .context_params
        .get("coordination_context")
        .and_then(|value| value.pointer("/wake_path/current/produces_schema_ids"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

fn emit_schema_branches(schema: &Value) -> Vec<&Value> {
    schema
        .get("oneOf")
        .and_then(Value::as_array)
        .map(|branches| branches.iter().collect())
        .unwrap_or_else(|| vec![schema])
}

fn emit_branch_parts(branch: &Value) -> Option<(String, u32, Value)> {
    let properties = branch.get("properties")?.as_object()?;
    let schema_id = string_literal_schema(properties.get("schema_id")?)?;
    let schema_version = integer_literal_schema(properties.get("schema_version")?).unwrap_or(1);
    let payload_schema = properties.get("payload")?.clone();
    Some((schema_id, schema_version, payload_schema))
}

fn string_literal_schema(schema: &Value) -> Option<String> {
    schema
        .get("const")
        .and_then(Value::as_str)
        .or_else(|| {
            schema
                .get("enum")
                .and_then(Value::as_array)
                .and_then(|values| values.first())
                .and_then(Value::as_str)
        })
        .map(str::to_string)
}

fn integer_literal_schema(schema: &Value) -> Option<u32> {
    let value = schema.get("const").and_then(Value::as_u64).or_else(|| {
        schema
            .get("enum")
            .and_then(Value::as_array)
            .and_then(|values| values.first())
            .and_then(Value::as_u64)
    })?;
    u32::try_from(value).ok()
}

fn typed_emit_input_schema(payload_schema: &Value, kind: PayloadKind, schema_id: &str) -> Value {
    let mut root = Map::new();
    root.insert("type".to_string(), Value::String("object".to_string()));
    root.insert(
        "description".to_string(),
        Value::String(typed_emit_description(kind, schema_id)),
    );
    root.insert("additionalProperties".to_string(), Value::Bool(false));

    if let Some(defs) = payload_schema.get("$defs").cloned() {
        root.insert("$defs".to_string(), defs);
    }
    if let Some(definitions) = payload_schema.get("definitions").cloned() {
        root.insert("definitions".to_string(), definitions);
    }

    let mut properties = payload_schema
        .get("properties")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    properties.insert(
        "text".to_string(),
        serde_json::json!({
            "type": ["string", "null"],
            "description": "Optional authored text. Omit or null to derive text from payload."
        }),
    );
    root.insert("properties".to_string(), Value::Object(properties));

    if let Some(required) = payload_schema.get("required").cloned() {
        root.insert("required".to_string(), required);
    }

    Value::Object(root)
}

fn typed_emit_description(kind: PayloadKind, schema_id: &str) -> String {
    let kind = match kind {
        PayloadKind::Abstraction => "Abstraction",
        PayloadKind::Perspective => "Perspective",
        _ => "typed memory",
    };
    format!(
        "Emit one {kind} memory with schema {schema_id}. Provide payload fields directly; schema_id and schema_version are hidden dispatch metadata."
    )
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

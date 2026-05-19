use std::path::PathBuf;

use proxima_core::FlavorRegistry;
use proxima_core::harness::{HarnessProgram, ProviderTarget, SubstrateToolBinding};
use proxima_harness::tools::strict_inventory::{
    assert_all_tools_strict_compatible, assert_inventory_checkpoint_is_current,
    registry_tool_inventory, sorted_rows, workspace_tool_inventory,
};
use proxima_harness::tools::strict_schema::StrictToolSchema;
use serde_json::json;

const COMMAND: &str = "cargo test -p proxima-harness --test tool_strictness_inventory inventory_checkpoint_is_current -- --nocapture";

#[test]
fn inventory_checkpoint_is_current() {
    let rows = collect_inventory();

    assert_inventory_checkpoint_is_current(
        &rows,
        checkpoint_path(),
        "Strict Tool Inventory Checkpoint",
        COMMAND,
    );
}

#[test]
fn all_tools_are_strict_schema_compatible() {
    let rows = collect_inventory();
    assert_all_tools_strict_compatible(&rows);
}

#[test]
fn strict_schema_rejects_unbounded_json_holes() {
    let schema = json!({
        "type": "object",
        "properties": { "payload": true },
        "required": ["payload"]
    });

    assert!(StrictToolSchema::from_schema(&schema).is_err());
}

#[test]
fn strict_schema_requires_closed_objects_and_all_properties_required() {
    let schema = json!({
        "type": "object",
        "properties": {
            "command": { "type": "string" },
            "timeout_ms": { "type": ["integer", "null"] }
        }
    });

    let strict = StrictToolSchema::from_schema(&schema).unwrap();
    assert_eq!(strict.value["additionalProperties"], false);
    assert_eq!(strict.value["required"], json!(["command", "timeout_ms"]));
}

#[test]
fn strict_schema_rejects_non_object_roots() {
    let schema = json!({
        "oneOf": [
            { "type": "object", "properties": { "path": { "type": "string" } } }
        ]
    });

    assert!(StrictToolSchema::from_schema(&schema).is_err());
}

#[test]
fn writable_schema_generates_strict_emit_abstraction_wrapper() {
    let schema_id = "proxima-intent/vision-brief-v1";
    let bindings = vec![SubstrateToolBinding {
        canonical_name: "core/emit_abstraction".into(),
        description: "Emit one Abstraction memory.".into(),
        args_schema: json!({
            "type": "object",
            "oneOf": [{
                "type": "object",
                "additionalProperties": false,
                "required": ["schema_id", "schema_version", "payload"],
                "properties": {
                    "schema_id": { "type": "string", "enum": [schema_id] },
                    "schema_version": { "type": "integer", "enum": [1] },
                    "payload": {
                        "type": "object",
                        "properties": {
                            "goal_id": { "type": "string" },
                            "planner_directive": { "type": "string" }
                        },
                        "required": ["goal_id", "planner_directive"]
                    }
                }
            }]
        }),
    }];
    let resolved = proxima_harness::program::resolve(
        HarnessProgram {
            system_prompt: "sys".into(),
            instructions: "do".into(),
            context_params: std::collections::HashMap::from([(
                "coordination_context".into(),
                json!({
                    "wake_path": {
                        "current": {
                            "produces_schema_ids": [schema_id]
                        }
                    }
                }),
            )]),
            substrate_tool_palette: vec!["core/emit_abstraction".into()],
            workspace_root: None,
            max_rounds: 4,
            provider: ProviderTarget::MistralChat {
                base_url: "http://localhost".into(),
                model_id: "mistral-medium-latest".into(),
                api_key: "test".into(),
                temperature: None,
                max_completion_tokens: None,
            },
        },
        &bindings,
    );

    let wrapper_canonical = "core/emit_abstraction::proxima-intent/vision-brief-v1";
    let wrapper = resolved
        .tools
        .iter()
        .find(|tool| tool.canonical == wrapper_canonical)
        .expect("generated VisionBrief emit wrapper");
    assert!(StrictToolSchema::from_schema(&wrapper.input_schema).is_ok());
    assert!(
        !resolved
            .tools
            .iter()
            .any(|tool| tool.canonical == "core/emit_abstraction")
    );
}

fn checkpoint_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(".plans/checkpoints/2026-05-19-strict-tool-inventory.md")
}

fn collect_inventory() -> Vec<proxima_harness::tools::strict_inventory::ToolInventoryRow> {
    let registry = FlavorRegistry::new().freeze();
    sorted_rows(
        registry_tool_inventory(&registry, "substrate")
            .into_iter()
            .chain(workspace_tool_inventory())
            .collect(),
    )
}

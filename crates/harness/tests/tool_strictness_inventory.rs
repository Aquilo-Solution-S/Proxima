use std::path::PathBuf;

use proxima_core::FlavorRegistry;
use proxima_core::harness::build_wake_tool_projection;
use proxima_core::harness::{
    HarnessProgram, HarnessToolDispatch, HarnessToolProjection, ProviderTarget,
    SubstrateToolBinding,
};
use proxima_core::personality::substrate_pack;
use proxima_harness::tools::strict_inventory::{
    assert_all_tools_strict_compatible, assert_inventory_checkpoint_is_current,
    assert_tool_schemas_have_property_descriptions, registry_tool_inventory, sorted_rows,
    workspace_tool_inventory,
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
fn wake_visible_core_and_workspace_tools_describe_object_properties() {
    let registry = FlavorRegistry::new().freeze();
    let palette = substrate_pack()
        .iter()
        .map(|tool| tool.tool_id().to_string())
        .filter(|tool_id| tool_id != "core/emit_abstraction" && tool_id != "core/emit_perspective")
        .chain(["core/emit_intervention_decision".to_string()])
        .collect::<Vec<_>>();
    let mut schemas = build_wake_tool_projection(&registry, &palette)
        .expect("core substrate projection")
        .into_iter()
        .map(|tool| (tool.canonical_name, tool.input_schema))
        .collect::<Vec<_>>();

    schemas.extend(
        [
            proxima_harness::tools::workspace::WorkspaceToolName::Shell,
            proxima_harness::tools::workspace::WorkspaceToolName::TextEditor,
            proxima_harness::tools::workspace::WorkspaceToolName::ListFiles,
        ]
        .into_iter()
        .map(|tool| (tool.canonical().to_string(), tool.input_schema())),
    );

    assert_tool_schemas_have_property_descriptions(schemas);
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
    let wrapper_canonical = "core/emit_abstraction::proxima-intent/vision-brief-v1::v1";
    let bindings = vec![SubstrateToolBinding {
        canonical_name: "core/emit_abstraction".into(),
        description: "Emit one Abstraction memory.".into(),
        args_schema: json!({
            "type": "object",
            "properties": {}
        }),
    }];
    let resolved = proxima_harness::program::resolve(
        HarnessProgram {
            system_prompt: "sys".into(),
            instructions: "do".into(),
            context_params: std::collections::HashMap::default(),
            tool_projection: vec![HarnessToolProjection {
                palette_id: "core/emit_abstraction".into(),
                canonical_name: wrapper_canonical.into(),
                provider_name: "core_emit_abstraction__proxima-intent_vision-brief-v1__v1".into(),
                description: "Emit one Abstraction memory.".into(),
                produces_schema_ids: vec![schema_id.into()],
                input_schema: json!({
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "goal_id": { "type": "string" },
                        "planner_directive": { "type": "string" },
                        "text": { "type": ["string", "null"] }
                    },
                    "required": ["goal_id", "planner_directive"]
                }),
                dispatch: HarnessToolDispatch::TypedEmit {
                    internal_canonical_name: "core/emit_abstraction".into(),
                    schema_id: schema_id.into(),
                    schema_version: 1,
                    payload_kind: proxima_core::verbs::schema::PayloadKind::Abstraction,
                },
            }],
            required_fulfillment_schema_ids: Vec::new(),
            substrate_tool_palette: vec!["core/emit_abstraction".into()],
            workspace_root: None,
            workspace_tool_palette: Vec::new(),
            max_rounds: 4,
            provider: ProviderTarget::MistralChat {
                base_url: "http://localhost".into(),
                model_id: "mistral-medium-latest".into(),
                api_key: "test".into(),
                temperature: None,
                max_completion_tokens: None,
                reasoning_effort: None,

                context_window_tokens: None,
            },
        },
        &bindings,
    )
    .expect("resolve");

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

#[test]
fn raw_emit_capability_is_not_provider_visible_without_projection() {
    let bindings = vec![SubstrateToolBinding {
        canonical_name: "core/emit_abstraction".into(),
        description: "Emit one Abstraction memory.".into(),
        args_schema: json!({
            "type": "object",
            "properties": {}
        }),
    }];
    let resolved = proxima_harness::program::resolve(
        HarnessProgram {
            system_prompt: "sys".into(),
            instructions: "do".into(),
            context_params: std::collections::HashMap::default(),
            tool_projection: Vec::new(),
            required_fulfillment_schema_ids: Vec::new(),
            substrate_tool_palette: vec!["core/emit_abstraction".into()],
            workspace_root: None,
            workspace_tool_palette: Vec::new(),
            max_rounds: 4,
            provider: ProviderTarget::MistralChat {
                base_url: "http://localhost".into(),
                model_id: "mistral-medium-latest".into(),
                api_key: "test".into(),
                temperature: None,
                max_completion_tokens: None,
                reasoning_effort: None,

                context_window_tokens: None,
            },
        },
        &bindings,
    )
    .expect("resolve");

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

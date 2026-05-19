use std::path::PathBuf;

use proxima_code::mcp::workspace_review::CodeEmitVerificationEvidenceArgs;
use proxima_core::FlavorRegistry;
use proxima_harness::tools::strict_inventory::{
    assert_all_tools_strict_compatible, assert_inventory_checkpoint_is_current,
    assert_tool_schemas_have_property_descriptions, registry_tool_inventory_for_prefix,
};
use serde_json::json;

const COMMAND: &str = "cargo test -p proxima-code --test tool_strictness_inventory inventory_checkpoint_is_current -- --nocapture";

#[test]
fn inventory_checkpoint_is_current() {
    let rows = collect_inventory();
    assert_inventory_checkpoint_is_current(
        &rows,
        checkpoint_path(),
        "Code Flavor Strict Tool Inventory Checkpoint",
        COMMAND,
    );
}

#[test]
fn all_tools_are_strict_schema_compatible() {
    let rows = collect_inventory();
    assert_all_tools_strict_compatible(&rows);
}

#[test]
fn code_wake_visible_tools_describe_object_properties() {
    let mut registry = FlavorRegistry::new();
    proxima_code::register(&mut registry);
    let registry = registry.freeze();

    let schemas = registry
        .list_mcp_tools()
        .iter()
        .filter(|tool| tool.name.starts_with("proxima-code/"))
        .map(|tool| (tool.name.to_string(), tool.args_schema.clone()))
        .collect();

    assert_tool_schemas_have_property_descriptions(schemas);
}

#[test]
fn execution_request_planner_fields_explain_handles_and_empty_evidence() {
    let mut registry = FlavorRegistry::new();
    proxima_code::register(&mut registry);
    let registry = registry.freeze();
    let tool = registry
        .list_mcp_tools()
        .iter()
        .find(|tool| tool.name == "proxima-code/code_emit_execution_request")
        .expect("execution request tool");

    let goal_activation = description_at(&tool.args_schema, "/properties/goal_activated_memory");
    assert!(goal_activation.contains("N"));
    assert!(goal_activation.contains("goal-activated Fact"));

    let evidence = description_at(&tool.args_schema, "/properties/evidence");
    for required in ["Fact", "N", "[]", "never G"] {
        assert!(
            evidence.contains(required),
            "evidence description must contain {required:?}: {evidence}"
        );
    }
}

#[test]
fn verification_evidence_rejects_non_object_artifact_refs() {
    let result = serde_json::from_value::<CodeEmitVerificationEvidenceArgs>(json!({
        "workspace_run_memory": "N1",
        "criterion_key": "static_entrypoint",
        "status": "passed",
        "summary": "index.html exists",
        "artifact_refs": ["index.html"],
        "idempotency_key": "artifact-refs-array"
    }));

    assert!(result.is_err());
}

fn checkpoint_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(".plans/checkpoints/2026-05-19-proxima-code-tool-inventory.md")
}

fn collect_inventory() -> Vec<proxima_harness::tools::strict_inventory::ToolInventoryRow> {
    let mut registry = FlavorRegistry::new();
    proxima_code::register(&mut registry);
    let registry = registry.freeze();

    registry_tool_inventory_for_prefix(&registry, "proxima-code/", "flavor")
}

fn description_at<'a>(schema: &'a serde_json::Value, pointer: &str) -> &'a str {
    schema
        .pointer(pointer)
        .and_then(|value| value.get("description"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
}

use std::path::PathBuf;

use proxima_core::FlavorRegistry;
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

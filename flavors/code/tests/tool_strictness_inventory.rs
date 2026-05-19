use std::path::PathBuf;

use proxima_code::mcp::workspace_review::CodeEmitVerificationEvidenceArgs;
use proxima_core::FlavorRegistry;
use proxima_harness::tools::strict_inventory::{
    assert_all_tools_strict_compatible, assert_inventory_checkpoint_is_current,
    registry_tool_inventory_for_prefix,
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

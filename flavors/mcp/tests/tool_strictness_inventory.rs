use std::path::PathBuf;

use proxima_core::FlavorRegistry;
use proxima_harness::tools::strict_inventory::{
    assert_all_tools_strict_compatible, assert_inventory_checkpoint_is_current,
    registry_tool_inventory_for_prefix,
};

const COMMAND: &str = "cargo test -p proxima-mcp-substrate --test tool_strictness_inventory inventory_checkpoint_is_current -- --nocapture";

#[test]
fn inventory_checkpoint_is_current() {
    let mut registry = FlavorRegistry::new();
    proxima_mcp_substrate::register(&mut registry);
    let registry = registry.freeze();

    let rows = registry_tool_inventory_for_prefix(&registry, "proxima-mcp/", "flavor");
    assert_inventory_checkpoint_is_current(
        &rows,
        checkpoint_path(),
        "MCP Flavor Strict Tool Inventory Checkpoint",
        COMMAND,
    );
}

#[test]
fn all_tools_are_strict_schema_compatible() {
    let mut registry = FlavorRegistry::new();
    proxima_mcp_substrate::register(&mut registry);
    let registry = registry.freeze();

    let rows = registry_tool_inventory_for_prefix(&registry, "proxima-mcp/", "flavor");
    assert_all_tools_strict_compatible(&rows);
}

fn checkpoint_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(".plans/checkpoints/2026-05-19-proxima-mcp-tool-inventory.md")
}

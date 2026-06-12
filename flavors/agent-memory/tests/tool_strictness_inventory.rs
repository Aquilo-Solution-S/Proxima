use std::path::PathBuf;

use proxima_core::FlavorRegistry;
use proxima_harness::tools::strict_inventory::{
    assert_all_tools_strict_compatible, assert_inventory_checkpoint_is_current,
    assert_tool_schemas_have_property_descriptions, registry_tool_inventory_for_prefix,
};

const COMMAND: &str = "cargo test -p proxima-agent-memory --test tool_strictness_inventory inventory_checkpoint_is_current -- --nocapture";

#[test]
fn inventory_checkpoint_is_current() {
    let mut registry = FlavorRegistry::new();
    proxima_agent_memory::register(&mut registry);
    let registry = registry.freeze();

    let rows = registry_tool_inventory_for_prefix(&registry, "proxima-agent-memory/", "flavor");
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
    proxima_agent_memory::register(&mut registry);
    let registry = registry.freeze();

    let rows = registry_tool_inventory_for_prefix(&registry, "proxima-agent-memory/", "flavor");
    assert_all_tools_strict_compatible(&rows);
}

#[test]
fn mcp_wake_visible_tools_describe_object_properties() {
    let mut registry = FlavorRegistry::new();
    proxima_agent_memory::register(&mut registry);
    let registry = registry.freeze();

    let schemas: Vec<_> = registry
        .list_mcp_tools()
        .iter()
        .filter(|tool| tool.name.starts_with("proxima-agent-memory/"))
        .map(|tool| (tool.name.to_string(), tool.args_schema.clone()))
        .collect();

    assert_tool_schemas_have_property_descriptions(&schemas);
}

fn checkpoint_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/checkpoints/tool-inventory.md")
}

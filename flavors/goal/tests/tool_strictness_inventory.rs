use std::path::PathBuf;

use proxima_core::FlavorRegistry;
use proxima_harness::tools::strict_inventory::{
    assert_all_tools_strict_compatible, assert_inventory_checkpoint_is_current,
    assert_tool_schemas_have_property_descriptions, registry_tool_inventory_for_prefix,
};

const COMMAND: &str = "cargo test -p proxima-flavor-goal --test tool_strictness_inventory inventory_checkpoint_is_current -- --nocapture";

#[test]
fn inventory_checkpoint_is_current() {
    let mut registry = FlavorRegistry::new();
    proxima_flavor_goal::register(&mut registry);
    let registry = registry.freeze();

    let rows = registry_tool_inventory_for_prefix(&registry, "proxima-goal/", "flavor");
    assert_inventory_checkpoint_is_current(
        &rows,
        checkpoint_path(),
        "Goal Flavor Strict Tool Inventory Checkpoint",
        COMMAND,
    );
}

#[test]
fn all_tools_are_strict_schema_compatible() {
    let mut registry = FlavorRegistry::new();
    proxima_flavor_goal::register(&mut registry);
    let registry = registry.freeze();

    let rows = registry_tool_inventory_for_prefix(&registry, "proxima-goal/", "flavor");
    assert_all_tools_strict_compatible(&rows);
}

#[test]
fn goal_wake_visible_tools_describe_object_properties() {
    let mut registry = FlavorRegistry::new();
    proxima_flavor_goal::register(&mut registry);
    let registry = registry.freeze();

    let schemas = registry
        .list_mcp_tools()
        .iter()
        .filter(|tool| tool.name.starts_with("proxima-goal/"))
        .map(|tool| (tool.name.to_string(), tool.args_schema.clone()))
        .collect();

    assert_tool_schemas_have_property_descriptions(schemas);
}

#[test]
fn decompose_handle_fields_explain_domains() {
    let mut registry = FlavorRegistry::new();
    proxima_flavor_goal::register(&mut registry);
    let registry = registry.freeze();
    let tool = registry
        .list_mcp_tools()
        .iter()
        .find(|tool| tool.name == "proxima-goal/goal_decompose")
        .expect("goal decompose tool");

    let parent_goal = description_at(&tool.args_schema, "/properties/parent_goal");
    assert!(parent_goal.contains("G"), "{parent_goal}");

    let target_personality = description_at(&tool.args_schema, "/properties/target_personality");
    assert!(target_personality.contains("I"), "{target_personality}");
}

fn checkpoint_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(".plans/checkpoints/2026-05-19-proxima-goal-tool-inventory.md")
}

fn description_at<'a>(schema: &'a serde_json::Value, pointer: &str) -> &'a str {
    schema
        .pointer(pointer)
        .and_then(|value| value.get("description"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
}

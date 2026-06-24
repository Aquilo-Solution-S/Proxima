use proxima_core::flavor::FlavorRegistry;

#[test]
fn core_wake_update_patch_schema_is_object() {
    fn contains_key(value: &serde_json::Value, key: &str) -> bool {
        match value {
            serde_json::Value::Object(map) => {
                map.contains_key(key) || map.values().any(|v| contains_key(v, key))
            }
            serde_json::Value::Array(items) => items.iter().any(|v| contains_key(v, key)),
            _ => false,
        }
    }
    fn property_schema<'a>(
        value: &'a serde_json::Value,
        property: &str,
    ) -> Option<&'a serde_json::Value> {
        match value {
            serde_json::Value::Object(map) => map
                .get("properties")
                .and_then(|properties| properties.get(property))
                .or_else(|| map.values().find_map(|v| property_schema(v, property))),
            serde_json::Value::Array(items) => {
                items.iter().find_map(|v| property_schema(v, property))
            }
            _ => None,
        }
    }
    let frozen = FlavorRegistry::default().freeze();
    let schema = &frozen
        .list_mcp_tools()
        .iter()
        .find(|tool| tool.name == "core_wake")
        .expect("core_wake registered")
        .args_schema;
    assert!(
        !contains_key(schema, "$defs"),
        "core_wake schema must be fully inlined, no $defs: {schema:#}",
    );
    let patch = property_schema(schema, "patch").expect("patch property schema present");
    assert_eq!(
        patch.get("type").and_then(serde_json::Value::as_str),
        Some("object"),
        "patch schema should be exposed as an object, not a string or unresolved ref: {patch:#}",
    );
    assert!(
        patch.get("$ref").is_none(),
        "patch schema must be inline for MCP clients that do not resolve refs: {patch:#}",
    );
    assert!(
        patch.pointer("/properties/probability_promille").is_some(),
        "patch schema should expose WakeEntryPatch fields: {patch:#}",
    );
}

/// Every registered MCP tool's argument schema must be fully inlined —
/// no `$ref`, no `$defs` anywhere — so MCP clients that do not resolve
/// references still render every field.
#[test]
fn all_mcp_tool_arg_schemas_are_ref_and_defs_free() {
    fn contains_key(value: &serde_json::Value, key: &str) -> bool {
        match value {
            serde_json::Value::Object(map) => {
                map.contains_key(key) || map.values().any(|v| contains_key(v, key))
            }
            serde_json::Value::Array(items) => items.iter().any(|v| contains_key(v, key)),
            _ => false,
        }
    }

    let frozen = FlavorRegistry::default().freeze();
    for tool in frozen.list_mcp_tools() {
        assert!(
            !contains_key(&tool.args_schema, "$ref"),
            "tool {} has a $ref in its argument schema: {:#}",
            tool.name,
            tool.args_schema,
        );
        assert!(
            !contains_key(&tool.args_schema, "$defs"),
            "tool {} has a $defs block in its argument schema: {:#}",
            tool.name,
            tool.args_schema,
        );
    }
}

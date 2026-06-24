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

/// Every registered MCP tool's argument schema must be a JSON object schema at
/// the root. MCP clients reject `tools/list` if any `inputSchema` omits the
/// top-level `type: object`, even when inner `oneOf` variants are object-shaped.
#[test]
fn all_mcp_tool_arg_schemas_have_object_root() {
    let frozen = FlavorRegistry::default().freeze();
    for tool in frozen.list_mcp_tools() {
        assert_eq!(
            tool.args_schema
                .get("type")
                .and_then(serde_json::Value::as_str),
            Some("object"),
            "tool {} argument schema must declare top-level type object: {:#}",
            tool.name,
            tool.args_schema,
        );
        assert!(
            tool.args_schema
                .get("properties")
                .is_some_and(serde_json::Value::is_object),
            "tool {} argument schema must expose top-level properties object: {:#}",
            tool.name,
            tool.args_schema,
        );
    }
}

#[test]
fn dispatcher_tool_arg_schemas_keep_variant_constraints() {
    let frozen = FlavorRegistry::default().freeze();
    let dispatcher_tools = [
        (
            "core_goal",
            ["set", "transition", "modify", "mark_achieved", "decompose"].as_slice(),
        ),
        (
            "core_wake",
            ["add", "update", "remove", "set", "list"].as_slice(),
        ),
        (
            "core_personality",
            [
                "instantiate",
                "tombstone",
                "set_read_scope",
                "list",
                "get",
                "list_read_scope",
            ]
            .as_slice(),
        ),
        (
            "core_fact",
            [
                "citation_of_fact",
                "citation_of_entity_head",
                "facts_citing_object",
                "tombstone",
            ]
            .as_slice(),
        ),
    ];
    for (tool_name, expected_actions) in dispatcher_tools {
        let schema = &frozen
            .list_mcp_tools()
            .iter()
            .find(|tool| tool.name == tool_name)
            .unwrap_or_else(|| panic!("{tool_name} registered"))
            .args_schema;
        let one_of = schema
            .get("oneOf")
            .and_then(serde_json::Value::as_array)
            .unwrap_or_else(|| panic!("dispatcher tool {tool_name} must keep oneOf: {schema:#}"));
        assert_eq!(
            one_of.len(),
            expected_actions.len(),
            "dispatcher tool {tool_name} must keep one action variant per supported action: {schema:#}",
        );
        let mut actions = Vec::new();
        for variant in one_of {
            assert_eq!(
                variant.get("type").and_then(serde_json::Value::as_str),
                Some("object"),
                "dispatcher tool {tool_name} variant must remain an object schema: {variant:#}",
            );
            assert!(
                variant
                    .get("properties")
                    .is_some_and(serde_json::Value::is_object),
                "dispatcher tool {tool_name} variant must expose properties: {variant:#}",
            );
            let action = variant
                .pointer("/properties/action/const")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_else(|| {
                    panic!(
                        "dispatcher tool {tool_name} variant must preserve action const: {variant:#}"
                    )
                });
            actions.push(action);
        }
        actions.sort_unstable();
        let mut expected = expected_actions.to_vec();
        expected.sort_unstable();
        assert_eq!(
            actions, expected,
            "dispatcher tool {tool_name} must preserve expected action variants",
        );
    }
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

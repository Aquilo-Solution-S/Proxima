use proxima_core::flavor::FlavorRegistry;
use proxima_core::verbs::schema::PayloadKind;

#[test]
fn update_wake_entry_patch_schema_is_object() {
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
    let schema = &frozen
        .list_mcp_tools()
        .iter()
        .find(|tool| tool.name == "core/update_wake_entry")
        .expect("core/update_wake_entry registered")
        .args_schema;
    assert!(
        !contains_key(schema, "$defs"),
        "update_wake_entry schema must be fully inlined, no $defs: {schema:#}",
    );
    let patch = schema
        .pointer("/properties/patch")
        .expect("patch property schema present");
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
        patch
            .pointer("/properties/substrate_tool_palette")
            .is_some(),
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

#[test]
fn wake_trace_schemas_are_registered_in_core_flavor() {
    let frozen = FlavorRegistry::default().freeze();
    let schemas = frozen.list();

    let has = |id: &str, kind: PayloadKind| {
        schemas
            .iter()
            .any(|s| s.schema_id.as_str() == id && s.kind == kind)
    };

    assert!(has("proxima-core/wake-trace-v1", PayloadKind::Fact));
    assert!(has(
        "proxima-core/wake-trace-jsonl-v1",
        PayloadKind::CitedObject
    ));
    assert!(has(
        "proxima-core/uploaded-blob-v1",
        PayloadKind::CitedObject
    ));
    assert!(has(
        "proxima-core/wake-trace-citation-v1",
        PayloadKind::CitationMapping
    ));
}

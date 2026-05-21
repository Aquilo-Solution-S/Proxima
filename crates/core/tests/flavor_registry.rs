use proxima_core::flavor::FlavorRegistry;
use proxima_core::verbs::schema::PayloadKind;

#[test]
fn update_wake_entry_patch_schema_is_object() {
    let frozen = FlavorRegistry::default().freeze();
    let schema = &frozen
        .list_mcp_tools()
        .iter()
        .find(|tool| tool.name == "core/update_wake_entry")
        .expect("core/update_wake_entry registered")
        .args_schema;
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

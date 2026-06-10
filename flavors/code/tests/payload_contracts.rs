use proxima_core::harness::build_wake_tool_projection;
use proxima_core::verbs::schema::PayloadKind;
use proxima_harness::tools::strict_inventory::assert_tool_schemas_have_property_descriptions;

#[test]
fn all_abstraction_and_perspective_schemas_have_json_schema() {
    let mut registry = proxima_core::FlavorRegistry::new();
    proxima_code::register(&mut registry);
    let registry = registry.freeze();

    for schema in registry.list() {
        if matches!(
            schema.kind,
            PayloadKind::Abstraction | PayloadKind::Perspective
        ) {
            assert!(
                registry
                    .payload_json_schema(&schema.schema_id, schema.schema_version, schema.kind)
                    .is_some(),
                "{} v{} missing json_schema",
                schema.schema_id.as_str(),
                schema.schema_version.into_inner()
            );
        }
    }
}

#[test]
fn typed_emit_projections_describe_payload_fields() {
    let mut registry = proxima_core::FlavorRegistry::new();
    proxima_code::register(&mut registry);
    let registry = registry.freeze();

    let projection = build_wake_tool_projection(
        &registry,
        &[
            "core/emit_abstraction".to_string(),
            "core/emit_perspective".to_string(),
        ],
    )
    .expect("typed emit projection");
    let schemas: Vec<_> = projection
        .into_iter()
        .map(|tool| (tool.canonical_name, tool.input_schema))
        .collect();

    assert_tool_schemas_have_property_descriptions(&schemas);
}

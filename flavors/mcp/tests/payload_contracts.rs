use proxima_core::verbs::schema::PayloadKind;

#[test]
fn all_abstraction_and_perspective_schemas_have_json_schema() {
    let mut registry = proxima_core::FlavorRegistry::new();
    proxima_mcp_substrate::register(&mut registry);
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

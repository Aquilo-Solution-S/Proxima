use proxima_core::verbs::schema::{
    FlavorRegistryFrozen, PayloadKind, SchemaInfo, SchemaRequest, SchemaTombstone,
};
use proxima_core::{SchemaId, SchemaVersion};

#[test]
fn empty_registry_returns_empty_response() {
    let registry = FlavorRegistryFrozen::new();
    let resp = registry.handle(&SchemaRequest);
    assert!(resp.schemas.is_empty());
}

#[test]
fn stateful_filters_for_schema_returns_all_versions() {
    let schema_id = SchemaId::new("test/stateful".into());
    let registry = FlavorRegistryFrozen::with_schemas(vec![
        SchemaInfo {
            schema_id: schema_id.clone(),
            schema_version: SchemaVersion::new(1),
            kind: PayloadKind::Fact,
            filter_keys: vec![],
            sidecar_table: Some("test_schema.stateful_v1".into()),
            natural_key_columns: vec!["entity_id".into()],
            tombstone: Some(SchemaTombstone {
                column: "state".into(),
                value: "Tombstone".into(),
            }),
            cbor_encoder: None,
        },
        SchemaInfo {
            schema_id: schema_id.clone(),
            schema_version: SchemaVersion::new(2),
            kind: PayloadKind::Fact,
            filter_keys: vec![],
            sidecar_table: Some("test_schema.stateful_v2".into()),
            natural_key_columns: vec!["entity_id".into()],
            tombstone: Some(SchemaTombstone {
                column: "state".into(),
                value: "Tombstone".into(),
            }),
            cbor_encoder: None,
        },
        SchemaInfo {
            schema_id: SchemaId::new("test/stateless".into()),
            schema_version: SchemaVersion::new(1),
            kind: PayloadKind::Fact,
            filter_keys: vec![],
            sidecar_table: Some("test_schema.stateless_v1".into()),
            natural_key_columns: vec![],
            tombstone: None,
            cbor_encoder: None,
        },
    ]);

    let filters = registry.stateful_filters_for_schema(&schema_id);
    let versions = filters
        .iter()
        .map(|f| f.schema_version.into_inner())
        .collect::<Vec<_>>();
    assert_eq!(versions, vec![1, 2]);
    assert_eq!(
        filters
            .iter()
            .map(|f| f.sidecar_table.as_str())
            .collect::<Vec<_>>(),
        vec!["test_schema.stateful_v1", "test_schema.stateful_v2"],
    );
}

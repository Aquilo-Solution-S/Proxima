use proxima_core::verbs::schema::{
    FlavorRegistryFrozen, PayloadKind, SchemaInfo, SchemaRequest, SchemaTombstone,
};
use proxima_core::{
    CORE_DERIVED_FROM_RELATION, CORE_INSPIRES_RELATION, FlavorRegistry, SchemaId, SchemaVersion,
};

#[test]
fn empty_registry_returns_empty_response() {
    let registry = FlavorRegistryFrozen::new();
    let resp = registry.handle(&SchemaRequest);
    assert!(resp.schemas.is_empty());
    assert!(resp.relations.is_empty());
}

#[test]
fn schema_verb_exposes_relation_policies() {
    let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
    let resp = registry.handle(&SchemaRequest);

    let derived_from = resp
        .relations
        .iter()
        .find(|relation| relation.relation == CORE_DERIVED_FROM_RELATION)
        .expect("core derived-from relation exposed");
    assert_eq!(derived_from.owner_policy, "SourceOwned");
    assert_eq!(derived_from.target_access_policy, "Read");
    assert_eq!(derived_from.source_binding, "Pin");
    assert_eq!(derived_from.target_binding, "Pin");
    assert!(
        derived_from
            .authorship_mask
            .contains(&"OperatorFtoA".into())
    );
    assert!(
        derived_from
            .authorship_mask
            .contains(&"OperatorAtoA".into())
    );
    assert_eq!(derived_from.payload_schema, None);

    let inspires = resp
        .relations
        .iter()
        .find(|relation| relation.relation == CORE_INSPIRES_RELATION)
        .expect("core inspires relation exposed");
    assert_eq!(inspires.owner_policy, "SameOwner");
    assert_eq!(inspires.target_access_policy, "Write");
    assert!(
        inspires
            .authorship_mask
            .contains(&"PerspectiveGoalLink".into())
    );
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
            has_typed_ingress: false,
            cited_object_schema: None,
            embeddable: true,
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
            has_typed_ingress: false,
            cited_object_schema: None,
            embeddable: true,
        },
        SchemaInfo {
            schema_id: SchemaId::new("test/stateless".into()),
            schema_version: SchemaVersion::new(1),
            kind: PayloadKind::Fact,
            filter_keys: vec![],
            sidecar_table: Some("test_schema.stateless_v1".into()),
            natural_key_columns: vec![],
            tombstone: None,
            has_typed_ingress: false,
            cited_object_schema: None,
            embeddable: true,
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

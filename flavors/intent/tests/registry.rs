use proxima_core::verbs::schema::PayloadKind;
use proxima_core::{CORE_DERIVED_FROM_RELATION, FlavorRegistry};
use std::collections::HashSet;

#[test]
fn intent_schema_registers() {
    let mut registry = FlavorRegistry::new();
    proxima_flavor_intent::register(&mut registry);
    let frozen = registry.freeze();

    let schemas = frozen.list();
    let schema = schemas
        .iter()
        .find(|schema| schema.schema_id.as_str() == "proxima-intent/vision-brief-v1")
        .expect("vision brief schema registered");
    assert_eq!(schema.kind, PayloadKind::Abstraction);
    assert_eq!(schema.schema_version.into_inner(), 1);
    assert_eq!(
        schema.sidecar_table.as_deref(),
        Some("proxima_intent.vision_brief_v1")
    );

    let relation_ids: HashSet<_> = frozen
        .list_relations()
        .iter()
        .map(|relation| relation.relation.as_str())
        .collect();
    assert!(relation_ids.contains(CORE_DERIVED_FROM_RELATION));
}

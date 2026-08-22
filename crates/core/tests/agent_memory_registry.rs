use proxima_core::FlavorRegistry;
use proxima_core::verbs::schema::PayloadKind;
use std::collections::HashSet;

#[test]
fn substrate_schemas_register() {
    let registry = FlavorRegistry::new();
    let frozen = registry.freeze_or_panic_for_tests();

    let schemas = frozen.list();
    let schema_ids: HashSet<_> = schemas.iter().map(|s| s.schema_id.as_str()).collect();
    assert!(schema_ids.contains("core/agent-note-v1"));
    assert!(schema_ids.contains("core/utterance-v1"));
    assert!(schema_ids.contains("core/agent-derivation-v1"));
    // Reason and confidence live on the interpretation Perspective node,
    // never on an edge.
    assert!(schema_ids.contains("core/interpretation-v1"));
}

/// A judgment about other memories is a Perspective. Nothing registers
/// it as anything else, and there is no edge-payload kind left for it to
/// have been registered as.
#[test]
fn the_interpretation_schema_is_a_perspective() {
    let frozen = FlavorRegistry::new().freeze_or_panic_for_tests();
    let interpretation = frozen
        .list()
        .into_iter()
        .find(|schema| schema.schema_id.as_str() == "core/interpretation-v1")
        .expect("interpretation schema registered");
    assert_eq!(interpretation.kind, PayloadKind::Perspective);
    assert_eq!(
        interpretation.sidecar_table.as_deref(),
        Some("proxima_core.interpretation_v1")
    );
}

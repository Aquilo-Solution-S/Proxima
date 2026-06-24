use proxima_core::{CORE_DERIVED_FROM_RELATION, FlavorRegistry};
use std::collections::HashSet;

#[test]
fn substrate_schemas_register() {
    let registry = FlavorRegistry::new();
    let frozen = registry.freeze();

    let schemas = frozen.list();
    let schema_ids: HashSet<_> = schemas.iter().map(|s| s.schema_id.as_str()).collect();
    assert!(schema_ids.contains("core/agent-note-v1"));
    assert!(schema_ids.contains("core/utterance-v1"));
    assert!(schema_ids.contains("core/agent-derivation-v1"));
    assert!(schema_ids.contains("core/agent-link-v1"));

    let relation_ids: HashSet<_> = frozen
        .list_relations()
        .iter()
        .map(|r| r.relation.as_str())
        .collect();
    assert!(relation_ids.contains("core/agent-link-refers-to"));
    assert!(relation_ids.contains(CORE_DERIVED_FROM_RELATION));

    let resolved = frozen
        .resolve_relation("core/agent-link-refers-to")
        .expect("relation resolves");
    assert_eq!(
        resolved.payload_sidecar_table,
        Some("proxima_core.agent_link_v1")
    );
}

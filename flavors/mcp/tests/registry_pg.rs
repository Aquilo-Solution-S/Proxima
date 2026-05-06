use proxima_core::{CORE_DERIVED_FROM_RELATION, FlavorRegistry};
use std::collections::HashSet;

#[test]
fn substrate_schemas_register() {
    let mut registry = FlavorRegistry::new();
    proxima_mcp_substrate::register(&mut registry);
    let frozen = registry.freeze();

    let schemas = frozen.list();
    let schema_ids: HashSet<_> = schemas.iter().map(|s| s.schema_id.as_str()).collect();
    assert!(schema_ids.contains("proxima-mcp/agent-note-v1"));
    assert!(schema_ids.contains("proxima-mcp/agent-derivation-v1"));
    assert!(schema_ids.contains("proxima-mcp/agent-link-v1"));

    let relation_ids: HashSet<_> = frozen
        .list_relations()
        .iter()
        .map(|r| r.relation.as_str())
        .collect();
    assert!(relation_ids.contains("proxima-mcp/agent-link-refers-to"));
    assert!(relation_ids.contains(CORE_DERIVED_FROM_RELATION));

    let resolved = frozen
        .resolve_relation("proxima-mcp/agent-link-refers-to")
        .expect("relation resolves");
    assert_eq!(
        resolved.payload_sidecar_table,
        Some("proxima_mcp.agent_link_v1")
    );
}

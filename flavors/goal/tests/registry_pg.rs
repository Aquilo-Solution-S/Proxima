use proxima_core::{CORE_DERIVED_FROM_RELATION, FlavorRegistry};
use std::collections::HashSet;

#[test]
fn goal_schemas_and_relations_register() {
    let mut registry = FlavorRegistry::new();
    proxima_flavor_goal::register(&mut registry);
    let frozen = registry.freeze();

    let schema_ids: HashSet<_> = frozen
        .list()
        .into_iter()
        .map(|schema| schema.schema_id.as_str().to_string())
        .collect();
    assert!(schema_ids.contains("proxima-goal/goal-proposed-v1"));
    assert!(schema_ids.contains("proxima-goal/goal-activated-v1"));
    assert!(schema_ids.contains("proxima-goal/simple-text-v1"));
    assert!(schema_ids.contains("proxima-goal/task-v1"));

    let relation_ids: HashSet<_> = frozen
        .list_relations()
        .iter()
        .map(|relation| relation.relation.as_str())
        .collect();
    assert!(relation_ids.contains("proxima-goal/motivated-by"));
    assert!(relation_ids.contains(CORE_DERIVED_FROM_RELATION));

    let resolved = frozen
        .resolve_relation("proxima-goal/motivated-by")
        .expect("relation resolves");
    assert_eq!(resolved.payload_sidecar_table, None);
    let authored = frozen
        .resolve_relation("core/authored")
        .expect("core authored relation resolves");
    assert_eq!(authored.payload_sidecar_table, None);

    let tool_names: HashSet<_> = frozen
        .list_mcp_tools()
        .iter()
        .map(|tool| tool.name)
        .collect();
    assert!(tool_names.contains("proxima-goal/goal_propose"));
    assert!(tool_names.contains("proxima-goal/goal_accept"));
    assert!(tool_names.contains("proxima-goal/goal_modify"));
    assert!(tool_names.contains("proxima-goal/goal_decline"));
}

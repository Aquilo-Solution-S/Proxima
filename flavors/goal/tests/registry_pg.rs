use proxima_core::{CORE_DERIVED_FROM_RELATION, FlavorRegistry};
use std::collections::{HashMap, HashSet};

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
    assert!(schema_ids.contains("proxima-goal/goal-achieved-v1"));
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
    assert!(tool_names.contains("proxima-goal/goal_mark_achieved"));

    let produced_by_tool: HashMap<_, _> = frozen
        .list_mcp_tools()
        .iter()
        .map(|tool| (tool.name, tool.produces_schema_ids))
        .collect();
    assert_eq!(
        produced_by_tool["proxima-goal/goal_propose"],
        ["proxima-goal/goal-proposed-v1"]
    );
    assert_eq!(
        produced_by_tool["proxima-goal/goal_accept"],
        ["proxima-goal/goal-activated-v1"]
    );
    assert_eq!(
        produced_by_tool["proxima-goal/goal_modify"],
        ["proxima-goal/goal-activated-v1"]
    );
    assert_eq!(
        produced_by_tool["proxima-goal/goal_decompose"],
        [
            "proxima-goal/goal-activated-v1",
            "proxima-goal/goal-proposed-v1"
        ]
    );
    assert_eq!(
        produced_by_tool["proxima-goal/goal_decline"],
        [] as [&str; 0]
    );
    assert_eq!(
        produced_by_tool["proxima-goal/goal_mark_achieved"],
        ["proxima-goal/goal-achieved-v1"]
    );
}

#[test]
fn goal_payload_tool_schema_exposes_adjacent_tagged_object() {
    let mut registry = FlavorRegistry::new();
    proxima_flavor_goal::register(&mut registry);
    let frozen = registry.freeze();

    let propose = frozen
        .list_mcp_tools()
        .iter()
        .find(|tool| tool.name == "proxima-goal/goal_propose")
        .expect("goal_propose tool registered");

    let payload = propose
        .args_schema
        .pointer("/properties/payload")
        .expect("payload schema");
    assert_eq!(
        payload.pointer("/type").and_then(|v| v.as_str()),
        Some("object")
    );
    assert_eq!(
        payload
            .pointer("/properties/schema_id/type")
            .and_then(|v| v.as_str()),
        Some("string")
    );
    assert_eq!(
        payload
            .pointer("/properties/body/type")
            .and_then(|v| v.as_str()),
        Some("object")
    );
    assert_eq!(
        payload.pointer("/required"),
        Some(&serde_json::json!(["schema_id", "body"]))
    );
    assert_ne!(
        payload.pointer("/type").and_then(|v| v.as_str()),
        Some("string")
    );
}

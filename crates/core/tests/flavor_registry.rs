use proxima_core::flavor::FlavorRegistry;

#[test]
fn core_wake_update_patch_schema_is_object() {
    fn contains_key(value: &serde_json::Value, key: &str) -> bool {
        match value {
            serde_json::Value::Object(map) => {
                map.contains_key(key) || map.values().any(|v| contains_key(v, key))
            }
            serde_json::Value::Array(items) => items.iter().any(|v| contains_key(v, key)),
            _ => false,
        }
    }
    fn property_schema<'a>(
        value: &'a serde_json::Value,
        property: &str,
    ) -> Option<&'a serde_json::Value> {
        match value {
            serde_json::Value::Object(map) => map
                .get("properties")
                .and_then(|properties| properties.get(property))
                .or_else(|| map.values().find_map(|v| property_schema(v, property))),
            serde_json::Value::Array(items) => {
                items.iter().find_map(|v| property_schema(v, property))
            }
            _ => None,
        }
    }
    let frozen = FlavorRegistry::default().freeze();
    let schema = &frozen
        .list_mcp_tools()
        .iter()
        .find(|tool| tool.name == "core_wake")
        .expect("core_wake registered")
        .args_schema;
    assert!(
        !contains_key(schema, "$defs"),
        "core_wake schema must be fully inlined, no $defs: {schema:#}",
    );
    let patch = property_schema(schema, "patch").expect("patch property schema present");
    assert_eq!(
        patch.get("type").and_then(serde_json::Value::as_str),
        Some("object"),
        "patch schema should be exposed as an object, not a string or unresolved ref: {patch:#}",
    );
    assert!(
        patch.get("$ref").is_none(),
        "patch schema must be inline for MCP clients that do not resolve refs: {patch:#}",
    );
    assert!(
        patch.pointer("/properties/probability_promille").is_some(),
        "patch schema should expose WakeEntryPatch fields: {patch:#}",
    );
}

/// Every registered MCP tool's argument schema must be a JSON object schema at
/// the root. MCP clients reject `tools/list` if any `inputSchema` omits the
/// top-level `type: object`, even when inner `oneOf` variants are object-shaped.
#[test]
fn all_mcp_tool_arg_schemas_have_object_root() {
    let frozen = FlavorRegistry::default().freeze();
    for tool in frozen.list_mcp_tools() {
        assert_eq!(
            tool.args_schema
                .get("type")
                .and_then(serde_json::Value::as_str),
            Some("object"),
            "tool {} argument schema must declare top-level type object: {:#}",
            tool.name,
            tool.args_schema,
        );
        assert!(
            tool.args_schema
                .get("properties")
                .is_some_and(serde_json::Value::is_object),
            "tool {} argument schema must expose top-level properties object: {:#}",
            tool.name,
            tool.args_schema,
        );
    }
}

#[test]
fn all_mcp_tool_arg_schemas_avoid_root_combinators() {
    let frozen = FlavorRegistry::default().freeze();
    for tool in frozen.list_mcp_tools() {
        for keyword in ["oneOf", "anyOf", "allOf"] {
            assert!(
                tool.args_schema.get(keyword).is_none(),
                "tool {} argument schema must not expose top-level {keyword}: {:#}",
                tool.name,
                tool.args_schema,
            );
        }
    }
}

const CORE_GOAL_ACTION_NAMES: &[&str] =
    &["set", "transition", "modify", "mark_achieved", "decompose"];
const CORE_WAKE_ACTION_NAMES: &[&str] = &["add", "update", "remove", "set", "list"];
const CORE_PERSONALITY_ACTION_NAMES: &[&str] = &[
    "instantiate",
    "tombstone",
    "set_read_scope",
    "list",
    "get",
    "list_read_scope",
];
const CORE_FACT_ACTION_NAMES: &[&str] = &[
    "citation_of_fact",
    "citation_of_entity_head",
    "facts_citing_object",
    "tombstone",
];
const CORE_MEMBERSHIP_ACTION_NAMES: &[&str] = &["add_member", "remove_member", "list_members"];
const CORE_SHARE_ACTION_NAMES: &[&str] = &[
    "share",
    "unshare",
    "publish",
    "unpublish",
    "list_shares",
    "list_world",
];
const DISPATCHER_TOOL_ACTIONS: &[(&str, &[&str])] = &[
    ("core_goal", CORE_GOAL_ACTION_NAMES),
    ("core_wake", CORE_WAKE_ACTION_NAMES),
    ("core_personality", CORE_PERSONALITY_ACTION_NAMES),
    ("core_fact", CORE_FACT_ACTION_NAMES),
    ("core_membership", CORE_MEMBERSHIP_ACTION_NAMES),
    ("core_share", CORE_SHARE_ACTION_NAMES),
];

#[test]
fn dispatcher_tool_arg_schemas_expose_action_enum() {
    let frozen = FlavorRegistry::default().freeze();
    for &(tool_name, expected_actions) in DISPATCHER_TOOL_ACTIONS {
        let schema = &frozen
            .list_mcp_tools()
            .iter()
            .find(|tool| tool.name == tool_name)
            .unwrap_or_else(|| panic!("{tool_name} registered"))
            .args_schema;
        assert!(
            schema
                .get("properties")
                .is_some_and(serde_json::Value::is_object),
            "dispatcher tool {tool_name} must expose root properties: {schema:#}",
        );
        assert!(
            schema
                .pointer("/required")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|required| required.iter().any(|item| item == "action")),
            "dispatcher tool {tool_name} must require the action discriminator: {schema:#}",
        );
        let action_enum = schema
            .pointer("/properties/action/enum")
            .and_then(serde_json::Value::as_array)
            .unwrap_or_else(|| {
                panic!("dispatcher tool {tool_name} must expose action enum: {schema:#}")
            });
        let action_metadata = schema
            .get("x-proxima-actions")
            .and_then(serde_json::Value::as_object)
            .unwrap_or_else(|| {
                panic!("dispatcher tool {tool_name} must expose x-proxima-actions: {schema:#}")
            });
        assert_eq!(
            action_metadata.len(),
            expected_actions.len(),
            "dispatcher tool {tool_name} must expose one metadata entry per action",
        );
        let mut actions = action_enum
            .iter()
            .map(|value| {
                value.as_str().unwrap_or_else(|| {
                    panic!("dispatcher tool {tool_name} action enum values must be strings")
                })
            })
            .collect::<Vec<_>>();
        actions.sort_unstable();
        let mut expected = expected_actions.to_vec();
        expected.sort_unstable();
        assert_eq!(
            actions, expected,
            "dispatcher tool {tool_name} must preserve expected action variants",
        );
        for action in expected_actions {
            assert!(
                action_metadata.contains_key(*action),
                "dispatcher tool {tool_name} metadata must include action {action}",
            );
        }
    }
}

#[test]
fn core_goal_action_metadata_preserves_required_fields() {
    let frozen = FlavorRegistry::default().freeze();
    let schema = &frozen
        .list_mcp_tools()
        .iter()
        .find(|tool| tool.name == "core_goal")
        .expect("core_goal registered")
        .args_schema;

    let required_for = |action: &str| -> Vec<&str> {
        schema
            .pointer(&format!("/x-proxima-actions/{action}/required_fields"))
            .and_then(serde_json::Value::as_array)
            .unwrap_or_else(|| {
                panic!("core_goal metadata must expose required_fields for {action}")
            })
            .iter()
            .map(|item| item.as_str().expect("required field names are strings"))
            .collect()
    };

    assert!(
        required_for("decompose").contains(&"idempotency_key"),
        "decompose must advertise its required idempotency_key",
    );
    assert!(
        required_for("mark_achieved").contains(&"evidence"),
        "mark_achieved must advertise required completion evidence",
    );

    for field in ["evidence", "title", "schema_id"] {
        let description = schema
            .pointer(&format!("/properties/{field}/description"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_else(|| panic!("{field} root description"));
        assert!(
            description.contains("Shared dispatcher field"),
            "shared root {field} description must be neutral, not action-specific: {description}",
        );
        assert!(
            description.contains("x-proxima-actions"),
            "shared root {field} description must point LLMs to action metadata: {description}",
        );
    }
    assert!(
        schema.pointer("/properties/evidence/default").is_none(),
        "shared root evidence schema must not keep an action-specific default",
    );
    let mark_achieved_evidence_description = schema
        .pointer("/x-proxima-actions/mark_achieved/field_descriptions/evidence")
        .and_then(serde_json::Value::as_str)
        .expect("mark_achieved evidence field description");
    assert!(
        mark_achieved_evidence_description.contains("at least one"),
        "action metadata must preserve action-specific evidence semantics: {mark_achieved_evidence_description}",
    );
}

/// The hand-written `McpActionArgSpec` lists that gate `validate_action_args`
/// (and feed the `proxima://tools` catalog) must match the schemars-derived
/// `x-proxima-actions` metadata exactly. Without this guard the two silently
/// drift: add a field to a dispatcher variant struct and forget its
/// `allowed_fields` entry, and `validate_action_args` starts rejecting valid
/// calls; drop one and it starts accepting fields serde cannot deserialize.
#[test]
fn action_arg_specs_match_schema_derived_action_fields() {
    use std::collections::BTreeSet;

    let frozen = FlavorRegistry::default().freeze();
    let mut dispatchers_seen = BTreeSet::new();
    for tool in frozen.list_mcp_tools() {
        if tool.action_arg_specs.is_empty() {
            continue;
        }
        dispatchers_seen.insert(tool.name);
        let actions = tool
            .args_schema
            .pointer("/x-proxima-actions")
            .and_then(serde_json::Value::as_object)
            .unwrap_or_else(|| {
                panic!(
                    "dispatcher {} must expose x-proxima-actions: {:#}",
                    tool.name, tool.args_schema
                )
            });
        for spec in tool.action_arg_specs {
            let meta = actions.get(spec.action).unwrap_or_else(|| {
                panic!(
                    "{} spec action `{}` has no x-proxima-actions entry: {:#}",
                    tool.name, spec.action, tool.args_schema
                )
            });
            let schema_fields = |key: &str| -> BTreeSet<String> {
                meta.get(key)
                    .and_then(serde_json::Value::as_array)
                    .unwrap_or_else(|| {
                        panic!(
                            "{} action `{}` metadata missing `{key}`",
                            tool.name, spec.action
                        )
                    })
                    .iter()
                    .map(|field| field.as_str().expect("field names are strings").to_string())
                    .collect()
            };
            let spec_fields = |fields: &[&str]| {
                fields
                    .iter()
                    .map(|s| (*s).to_string())
                    .collect::<BTreeSet<_>>()
            };

            assert_eq!(
                schema_fields("allowed_fields"),
                spec_fields(spec.allowed_fields),
                "{} action `{}`: ACTION_ARG_SPECS.allowed_fields drifted from the schemars-derived schema",
                tool.name,
                spec.action,
            );
            assert_eq!(
                schema_fields("required_fields"),
                spec_fields(spec.required_fields),
                "{} action `{}`: ACTION_ARG_SPECS.required_fields drifted from the schemars-derived schema",
                tool.name,
                spec.action,
            );
        }
    }

    for expected in [
        "core_goal",
        "core_wake",
        "core_personality",
        "core_fact",
        "core_membership",
        "core_share",
    ] {
        assert!(
            dispatchers_seen.contains(expected),
            "expected dispatcher {expected} to carry ACTION_ARG_SPECS; saw {dispatchers_seen:?}",
        );
    }
}

/// Every registered MCP tool's argument schema must be fully inlined —
/// no `$ref`, no `$defs` anywhere — so MCP clients that do not resolve
/// references still render every field.
#[test]
fn all_mcp_tool_arg_schemas_are_ref_and_defs_free() {
    fn contains_key(value: &serde_json::Value, key: &str) -> bool {
        match value {
            serde_json::Value::Object(map) => {
                map.contains_key(key) || map.values().any(|v| contains_key(v, key))
            }
            serde_json::Value::Array(items) => items.iter().any(|v| contains_key(v, key)),
            _ => false,
        }
    }

    let frozen = FlavorRegistry::default().freeze();
    for tool in frozen.list_mcp_tools() {
        assert!(
            !contains_key(&tool.args_schema, "$ref"),
            "tool {} has a $ref in its argument schema: {:#}",
            tool.name,
            tool.args_schema,
        );
        assert!(
            !contains_key(&tool.args_schema, "$defs"),
            "tool {} has a $defs block in its argument schema: {:#}",
            tool.name,
            tool.args_schema,
        );
    }
}

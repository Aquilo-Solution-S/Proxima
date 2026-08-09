use proxima_core::flavor::FlavorRegistry;

/// Every registered MCP tool's argument schema must be a JSON object schema at
/// the root. MCP clients reject `tools/list` if any `inputSchema` omits the
/// top-level `type: object`, even when inner `oneOf` variants are object-shaped.
#[test]
fn all_mcp_tool_arg_schemas_have_object_root() {
    let frozen = FlavorRegistry::default().freeze_or_panic_for_tests();
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

/// A parameter whose prose promises a bound must declare it in the schema
/// too, at both ends. A strict JSON-Schema client must not be told that
/// `limit: 0` validates, nor that a 30,000-character body does, and then be
/// refused at runtime after paying to send it. The rule lives in
/// `schema_bound_mismatches` so the core registry, `proxima-code`, and any
/// out-of-tree flavor check it the same way instead of keeping three copies.
#[test]
fn a_schema_declares_the_bounds_its_description_promises() {
    let frozen = FlavorRegistry::default().freeze_or_panic_for_tests();
    let offenders = proxima_core::mcp::schema_bound_mismatches(&frozen);
    assert!(
        offenders.is_empty(),
        "schema and description disagree about a bound:\n  {}",
        offenders.join("\n  "),
    );
}

#[test]
fn all_mcp_tool_arg_schemas_avoid_root_combinators() {
    let frozen = FlavorRegistry::default().freeze_or_panic_for_tests();
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
const CORE_FACT_ACTION_NAMES: &[&str] = &[
    "citation_of_fact",
    "citation_of_entity_head",
    "facts_citing_object",
];
const CORE_MEMBERSHIP_ACTION_NAMES: &[&str] = &["add_member", "remove_member", "list_members"];
const CORE_PUBLISH_ACTION_NAMES: &[&str] = &["publish_to_world"];
const DISPATCHER_TOOL_ACTIONS: &[(&str, &[&str])] = &[
    ("core_goal", CORE_GOAL_ACTION_NAMES),
    ("core_fact", CORE_FACT_ACTION_NAMES),
    ("core_membership", CORE_MEMBERSHIP_ACTION_NAMES),
    ("core_publish", CORE_PUBLISH_ACTION_NAMES),
];

#[test]
fn pr6_retired_wake_and_personality_dispatchers_are_absent() {
    let frozen = FlavorRegistry::default().freeze_or_panic_for_tests();
    let names = frozen
        .list_mcp_tools()
        .iter()
        .map(|tool| tool.name)
        .collect::<Vec<_>>();
    for retired in [
        format!("core_{}", "wake"),
        format!("core_{}", "personality"),
    ] {
        assert!(
            !names.contains(&retired.as_str()),
            "retired PR6 dispatcher remains registered: {retired}",
        );
    }
}

#[test]
fn dispatcher_tool_arg_schemas_expose_action_enum() {
    let frozen = FlavorRegistry::default().freeze_or_panic_for_tests();
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
    let frozen = FlavorRegistry::default().freeze_or_panic_for_tests();
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
///
/// The same invariant is now also a boot guard —
/// `FlavorRegistry::try_freeze` refuses a registry whose specs and derived
/// schema disagree, for every registered tool including a flavor's. This
/// stays as the backstop that says which field of which action drifted:
/// freeze answers "this registry does not seal", and a per-action message
/// is what makes the fix a one-line edit rather than a bisect.
#[test]
fn action_arg_specs_match_schema_derived_action_fields() {
    use std::collections::BTreeSet;

    let frozen = FlavorRegistry::default().freeze_or_panic_for_tests();
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
        "core_fact",
        "core_membership",
        "core_publish",
        "core_upload",
    ] {
        assert!(
            dispatchers_seen.contains(expected),
            "expected dispatcher {expected} to carry ACTION_ARG_SPECS; saw {dispatchers_seen:?}",
        );
    }
}

/// `CoreActionMeta` is decoration, not enumeration.
///
/// A substrate action is described in two places on purpose: the
/// descriptor's `ACTION_ARG_SPECS` say which actions exist, what fields each
/// takes, and how each is authorized. The `CoreActionMeta` table adds only
/// substrate decoration — a scope key, prose, and produced schema ids. The
/// split is fine; the two silently disagreeing is not. A
/// meta entry for an action no spec declares describes a call nobody can
/// make; a declared action with no meta entry is a substrate action that
/// lists no description in `proxima://tools` and answers the owner-role gate
/// at tool level.
///
/// A test rather than a freeze guard: `all_core_actions()` is a curated
/// substrate allow-list (`memory_keep_set` in `proxima-mcp` reads it as
/// exactly that), and boot has no business refusing to start over a missing
/// sentence.
#[test]
fn core_action_meta_decorates_only_declared_actions() {
    use proxima_core::mcp::{McpToolOrigin, all_core_actions};
    use std::collections::BTreeSet;

    let frozen = FlavorRegistry::default().freeze_or_panic_for_tests();

    for meta in all_core_actions() {
        let declared: BTreeSet<&str> = frozen
            .mcp_tool(meta.tool)
            .unwrap_or_else(|| panic!("CoreActionMeta names unregistered tool {}", meta.tool))
            .action_arg_specs
            .iter()
            .map(|spec| spec.action)
            .collect();
        assert!(
            declared.contains(meta.action),
            "CoreActionMeta describes {}:{}, which its ACTION_ARG_SPECS do not declare; \
             the enumeration is the specs, so this action does not exist (declared: {declared:?})",
            meta.tool,
            meta.action,
        );
    }

    for tool in frozen.list_mcp_tools() {
        if tool.origin != McpToolOrigin::Substrate {
            continue;
        }
        for spec in tool.action_arg_specs {
            assert!(
                proxima_core::mcp::core_action_meta(tool.name, spec.action).is_some(),
                "substrate action {}:{} declares itself but has no CoreActionMeta, so it has no \
                 scope key or substrate description",
                tool.name,
                spec.action,
            );
        }
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

    let frozen = FlavorRegistry::default().freeze_or_panic_for_tests();
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

/// Every registered tool describes what it answers with, and describes it as
/// itself. The `Output` schema goes through a plain generation pass, not the
/// dispatcher normalization the `Args` schema gets: `x-proxima-actions` is a
/// statement about which fields a *caller* may send per action, and stamping
/// it onto a reply would merge variants that a client is trying to tell apart.
#[test]
fn every_mcp_tool_declares_an_output_schema_without_dispatcher_normalization() {
    let frozen = FlavorRegistry::default().freeze_or_panic_for_tests();
    for tool in frozen.list_mcp_tools() {
        assert!(
            tool.output_schema.is_object(),
            "tool {} must declare an output schema document: {:#}",
            tool.name,
            tool.output_schema,
        );
        assert!(
            tool.output_schema.get("x-proxima-actions").is_none(),
            "tool {} output schema must not carry the dispatcher args extension: {:#}",
            tool.name,
            tool.output_schema,
        );
    }
}

/// A dispatcher answers with an untagged union, and the manifest says so. The
/// argument-side flattener would have collapsed this into one merged object
/// with `additionalProperties: false`, which describes no reply the tool ever
/// sends.
#[test]
fn a_dispatcher_output_schema_keeps_its_union_root() {
    let frozen = FlavorRegistry::default().freeze_or_panic_for_tests();
    let goal = frozen
        .list_mcp_tools()
        .iter()
        .find(|tool| tool.name == "core_goal")
        .expect("core_goal is registered");
    assert!(
        goal.output_schema
            .get("anyOf")
            .is_some_and(serde_json::Value::is_array),
        "core_goal answers with an untagged union and must advertise it: {:#}",
        goal.output_schema,
    );
}

/// Declaring an output schema is a promise a client may enforce. MCP carries
/// a tool's typed reply in `structuredContent`, which is a JSON *object*, and
/// `McpToolHost` already sets it from whatever the tool returned — so a
/// registered tool whose output could serialize to a scalar, array, or null
/// would advertise a shape its own replies violate. Every branch of a union
/// root has to clear the same bar.
#[test]
fn every_registered_tool_answers_with_an_object() {
    fn describes_an_object(schema: &serde_json::Value) -> bool {
        if schema.get("type").and_then(serde_json::Value::as_str) == Some("object") {
            return true;
        }
        for key in ["anyOf", "oneOf"] {
            if let Some(branches) = schema.get(key).and_then(serde_json::Value::as_array) {
                return !branches.is_empty() && branches.iter().all(describes_an_object);
            }
        }
        false
    }

    let frozen = FlavorRegistry::default().freeze_or_panic_for_tests();
    for tool in frozen.list_mcp_tools() {
        assert!(
            describes_an_object(&tool.output_schema),
            "tool {} advertises an output that is not an object: {:#}",
            tool.name,
            tool.output_schema,
        );
    }
}

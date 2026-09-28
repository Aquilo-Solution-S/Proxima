use proxima_core::flavor::FlavorRegistry;

const CORE_GOAL_ACTION_NAMES: &[&str] =
    &["set", "transition", "modify", "mark_achieved", "decompose"];
const CORE_FACT_ACTION_NAMES: &[&str] = &["citation_of_fact", "facts_citing_object"];
const CORE_MEMBERSHIP_ACTION_NAMES: &[&str] = &["add_member", "remove_member", "list_members"];
const CORE_TRANSFER_ACTION_NAMES: &[&str] = &["transfer_to_owner"];
const DISPATCHER_TOOL_ACTIONS: &[(&str, &[&str])] = &[
    ("core_goal", CORE_GOAL_ACTION_NAMES),
    ("core_fact", CORE_FACT_ACTION_NAMES),
    ("core_membership", CORE_MEMBERSHIP_ACTION_NAMES),
    ("core_transfer", CORE_TRANSFER_ACTION_NAMES),
];

#[test]
fn served_catalog_includes_forget_and_drops_open_batch() {
    let frozen = FlavorRegistry::default().freeze_or_panic_for_tests();
    let names = frozen
        .list_mcp_tools()
        .iter()
        .map(|tool| tool.name)
        .collect::<Vec<_>>();
    assert!(
        names.contains(&"core_forget"),
        "the catalog must serve forget: {names:?}"
    );
    assert!(
        !names.iter().any(|name| name.contains("open_batch")),
        "open_batch must stay deleted: {names:?}"
    );
}

#[test]
fn dispatcher_tool_arg_schemas_expose_action_enum() {
    let frozen = FlavorRegistry::default().freeze_or_panic_for_tests();
    for &(tool_name, expected_actions) in DISPATCHER_TOOL_ACTIONS {
        let tool = frozen
            .list_mcp_tools()
            .iter()
            .find(|tool| tool.name == tool_name)
            .unwrap_or_else(|| panic!("{tool_name} registered"));
        let schema = &tool.args_schema;
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
        let dispatcher = tool
            .dispatcher_schema
            .as_ref()
            .unwrap_or_else(|| panic!("dispatcher tool {tool_name} must derive its actions"));
        assert_eq!(
            dispatcher.actions.len(),
            expected_actions.len(),
            "dispatcher tool {tool_name} must derive one schema per action",
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
                dispatcher.action(action).is_some(),
                "dispatcher tool {tool_name} must derive action {action}",
            );
        }
    }
}

#[test]
fn core_goal_action_metadata_preserves_required_fields() {
    let frozen = FlavorRegistry::default().freeze_or_panic_for_tests();
    let tool = frozen
        .list_mcp_tools()
        .iter()
        .find(|tool| tool.name == "core_goal")
        .expect("core_goal registered");
    let schema = &tool.args_schema;
    let required_for = |action: &str| -> Vec<String> {
        tool.dispatcher_schema
            .as_ref()
            .and_then(|dispatcher| dispatcher.action(action))
            .unwrap_or_else(|| panic!("core_goal must derive {action}"))
            .required_fields()
    };

    assert!(
        required_for("decompose").contains(&"idempotency_key".to_owned()),
        "decompose must advertise its required idempotency_key",
    );
    assert!(
        required_for("mark_achieved").contains(&"evidence".to_owned()),
        "mark_achieved must advertise required completion evidence",
    );
    let guide = schema
        .pointer("/properties/action/description")
        .and_then(serde_json::Value::as_str)
        .expect("action guide");
    assert!(
        guide.contains("\n- decompose: ")
            && guide.contains("Required: parent_goal, children, idempotency_key."),
        "the action guide must carry decompose's required fields: {guide}",
    );

    // A shared field whose prose differs by action carries every action's
    // prose inline; none points a client at a document it cannot read.
    let evidence = schema
        .pointer("/properties/evidence/description")
        .and_then(serde_json::Value::as_str)
        .expect("evidence root description");
    assert!(
        evidence.starts_with("Depends on `action`:"),
        "shared evidence prose must be per action: {evidence}",
    );
    for action in ["set", "modify", "mark_achieved"] {
        assert!(
            evidence.contains(&format!("\n- {action}: ")),
            "evidence prose must cover {action}: {evidence}",
        );
    }
    assert!(
        evidence.contains("at least one"),
        "action-specific evidence semantics must survive: {evidence}",
    );
    for field in ["title", "schema_id"] {
        let description = schema
            .pointer(&format!("/properties/{field}/description"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_else(|| panic!("{field} root description"));
        assert!(
            !description.starts_with("Depends on"),
            "{field} reads the same for every action, so it carries one description: {description}",
        );
    }
    assert!(
        schema.pointer("/properties/evidence/default").is_none(),
        "shared root evidence schema must not keep an action-specific default",
    );
    assert!(
        !schema.to_string().contains("x-proxima"),
        "no wire schema names an extension: {schema:#}",
    );
}

#[test]
fn goal_child_and_episode_evidence_advertise_nonempty_inputs() {
    let frozen = FlavorRegistry::default().freeze_or_panic_for_tests();
    let schema_for = |name: &str| {
        &frozen
            .list_mcp_tools()
            .iter()
            .find(|tool| tool.name == name)
            .unwrap_or_else(|| panic!("{name} registered"))
            .args_schema
    };
    assert_eq!(
        schema_for("core_goal")
            .pointer("/properties/children/items/properties/evidence/minItems")
            .and_then(serde_json::Value::as_u64),
        Some(1),
        "decompose child evidence must be nonempty"
    );
    assert_eq!(
        schema_for("core_goal")
            .pointer("/properties/children/minItems")
            .and_then(serde_json::Value::as_u64),
        Some(1),
        "decompose children must be nonempty"
    );
    assert_eq!(
        schema_for("core_episode_commit")
            .pointer("/properties/goal/items/properties/evidence/minItems")
            .and_then(serde_json::Value::as_u64),
        Some(1),
        "episode Goal evidence must be nonempty"
    );
}

/// The hand-written `McpActionArgSpec` lists that gate `validate_action_args`
/// (and feed the `proxima://tools` catalog) must match the schemars-derived
/// dispatcher schema exactly. Without this guard the two silently
/// drift: add a field to a dispatcher variant struct and forget its
/// `allowed_fields` entry, and `validate_action_args` starts rejecting valid
/// calls; drop one and it starts accepting fields serde cannot deserialize.
///
/// `FlavorRegistry::try_freeze` refuses a registry whose specs and derived
/// schema disagree. This stays as the backstop that names which field of
/// which action drifted: freeze answers "this registry does not seal".
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
        let dispatcher = tool
            .dispatcher_schema
            .as_ref()
            .unwrap_or_else(|| panic!("dispatcher {} must derive its action schemas", tool.name));
        for spec in tool.action_arg_specs {
            let derived = dispatcher.action(spec.action).unwrap_or_else(|| {
                panic!(
                    "{} spec action `{}` has no derived schema: {:#}",
                    tool.name, spec.action, tool.args_schema
                )
            });
            let schema_fields = |key: &str| -> BTreeSet<String> {
                match key {
                    "allowed_fields" => derived.allowed_fields(),
                    _ => derived.required_fields(),
                }
                .into_iter()
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
        "core_transfer",
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
/// substrate decoration — a scope key and produced schema ids. The split is
/// fine; the two silently disagreeing is not. A meta entry for an action no
/// spec declares describes a call nobody can make; a declared action with no
/// meta entry is a substrate action `proxima://tools` lists under a guessed
/// scope key.
///
/// A test rather than a freeze guard: `all_core_actions()` is a curated
/// substrate allow-list (`memory_keep_set` in `proxima-mcp` reads it as
/// exactly that), and boot has no business refusing to start over it.
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
                 scope key",
                tool.name,
                spec.action,
            );
            assert!(
                tool.action_description(spec.action)
                    .is_some_and(|text| !text.trim().is_empty()),
                "substrate action {}:{} has no variant doc comment, so the action guide \
                 says nothing about what it does",
                tool.name,
                spec.action,
            );
        }
    }
}

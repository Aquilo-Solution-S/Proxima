//! Freeze refusals over a dispatcher tool's per-action argument schemas.
//!
//! The registry is the shipped one with one action's schema mutated, so
//! what each case pins is the drift a real edit would produce rather than a
//! hand-built shape no generator emits.

use crate::{FlavorRegistry, FlavorRegistryError};

fn mutated_goal_schema_error(mutate: impl FnOnce(&mut serde_json::Value)) -> FlavorRegistryError {
    let mut registry = FlavorRegistry::default();
    let set = registry
        .mcp_tools
        .iter_mut()
        .find(|tool| tool.name == "core_goal")
        .expect("core_goal is registered")
        .dispatcher_schema
        .as_mut()
        .expect("core_goal is a dispatcher")
        .actions
        .iter_mut()
        .find(|action| action.action == "set")
        .expect("core_goal derives `set`");
    mutate(&mut set.argument_schema);
    registry
        .try_freeze()
        .expect_err("mutated argument_schema must not freeze")
}

#[test]
fn dispatcher_argument_schema_freeze_rejects_malformed_metadata() {
    for (mutate, expected) in [
        (
            Box::new(|schema: &mut serde_json::Value| {
                schema["$ref"] = serde_json::json!("#/$defs/Value");
            }) as Box<dyn FnOnce(&mut serde_json::Value)>,
            "ref-free",
        ),
        (
            Box::new(|schema: &mut serde_json::Value| {
                schema["$defs"] = serde_json::json!({});
            }),
            "ref-free",
        ),
        (
            Box::new(|schema: &mut serde_json::Value| {
                *schema = serde_json::json!("bad");
            }),
            "argument_schema is invalid",
        ),
        (
            Box::new(|schema: &mut serde_json::Value| {
                *schema = serde_json::json!([]);
            }),
            "argument_schema is invalid",
        ),
        (
            Box::new(|schema: &mut serde_json::Value| {
                schema["properties"]["action"] = serde_json::json!({ "type": "string" });
            }),
            "action property",
        ),
        (
            Box::new(|schema: &mut serde_json::Value| {
                schema["additionalProperties"] = serde_json::json!(true);
            }),
            "additionalProperties",
        ),
        (
            Box::new(|schema: &mut serde_json::Value| {
                schema
                    .as_object_mut()
                    .expect("argument schema object")
                    .remove("additionalProperties");
            }),
            "additionalProperties",
        ),
        (
            Box::new(|schema: &mut serde_json::Value| {
                schema["oneOf"] = serde_json::json!([
                    { "type": "object", "properties": { "hidden": { "type": "string" } } }
                ]);
            }),
            "not hoisted",
        ),
    ] {
        let err = mutated_goal_schema_error(mutate);
        assert!(
            matches!(err, FlavorRegistryError::InvalidActionSpecs { .. }),
            "got {err:?}"
        );
        assert!(err.to_string().contains(expected), "{err}");
    }
    // A derived field the spec does not declare is drift between the two.
    let err = mutated_goal_schema_error(|schema| {
        schema["properties"]["other"] = serde_json::json!({ "type": "string" });
    });
    assert!(matches!(
        err,
        FlavorRegistryError::InvalidActionSpecs { .. }
    ));
    assert!(err.to_string().contains("declares allowed_fields"), "{err}");
}

fn goal_mut(registry: &mut FlavorRegistry) -> &mut crate::McpToolDescriptor {
    registry
        .mcp_tools
        .iter_mut()
        .find(|tool| tool.name == "core_goal")
        .expect("core_goal is registered")
}

/// A title is what a person reads beside a recorded call: a tool's and an
/// action's alike are refused at freeze when they are blank, padded, span
/// lines or run past `MAX_TITLE_CHARS`.
#[test]
fn a_title_a_person_cannot_read_is_refused_at_freeze() {
    let longest = "x".repeat(crate::mcp::MAX_TITLE_CHARS);
    let too_long = "x".repeat(crate::mcp::MAX_TITLE_CHARS + 1);
    let leak = |text: &str| -> &'static str { Box::leak(text.to_owned().into_boxed_str()) };

    let mut registry = FlavorRegistry::default();
    goal_mut(&mut registry).title = leak(&longest);
    registry.try_freeze().expect("the longest title is a title");

    for bad in [
        "",
        " Goals",
        "Goals ",
        "Go\nals",
        "Go\tals",
        too_long.as_str(),
    ] {
        let mut registry = FlavorRegistry::default();
        goal_mut(&mut registry).title = leak(bad);
        let err = registry
            .try_freeze()
            .expect_err("a tool title that is none");
        assert!(
            matches!(&err, FlavorRegistryError::InvalidTitle { key, .. } if key == "core_goal"),
            "{bad:?}: {err:?}"
        );

        let mut registry = FlavorRegistry::default();
        let goal = goal_mut(&mut registry);
        let mut specs = goal.action_arg_specs.to_vec();
        specs
            .iter_mut()
            .find(|spec| spec.action == "set")
            .expect("core_goal declares `set`")
            .title = leak(bad);
        goal.action_arg_specs = Box::leak(specs.into_boxed_slice());
        let err = registry
            .try_freeze()
            .expect_err("an action title that is none");
        assert!(
            matches!(&err, FlavorRegistryError::InvalidTitle { key, .. } if key == "core_goal:set"),
            "{bad:?}: {err:?}"
        );
    }
}

/// A host names a recorded call from the descriptor: the tool's title for
/// its bare key, the action's for `tool:action`, nothing for a key the
/// descriptor does not declare.
#[test]
fn the_descriptor_titles_a_tool_and_each_of_its_actions() {
    let registry = FlavorRegistry::default();
    let goal = registry
        .mcp_tools
        .iter()
        .find(|tool| tool.name == "core_goal")
        .expect("core_goal is registered");
    let title = |key: &str| goal.title_of(&crate::ScopeKey::parse(key).expect("a scope key"));
    assert_eq!(title("core_goal"), Some("Goals"));
    assert_eq!(title("core_goal:set"), Some("Create a goal"));
    assert_eq!(title("core_goal:undeclared"), None);
    assert_eq!(title("core_remember"), None);
    assert_eq!(title("core_remember:set"), None);
    assert_eq!(title("resource:memory"), None);
}

/// Each action name of an argv-keyed dispatcher is declared once.
#[test]
fn an_argv_action_name_declared_twice_is_refused_at_freeze() {
    use crate::mcp::{McpArgvActionSpec, McpToolAudience, ToolEffect};

    let spec = |prefix: &'static [&'static str]| McpArgvActionSpec {
        action: "approval",
        argv_prefix: prefix,
        effect: ToolEffect::ReadOnly,
        audience: McpToolAudience::Shared,
        title: "Approve",
    };
    let mut registry = FlavorRegistry::default();
    let goal = goal_mut(&mut registry);
    goal.action_arg_specs = &[];
    goal.dispatcher_schema = None;
    goal.effect = None;
    goal.argv_action_specs = Box::leak(Box::new([spec(&["a"]), spec(&["a", "b"])]));
    let err = registry.try_freeze().expect_err("a repeated action name");
    assert!(
        matches!(&err, FlavorRegistryError::InvalidActionSpecs { name, message }
            if *name == "core_goal" && message.contains("declared once")),
        "{err:?}"
    );
}

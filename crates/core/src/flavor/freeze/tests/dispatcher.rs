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

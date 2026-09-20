//! Freeze refusals over a dispatcher tool's per-action metadata.
//!
//! The registry is the shipped one with one action's schema or metadata
//! mutated, so what each case pins is the drift a real edit would produce
//! rather than a hand-built shape no generator emits.

use crate::{FlavorRegistry, FlavorRegistryError};

fn mutated_goal_schema_error(mutate: impl FnOnce(&mut serde_json::Value)) -> FlavorRegistryError {
    let mut registry = FlavorRegistry::default();
    let tool = registry
        .mcp_tools
        .iter_mut()
        .find(|tool| tool.name == "core_goal")
        .expect("core_goal is registered");
    mutate(&mut tool.args_schema["x-proxima-actions"]["set"]["argument_schema"]);
    registry
        .try_freeze()
        .expect_err("mutated argument_schema must not freeze")
}

fn mutated_goal_metadata_error(
    mutate: impl FnOnce(&mut serde_json::Map<String, serde_json::Value>),
) -> FlavorRegistryError {
    let mut registry = FlavorRegistry::default();
    let tool = registry
        .mcp_tools
        .iter_mut()
        .find(|tool| tool.name == "core_goal")
        .expect("core_goal is registered");
    let action = tool.args_schema["x-proxima-actions"]["set"]
        .as_object_mut()
        .expect("action metadata object");
    mutate(action);
    registry
        .try_freeze()
        .expect_err("mutated action metadata must not freeze")
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
    let err = mutated_goal_metadata_error(|metadata| {
        metadata["allowed_fields"] = serde_json::json!(["other"]);
    });
    assert!(matches!(
        err,
        FlavorRegistryError::InvalidActionSpecs { .. }
    ));
    assert!(err.to_string().contains("metadata allowed_fields"), "{err}");
}

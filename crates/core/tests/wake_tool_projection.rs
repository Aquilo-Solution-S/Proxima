use proxima_core::{
    AbstractionPayload, FlavorRegistry, SchemaId, SchemaVersion,
    harness::build_wake_tool_projection,
};
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct TestBrief {
    goal_id: String,
    planner_directive: String,
}

impl AbstractionPayload for TestBrief {
    const SCHEMA_ID: &'static str = "test/brief-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "test.brief_v1"
    }

    fn json_schema() -> Option<serde_json::Value> {
        serde_json::to_value(schema_for!(TestBrief)).ok()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TestLooseBrief {
    title: String,
}

impl AbstractionPayload for TestLooseBrief {
    const SCHEMA_ID: &'static str = "test/loose-brief-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "test.loose_brief_v1"
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct TestCollisionOne {
    title: String,
}

impl AbstractionPayload for TestCollisionOne {
    const SCHEMA_ID: &'static str = "test/a:b-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "test.collision_one_v1"
    }

    fn json_schema() -> Option<serde_json::Value> {
        serde_json::to_value(schema_for!(TestCollisionOne)).ok()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct TestCollisionTwo {
    title: String,
}

impl AbstractionPayload for TestCollisionTwo {
    const SCHEMA_ID: &'static str = "test/a/b-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "test.collision_two_v1"
    }

    fn json_schema() -> Option<serde_json::Value> {
        serde_json::to_value(schema_for!(TestCollisionTwo)).ok()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct TestReservedText {
    text: String,
}

impl AbstractionPayload for TestReservedText {
    const SCHEMA_ID: &'static str = "test/reserved-text-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "test.reserved_text_v1"
    }

    fn json_schema() -> Option<serde_json::Value> {
        serde_json::to_value(schema_for!(TestReservedText)).ok()
    }
}

#[test]
fn emit_abstraction_palette_expands_to_schema_wrapper() {
    let mut registry = FlavorRegistry::new();
    registry.add_abstraction_schema::<TestBrief>();
    let registry = registry.freeze();

    let projection = build_wake_tool_projection(&registry, &["core/emit_abstraction".to_string()])
        .expect("projection");

    let wrapper = projection
        .iter()
        .find(|tool| tool.palette_id == "core/emit_abstraction")
        .expect("typed wrapper");
    assert_eq!(
        wrapper.canonical_name,
        "core/emit_abstraction::test/brief-v1::v1"
    );
    assert_eq!(
        wrapper.provider_name,
        "core_emit_abstraction__test_brief-v1__v1"
    );
    assert_eq!(
        wrapper.input_schema.pointer("/properties/goal_id/type"),
        Some(&serde_json::json!("string"))
    );
    assert!(
        wrapper
            .input_schema
            .pointer("/properties/goal_id/description")
            .and_then(serde_json::Value::as_str)
            .expect("goal_id description")
            .contains("Use the wake handle")
    );
    assert_eq!(
        wrapper.input_schema.pointer("/properties/text/type"),
        Some(&serde_json::json!(["string", "null"]))
    );
    assert!(
        projection
            .iter()
            .all(|tool| tool.canonical_name != "core/emit_abstraction")
    );
}

#[test]
fn emit_projection_rejects_registered_schema_without_json_schema() {
    let mut registry = FlavorRegistry::new();
    registry.add_abstraction_schema::<TestLooseBrief>();
    let err =
        build_wake_tool_projection(&registry.freeze(), &["core/emit_abstraction".to_string()])
            .expect_err("schema-less A/P payload must fail projection");

    assert!(err.to_string().contains("test/loose-brief-v1"));
    assert!(err.to_string().contains("missing json_schema"));
}

#[test]
fn non_emit_palette_ids_remain_direct_provider_tools() {
    let projection = build_wake_tool_projection(
        &FlavorRegistry::new().freeze(),
        &["core/fetch_memory".to_string()],
    )
    .expect("projection");

    assert_eq!(projection[0].palette_id, "core/fetch_memory");
    assert_eq!(projection[0].canonical_name, "core/fetch_memory");
    assert_eq!(projection[0].provider_name, "core_fetch_memory");
}

#[test]
fn projection_rejects_provider_safe_name_collisions() {
    let mut registry = FlavorRegistry::new();
    registry.add_abstraction_schema::<TestCollisionOne>();
    registry.add_abstraction_schema::<TestCollisionTwo>();

    let err =
        build_wake_tool_projection(&registry.freeze(), &["core/emit_abstraction".to_string()])
            .expect_err("lossy provider-safe name collision must fail projection");

    assert!(
        err.to_string()
            .contains("provider-safe tool name collision")
    );
}

#[test]
fn projection_rejects_reserved_wrapper_fields() {
    let mut registry = FlavorRegistry::new();
    registry.add_abstraction_schema::<TestReservedText>();

    let err =
        build_wake_tool_projection(&registry.freeze(), &["core/emit_abstraction".to_string()])
            .expect_err("reserved wrapper field must fail projection");

    assert!(err.to_string().contains("reserved wrapper field text"));
}

#[test]
fn direct_projection_rejects_unknown_tool_ids() {
    let err = build_wake_tool_projection(
        &FlavorRegistry::new().freeze(),
        &["unknown/tool".to_string()],
    )
    .expect_err("unknown tool id must fail projection");

    assert!(err.to_string().contains("unknown/tool"));
}

#[test]
fn emit_projection_rejects_empty_concrete_schema_set() {
    let err = build_wake_tool_projection(
        &FlavorRegistry::new().freeze(),
        &["core/emit_abstraction".to_string()],
    )
    .expect_err("empty abstraction registry must fail projection");

    assert!(
        err.to_string()
            .contains("no registered Abstraction schemas")
    );
}

#[test]
fn projection_keeps_schema_version_in_dispatch_identity() {
    let mut registry = FlavorRegistry::new();
    registry.add_abstraction_schema::<TestBrief>();
    let projection =
        build_wake_tool_projection(&registry.freeze(), &["core/emit_abstraction".to_string()])
            .expect("projection");

    let schema_id = SchemaId::new("test/brief-v1".to_string());
    let schema_version = SchemaVersion::new(1);
    assert!(projection.iter().any(|tool| {
        matches!(
            &tool.dispatch,
            proxima_core::harness::HarnessToolDispatch::TypedEmit {
                schema_id: id,
                schema_version: version,
                ..
            } if id == schema_id.as_str() && *version == schema_version.into_inner()
        )
    }));
}

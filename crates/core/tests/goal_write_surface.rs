use proxima_core::verbs::goal_write::{
    GoalAssignmentTarget, GoalAuthorship, GoalCreateRequest, GoalPayloadWrite, GoalWakeConfigWrite,
    GoalWakeToolId, GoalWakeTrigger, GoalWriteBuildError, IdempotencyKey,
};
use proxima_core::{
    FlavorRegistry, GoalPayload, MemoryId, Owner, OwnerRef, PayloadKeyBuilder, SchemaId,
    SchemaVersion, UserId,
};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct ProductGoalPayload {
    stable_key: String,
}

impl GoalPayload for ProductGoalPayload {
    const SCHEMA_ID: &'static str = "test/product-goal-v1";
    const SCHEMA_VERSION: u32 = 7;

    fn goal_key(&self) -> Vec<u8> {
        let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_str("stable_key", &self.stable_key);
        key.finish()
    }
}

#[test]
fn typed_goal_payload_write_uses_goal_key_and_sidecar_metadata() {
    let payload = ProductGoalPayload {
        stable_key: "onboarding:first-goal".to_string(),
    };
    let expected_key = payload.goal_key();

    let write = GoalPayloadWrite::from_payload("  First goal  ", "  Learn daily  ", payload)
        .expect("valid typed goal payload write");

    assert_eq!(write.schema_id.as_str(), ProductGoalPayload::SCHEMA_ID);
    assert_eq!(write.schema_version, SchemaVersion::new(7));
    assert_eq!(write.title, "First goal");
    assert_eq!(write.text, "Learn daily");
    assert_eq!(write.payload, expected_key);

    let sidecar = write.sidecar_payload.as_ref().expect(
        "typed product goals carry a sidecar payload for storage backends that registered one",
    );
    assert_eq!(sidecar.schema_id.as_str(), ProductGoalPayload::SCHEMA_ID);
    assert_eq!(sidecar.schema_version, SchemaVersion::new(7));
}

#[test]
fn typed_goal_payload_write_rejects_invalid_display_fields() {
    let err = GoalPayloadWrite::from_payload(
        " ",
        "body",
        ProductGoalPayload {
            stable_key: "k".to_string(),
        },
    )
    .expect_err("empty title rejected");
    assert_eq!(err, GoalWriteBuildError::InvalidTitle);

    let err = GoalPayloadWrite::from_payload(
        "title",
        " ",
        ProductGoalPayload {
            stable_key: "k".to_string(),
        },
    )
    .expect_err("empty text rejected");
    assert_eq!(err, GoalWriteBuildError::InvalidText);
}

#[test]
fn product_goal_create_request_defaults_to_user_authorship_and_explicit_self_target() {
    let owner: Owner = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
    let target_self = MemoryId::new(uuid::Uuid::now_v7());
    let request_id = IdempotencyKey::new("onboarding:initial-goal:1").expect("stable key");

    let target = GoalAssignmentTarget::perspective(target_self);
    let request = GoalCreateRequest::product(
        owner,
        target,
        request_id,
        "Initial goal",
        "Practice every weekday",
        ProductGoalPayload {
            stable_key: "weekday-practice".to_string(),
        },
    );

    assert_eq!(request.principal, owner);
    assert_eq!(request.topology.assignment(), target);
    assert_eq!(request.author_self_perspective_id, None);
    assert!(request.topology.evidence().is_empty());
    assert!(request.topology.dependencies().is_empty());
    assert_eq!(request.authorship, GoalAuthorship::User);
}

#[test]
fn goal_wake_tool_id_requires_leaf_scope_for_grouped_core_tools() {
    let registry = FlavorRegistry::new().freeze();

    let err = GoalWakeToolId::parse("core_goal", &registry)
        .expect_err("grouped action-dispatch tool requires an exact leaf scope key");
    assert!(err.message.contains("leaf action scope required"));

    let leaf = GoalWakeToolId::parse("core_goal:set", &registry)
        .expect("registered leaf action scope key is valid");
    assert_eq!(leaf.as_str(), "core_goal:set");

    let flat = GoalWakeToolId::parse("core_search_memories", &registry)
        .expect("flat registered non-action tool is valid");
    assert_eq!(flat.as_str(), "core_search_memories");
}

#[test]
fn goal_wake_config_normalizes_tool_ids_and_rejects_duplicate_hard_memory() {
    let registry = FlavorRegistry::new().freeze();
    let search = GoalWakeToolId::parse("core_search_memories", &registry).expect("valid tool");
    let goal_set = GoalWakeToolId::parse("core_goal:set", &registry).expect("valid leaf action");
    let hard_memory = MemoryId::new(uuid::Uuid::now_v7());

    let config = GoalWakeConfigWrite::new(
        GoalWakeTrigger::FactSchema {
            schema_id: SchemaId::new("core/agent-note-v1".into()),
            schema_version: SchemaVersion::new(1),
        },
        vec![goal_set.clone(), search.clone(), search],
        "  wake prompt  ",
        &[hard_memory],
    )
    .expect("valid wake config");
    assert_eq!(
        config
            .tool_ids()
            .iter()
            .map(GoalWakeToolId::as_str)
            .collect::<Vec<_>>(),
        ["core_goal:set", "core_search_memories"]
    );
    assert_eq!(config.prompt(), "wake prompt");

    let err = GoalWakeConfigWrite::new(
        GoalWakeTrigger::FactSchema {
            schema_id: SchemaId::new("core/agent-note-v1".into()),
            schema_version: SchemaVersion::new(1),
        },
        vec![goal_set],
        "wake prompt",
        &[hard_memory, hard_memory],
    )
    .expect_err("duplicate hard memory ids are rejected");
    assert!(err.message.contains("duplicate hard memory id"));
}

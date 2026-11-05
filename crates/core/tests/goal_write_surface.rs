use proxiima_core::verbs::goal_write::{
    GoalAuthorship, GoalCreateRequest, GoalMotivationRef, GoalPayloadWrite, GoalTopologyWrite,
    GoalWriteBuildError, IdempotencyKey,
};
use proxiima_core::{
    GoalPayload, MemoryId, OwnerId, PayloadKeyBuilder, TrimmedLenViolation, UserId,
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

impl proxiima_core::PayloadProjection for ProductGoalPayload {
    const KEY_FIELDS: &'static [proxiima_core::KeyField<Self>] = &[];

    fn display(&self) -> proxiima_core::PayloadDisplay {
        proxiima_core::PayloadDisplay::default()
    }

    fn durable(&self) -> Result<proxiima_core::DurablePayload, String> {
        let mut builder = proxiima_core::DurablePayloadBuilder::new();
        builder.field("stable_key", &self.stable_key)?;
        Ok(builder.finish())
    }
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
    assert_eq!(
        err,
        GoalWriteBuildError::InvalidTitle(TrimmedLenViolation::Blank)
    );

    let err = GoalPayloadWrite::from_payload(
        "title",
        " ",
        ProductGoalPayload {
            stable_key: "k".to_string(),
        },
    )
    .expect_err("empty text rejected");
    assert_eq!(
        err,
        GoalWriteBuildError::InvalidText(TrimmedLenViolation::Blank)
    );
}

#[test]
fn embedded_host_rejections_name_the_bound() {
    let payload = || ProductGoalPayload {
        stable_key: "k".to_string(),
    };
    let title_blank = GoalPayloadWrite::from_payload("  ", "text", payload())
        .expect_err("blank title")
        .to_string();
    let title_long = GoalPayloadWrite::from_payload("a".repeat(241), "text", payload())
        .expect_err("long title")
        .to_string();
    assert!(title_blank.starts_with("goal title"));
    assert!(title_long.starts_with("goal title"));

    let key_blank = IdempotencyKey::new("  ").expect_err("blank key");
    let key_long =
        IdempotencyKey::new("a".repeat(IdempotencyKey::MAX_CHARS + 1)).expect_err("long key");
    assert!(key_blank.starts_with("idempotency_key"));
    assert!(key_long.starts_with("idempotency_key"));
}

#[test]
fn product_goal_create_request_defaults_to_user_authorship_and_explicit_self_target() {
    let owner: OwnerId = OwnerId::from(UserId::new(uuid::Uuid::now_v7()));
    let target_self = MemoryId::new(uuid::Uuid::now_v7());
    let request_id = IdempotencyKey::new("onboarding:initial-goal:1").expect("stable key");
    let target = GoalMotivationRef::perspective(target_self);
    let request = GoalCreateRequest::product(
        owner,
        vec![target],
        request_id,
        "Initial goal",
        "Practice every weekday",
        ProductGoalPayload {
            stable_key: "weekday-practice".to_string(),
        },
    );
    assert_eq!(request.owner, owner);
    assert_eq!(request.topology.motivated_by(), &[target]);
    assert_eq!(request.author_self_perspective_id, None);
    assert!(request.topology.evidence().is_empty());
    assert!(request.topology.dependencies().is_empty());
    assert_eq!(request.authorship, GoalAuthorship::User);
}

#[test]
fn goal_topology_normalizes_duplicate_perspective_motivations() {
    let perspective = GoalMotivationRef::perspective(MemoryId::new(uuid::Uuid::now_v7()));
    let topology = GoalTopologyWrite::new(vec![perspective, perspective], Vec::new(), Vec::new())
        .expect("duplicate motivations normalize");
    assert_eq!(topology.motivated_by(), &[perspective]);
}

#[test]
fn goal_topology_deserialization_uses_the_validating_constructor() {
    let goal_id = uuid::Uuid::now_v7();
    let wire = serde_json::json!({
        "motivated_by": [],
        "dependencies": [
            {"goal_id": goal_id},
            {"goal_id": goal_id}
        ],
        "evidence": []
    });

    let error = serde_json::from_value::<GoalTopologyWrite>(wire)
        .expect_err("duplicate dependencies must be refused at deserialization");
    assert!(error.to_string().contains("duplicate goal dependency"));
}

#[test]
fn goal_topology_deserialization_refuses_duplicate_evidence() {
    let memory_id = uuid::Uuid::now_v7();
    let wire = serde_json::json!({
        "motivated_by": [],
        "dependencies": [],
        "evidence": [
            {"memory_id": memory_id},
            {"memory_id": memory_id}
        ]
    });

    let error = serde_json::from_value::<GoalTopologyWrite>(wire)
        .expect_err("duplicate evidence must be refused at deserialization");
    assert!(error.to_string().contains("duplicate goal evidence"));
}

#[test]
fn goal_topology_deserialization_preserves_motivation_normalization() {
    let perspective_id = uuid::Uuid::now_v7();
    let wire = serde_json::json!({
        "motivated_by": [
            {"perspective_id": perspective_id},
            {"perspective_id": perspective_id}
        ],
        "dependencies": [],
        "evidence": []
    });

    let topology = serde_json::from_value::<GoalTopologyWrite>(wire)
        .expect("duplicate motivations are normalized");
    assert_eq!(topology.motivated_by().len(), 1);
    assert_eq!(
        topology.motivated_by()[0].perspective_id(),
        MemoryId::new(perspective_id)
    );
}

#[test]
fn goal_topology_serialization_round_trips_through_the_wire_shape() {
    let perspective = GoalMotivationRef::perspective(MemoryId::new(uuid::Uuid::now_v7()));
    let topology =
        GoalTopologyWrite::new(vec![perspective], Vec::new(), Vec::new()).expect("valid topology");
    let wire = serde_json::to_value(&topology).expect("serialize topology");
    let decoded: GoalTopologyWrite =
        serde_json::from_value(wire).expect("deserialize valid topology");
    assert_eq!(decoded, topology);
}

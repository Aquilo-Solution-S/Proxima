use proxima_core::verbs::goal_write::{
    GoalAuthorship, GoalCreateRequest, GoalPayloadWrite, GoalWriteBuildError, IdempotencyKey,
};
use proxima_core::{
    GoalPayload, MemoryId, Owner, OwnerRef, PayloadKeyBuilder, SchemaVersion, UserId,
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

    let request = GoalCreateRequest::product(
        owner,
        target_self,
        request_id,
        "Initial goal",
        "Practice every weekday",
        ProductGoalPayload {
            stable_key: "weekday-practice".to_string(),
        },
    );

    assert_eq!(request.principal, owner);
    assert_eq!(request.target_self_perspective_id, target_self);
    assert_eq!(request.author_self_perspective_id, None);
    assert!(request.evidence.is_empty());
    assert!(request.parent_goal_ids.is_empty());
    assert_eq!(request.authorship, GoalAuthorship::User);
}

use proxima::flavor::{GoalPayload, PayloadKeyBuilder};
use proxima::{
    GoalAssignmentTarget, GoalAuthorship, GoalCreateRequest, GoalPayloadWrite, IdempotencyKey,
    MemoryId, OwnerRef, SystemOrigin,
};
use proxima_core::{ToolId, UserId};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct FacadeGoalPayload {
    key: String,
}

impl GoalPayload for FacadeGoalPayload {
    const SCHEMA_ID: &'static str = "test/facade-goal-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn goal_key(&self) -> Vec<u8> {
        let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_str("key", &self.key);
        key.finish()
    }
}

#[test]
fn facade_reexports_typed_goalwrite_surface_for_embedded_hosts() {
    let owner = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
    let target_self = MemoryId::new(uuid::Uuid::now_v7());

    let write = GoalPayloadWrite::from_payload(
        "First goal",
        "Practice daily",
        FacadeGoalPayload {
            key: "daily-practice".to_string(),
        },
    )
    .expect("typed payload encodes through facade re-export");
    assert_eq!(write.schema_id.as_str(), FacadeGoalPayload::SCHEMA_ID);

    let request = GoalCreateRequest::product(
        owner,
        GoalAssignmentTarget::perspective(target_self),
        IdempotencyKey::new("product:daily-practice").expect("stable product request id is valid"),
        "First goal",
        "Practice daily",
        FacadeGoalPayload {
            key: "daily-practice".to_string(),
        },
    );
    assert_eq!(request.owner, owner);
    assert_eq!(request.topology.assignment().perspective_id(), target_self);

    let system_request = request.with_authorship(GoalAuthorship::System(SystemOrigin::Tool {
        tool_id: ToolId::new("product/onboarding"),
    }));
    assert!(matches!(
        system_request.authorship,
        GoalAuthorship::System(_)
    ));
}

use std::sync::Arc;

use proxima_core::mcp::{HandleTable, McpToolCaller, McpToolPresentation, OutputMode};
use proxima_core::{
    AuthPath, AuthzContext, FlavorRegistry, GroupId, MemoryId, OwnerRef, ToolCtx, ToolServices,
    UserId,
};
use uuid::Uuid;

use crate::payloads::{AcceptanceCriterionV1, AcceptanceVerifierKind, AcceptanceVerifierSpecV1};

use super::input_validation::{resolve_evidence, validate_plan_items};
use super::plan_persistence::execution_plan_memory_id;
use super::types::{ExecutionPlanItemArgs, ExecutionPlanItemKind};

/// Pins the org-free execution-plan `MemoryId` against drift. The v5 key folds
/// the owner *principal* id ‖ repo ‖ goal
/// memory ‖ plan key — no org. A fixed input must reproduce exactly
/// this uuid so re-issued plans stay idempotent.
#[test]
fn execution_plan_memory_id_golden_is_org_free() {
    let owner = OwnerRef::Personal(UserId::new(
        Uuid::parse_str("00000000-0000-0000-0000-000000000001").expect("uuid literal"),
    ));
    let repo_id = Uuid::parse_str("00000000-0000-0000-0000-0000000000aa").expect("uuid literal");
    let goal_activated = MemoryId::new(
        Uuid::parse_str("00000000-0000-0000-0000-0000000000bb").expect("uuid literal"),
    );
    let id = execution_plan_memory_id(&owner, repo_id, goal_activated, "plan:golden");
    assert_eq!(
        id.into_inner(),
        Uuid::parse_str("ec0bf05d-c797-559d-bdf8-9583028201cf").expect("uuid literal")
    );
}

fn test_ctx(handles: Arc<HandleTable>) -> ToolCtx {
    let owner = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
    let mut services = ToolServices::new();
    services.insert(McpToolPresentation::new(Some(handles), OutputMode::Handles));
    services.insert(McpToolCaller::new("test/model".into()));
    ToolCtx::new(
        owner,
        AuthzContext::single_owner(&owner, AuthPath::HostBearer),
        Arc::new(FlavorRegistry::new().freeze_or_panic_for_tests()),
        services,
    )
}

#[tokio::test]
async fn execution_request_evidence_accepts_only_fact_handles() {
    let handles = Arc::new(HandleTable::new());
    let fact = MemoryId::new(Uuid::now_v7());
    let abstraction = MemoryId::new(Uuid::now_v7());
    let fact_handle = handles.assign_fact_memory(fact).as_str().to_string();
    let abstraction_handle = handles
        .assign_abstraction_memory(abstraction)
        .as_str()
        .to_string();
    let ctx = test_ctx(handles);

    assert_eq!(
        resolve_evidence(&ctx, &[fact_handle]).expect("fact evidence"),
        vec![fact]
    );
    let err = resolve_evidence(&ctx, &[abstraction_handle]).expect_err("A handle rejected");
    assert!(
        err.to_string().contains("expected Fact memory handle"),
        "{err}"
    );
}

fn criterion(key: &str, required: bool) -> AcceptanceCriterionV1 {
    AcceptanceCriterionV1 {
        key: key.into(),
        description: format!("{key} passes"),
        required,
        verifier_kind: AcceptanceVerifierKind::Command,
        verifier_spec: AcceptanceVerifierSpecV1 {
            path: None,
            command: Some(vec!["true".into()]),
            pattern: None,
            note: None,
        },
    }
}

#[test]
fn validate_plan_items_accepts_mixed_implementation_and_test_nodes() {
    let items = validate_plan_items(vec![
        ExecutionPlanItemArgs {
            kind: ExecutionPlanItemKind::Implementation,
            key: "impl".into(),
            title: "Implement".into(),
            instructions: "Create the feature.".into(),
            idempotency_key: "impl-key".into(),
            depends_on: vec![],
            acceptance_criteria: vec![criterion("build", true)],
            test_criteria: vec![],
        },
        ExecutionPlanItemArgs {
            kind: ExecutionPlanItemKind::Test,
            key: "test".into(),
            title: "Test".into(),
            instructions: "Verify the feature.".into(),
            idempotency_key: "test-key".into(),
            depends_on: vec!["impl".into()],
            acceptance_criteria: vec![],
            test_criteria: vec![criterion("smoke", true)],
        },
    ])
    .expect("mixed plan validates");

    assert_eq!(items[0].kind, ExecutionPlanItemKind::Implementation);
    assert_eq!(items[1].kind, ExecutionPlanItemKind::Test);
    assert_eq!(items[1].depends_on, vec!["impl"]);
}

#[test]
fn validate_plan_items_rejects_test_without_required_criteria() {
    let err = validate_plan_items(vec![ExecutionPlanItemArgs {
        kind: ExecutionPlanItemKind::Test,
        key: "test".into(),
        title: "Test".into(),
        instructions: "Verify the feature.".into(),
        idempotency_key: "test-key".into(),
        depends_on: vec![],
        acceptance_criteria: vec![],
        test_criteria: vec![criterion("optional", false)],
    }])
    .expect_err("test must require one criterion");

    assert!(
        err.to_string()
            .contains("must include at least one required test criterion"),
        "{err}"
    );
}

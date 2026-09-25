use std::sync::Arc;

use proxima_core::mcp::{McpToolPresentation, PrefixedUuidClass, format_prefixed_uuid};
use proxima_core::{
    AuthPath, AuthzContext, FlavorRegistry, GroupId, MemoryId, OwnerRef, ToolCaller, ToolCtx,
    ToolServices,
};
use uuid::Uuid;

use crate::payloads::{AcceptanceCriterionV1, AcceptanceVerifierKind, AcceptanceVerifierSpecV1};

use super::input_validation::{resolve_evidence, validate_plan_items};
use super::types::{ExecutionPlanItemArgs, ExecutionPlanItemKind};

fn test_ctx() -> ToolCtx {
    let owner = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
    let mut services = ToolServices::new();
    services.insert(McpToolPresentation::new());
    ToolCtx::new(
        owner,
        AuthzContext::single_owner(&owner, AuthPath::HostBearer),
        Arc::new(FlavorRegistry::new().freeze_or_panic_for_tests()),
        services,
    )
    .with_caller(Some(ToolCaller::new("test/model", "test", "0")))
}

#[tokio::test]
async fn execution_request_evidence_accepts_only_fact_references() {
    let fact = MemoryId::new(Uuid::now_v7());
    let abstraction = MemoryId::new(Uuid::now_v7());
    let fact_handle = format_prefixed_uuid(fact.into_inner(), PrefixedUuidClass::Fact);
    let abstraction_handle =
        format_prefixed_uuid(abstraction.into_inner(), PrefixedUuidClass::Abstraction);
    let ctx = test_ctx();

    assert_eq!(
        resolve_evidence(&ctx, &[fact_handle]).expect("fact evidence"),
        vec![fact]
    );
    let err = resolve_evidence(&ctx, &[abstraction_handle]).expect_err("A reference rejected");
    assert!(err.to_string().contains("expected Fact id"), "{err}");
    assert!(err.to_string().contains("got prefix 'A'"), "{err}");
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

use uuid::Uuid;

use crate::payloads::{AcceptanceCriterionV1, AcceptanceVerifierKind, AcceptanceVerifierSpecV1};

use super::input_validation::{resolve_evidence, validate_plan_items};
use super::types::{ExecutionPlanItemArgs, ExecutionPlanItemKind};

#[test]
fn execution_request_evidence_rejects_prefixed_references_before_storage() {
    for prefix in ['F', 'A', 'P', 'G'] {
        let raw = format!("{prefix}:{}", Uuid::now_v7());
        let err = resolve_evidence(&[raw]).expect_err("bare UUID required");
        assert!(
            matches!(err, proxima_core::ToolError::InvalidInput(_)),
            "{err}"
        );
    }
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

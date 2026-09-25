use proxima_core::{
    EntityKind, InputContractId, MemoryId, MemoryOutputInvocation, OperatorId,
    OperatorInvocationManifest, OperatorPhase, OutputEdgeManifest, SchemaId, SchemaVersion,
};
use uuid::Uuid;

fn memory_id() -> MemoryId {
    MemoryId::new(Uuid::now_v7())
}

fn contract_id(seed: &str) -> InputContractId {
    InputContractId::new(Uuid::new_v5(&Uuid::NAMESPACE_URL, seed.as_bytes()))
}

#[test]
fn ftoa_manifest_rejects_missing_declared_input_edge() {
    let input = memory_id();
    let output = memory_id();
    let manifest = OperatorInvocationManifest::memory_output(MemoryOutputInvocation {
        phase: OperatorPhase::FtoA,
        operator_id: OperatorId::new(Uuid::now_v7()),
        input_contract_id: contract_id("test/ftoa"),
        inputs: vec![(input, EntityKind::Fact)],
        output_memory_id: output,
        output_kind: EntityKind::Abstraction,
        schema_id: SchemaId::new("proxima-core/agent-derivation".to_string()),
        schema_version: SchemaVersion::new(1),
        output_edges: Vec::new(),
    });

    let err = manifest
        .validate()
        .expect_err("declared input requires output→input ledger edge");
    assert!(err.to_string().contains("missing provenance edge"));
}

#[test]
fn atog_manifest_rejects_goal_output_for_memory_phase() {
    let input = memory_id();
    let goal = proxima_core::GoalId::new(Uuid::now_v7());
    let manifest = OperatorInvocationManifest::goal_output(
        OperatorPhase::AtoP,
        OperatorId::new(Uuid::now_v7()),
        contract_id("test/bad"),
        vec![(input, EntityKind::Abstraction)],
        goal,
        vec![OutputEdgeManifest::goal_to_memory(goal, input)],
    );

    let err = manifest
        .validate()
        .expect_err("only A→Goal may output Goals");
    assert!(err.to_string().contains("phase cannot output goal"));
}

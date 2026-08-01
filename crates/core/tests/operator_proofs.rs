use proxima_core::{
    EntityKind, InputContractId, MemoryId, MemoryOperatorKind, MemoryOutputInvocation, OperatorId,
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
fn input_contract_id_is_opaque_uuid_newtype() {
    let uuid = Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        b"core/derive:ftoa-agent-derivation-v1",
    );
    let id = InputContractId::new(uuid);

    assert_eq!(id.into_inner(), uuid);
}

#[test]
fn memory_operator_kind_is_closed_to_lean_memory_output_phases() {
    assert_eq!(MemoryOperatorKind::FtoA.phase(), OperatorPhase::FtoA);
    assert_eq!(MemoryOperatorKind::AtoA.phase(), OperatorPhase::AtoA);
    assert_eq!(MemoryOperatorKind::AtoP.phase(), OperatorPhase::AtoP);
}

#[test]
fn ftoa_manifest_requires_fact_inputs_abstraction_output_and_complete_edges() {
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
        output_edges: vec![OutputEdgeManifest::memory_to_memory(output, input)],
    });

    manifest.validate().expect("manifest is valid");
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
fn atogoal_manifest_requires_abstraction_inputs_and_structural_edges() {
    let input = memory_id();
    let goal = proxima_core::GoalId::new(Uuid::now_v7());
    let manifest = OperatorInvocationManifest::goal_output(
        OperatorPhase::AtoGoal,
        OperatorId::new(Uuid::now_v7()),
        contract_id("test/atogoal"),
        vec![(input, EntityKind::Abstraction)],
        goal,
        vec![OutputEdgeManifest::goal_to_memory(goal, input)],
    );

    manifest.validate().expect("A→Goal manifest is valid");
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

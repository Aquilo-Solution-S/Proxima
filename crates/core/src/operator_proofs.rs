//! Runtime carriers for Lean `OperatorInvocation` proof obligations.
//!
//! These types do not execute operators. They describe the persisted ledger shape
//! that proves an admitted operator output has the expected input/output kinds
//! and output→input edges.

use std::collections::BTreeSet;

use crate::{
    EdgeAuthorshipKind, EntityKind, GoalId, InputContractId, MemoryId, OperatorId, SchemaId,
    SchemaVersion,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum OperatorPhase {
    FtoA,
    AtoA,
    AtoP,
    AtoGoal,
}

impl OperatorPhase {
    #[must_use]
    pub const fn input_kind(self) -> EntityKind {
        match self {
            Self::FtoA => EntityKind::Fact,
            Self::AtoA | Self::AtoP | Self::AtoGoal => EntityKind::Abstraction,
        }
    }

    #[must_use]
    pub const fn output_memory_kind(self) -> Option<EntityKind> {
        match self {
            Self::FtoA | Self::AtoA => Some(EntityKind::Abstraction),
            Self::AtoP => Some(EntityKind::Perspective),
            Self::AtoGoal => None,
        }
    }

    #[must_use]
    pub const fn edge_authorship(self) -> EdgeAuthorshipKind {
        match self {
            Self::FtoA => EdgeAuthorshipKind::OperatorFtoA,
            Self::AtoA => EdgeAuthorshipKind::OperatorAtoA,
            Self::AtoP => EdgeAuthorshipKind::OperatorAtoP,
            Self::AtoGoal => EdgeAuthorshipKind::OperatorAtoGoal,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OperatorInputManifest {
    pub memory_id: MemoryId,
    pub kind: EntityKind,
}

impl OperatorInputManifest {
    #[must_use]
    pub const fn new(memory_id: MemoryId, kind: EntityKind) -> Self {
        Self { memory_id, kind }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum OperatorOutputManifest {
    Memory {
        memory_id: MemoryId,
        kind: EntityKind,
        schema_id: SchemaId,
        schema_version: SchemaVersion,
    },
    Goal {
        goal_id: GoalId,
    },
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum OutputNodeRef {
    Memory(MemoryId),
    Goal(GoalId),
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OutputEdgeManifest {
    pub source: OutputNodeRef,
    pub target_memory_id: MemoryId,
    pub authorship_kind: EdgeAuthorshipKind,
}

impl OutputEdgeManifest {
    #[must_use]
    pub const fn memory_to_memory(
        source_memory_id: MemoryId,
        target_memory_id: MemoryId,
        authorship_kind: EdgeAuthorshipKind,
    ) -> Self {
        Self {
            source: OutputNodeRef::Memory(source_memory_id),
            target_memory_id,
            authorship_kind,
        }
    }

    #[must_use]
    pub const fn goal_to_memory(
        source_goal_id: GoalId,
        target_memory_id: MemoryId,
        authorship_kind: EdgeAuthorshipKind,
    ) -> Self {
        Self {
            source: OutputNodeRef::Goal(source_goal_id),
            target_memory_id,
            authorship_kind,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OperatorInvocationManifest {
    pub phase: OperatorPhase,
    pub operator_id: OperatorId,
    pub input_contract_id: InputContractId,
    pub inputs: Vec<OperatorInputManifest>,
    pub outputs: Vec<OperatorOutputManifest>,
    pub output_edges: Vec<OutputEdgeManifest>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryOutputInvocation {
    pub phase: OperatorPhase,
    pub operator_id: OperatorId,
    pub input_contract_id: InputContractId,
    pub inputs: Vec<(MemoryId, EntityKind)>,
    pub output_memory_id: MemoryId,
    pub output_kind: EntityKind,
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub output_edges: Vec<OutputEdgeManifest>,
}

impl OperatorInvocationManifest {
    #[must_use]
    pub fn memory_output(invocation: MemoryOutputInvocation) -> Self {
        Self {
            phase: invocation.phase,
            operator_id: invocation.operator_id,
            input_contract_id: invocation.input_contract_id,
            inputs: invocation
                .inputs
                .into_iter()
                .map(|(memory_id, kind)| OperatorInputManifest::new(memory_id, kind))
                .collect(),
            outputs: vec![OperatorOutputManifest::Memory {
                memory_id: invocation.output_memory_id,
                kind: invocation.output_kind,
                schema_id: invocation.schema_id,
                schema_version: invocation.schema_version,
            }],
            output_edges: invocation.output_edges,
        }
    }

    #[must_use]
    pub fn goal_output(
        phase: OperatorPhase,
        operator_id: OperatorId,
        input_contract_id: InputContractId,
        inputs: Vec<(MemoryId, EntityKind)>,
        output_goal_id: GoalId,
        output_edges: Vec<OutputEdgeManifest>,
    ) -> Self {
        Self {
            phase,
            operator_id,
            input_contract_id,
            inputs: inputs
                .into_iter()
                .map(|(memory_id, kind)| OperatorInputManifest::new(memory_id, kind))
                .collect(),
            outputs: vec![OperatorOutputManifest::Goal {
                goal_id: output_goal_id,
            }],
            output_edges,
        }
    }

    /// # Errors
    ///
    /// Returns an [`OperatorProofError`] when the manifest violates Lean's
    /// phase-local shape or output×input ledger completeness obligations.
    pub fn validate(&self) -> Result<(), OperatorProofError> {
        if self.inputs.is_empty() {
            return Err(OperatorProofError::EmptyInputs);
        }
        if self.outputs.is_empty() {
            return Err(OperatorProofError::EmptyOutputs);
        }

        let mut seen_inputs = BTreeSet::new();
        for input in &self.inputs {
            if !seen_inputs.insert(input.memory_id) {
                return Err(OperatorProofError::DuplicateInput {
                    memory_id: input.memory_id,
                });
            }
            let expected = self.phase.input_kind();
            if input.kind != expected {
                return Err(OperatorProofError::InvalidInputKind {
                    memory_id: input.memory_id,
                    expected,
                    actual: input.kind,
                });
            }
        }

        let expected_edge_authorship = self.phase.edge_authorship();
        for edge in &self.output_edges {
            if edge.authorship_kind != expected_edge_authorship {
                return Err(OperatorProofError::InvalidEdgeAuthorship {
                    expected: expected_edge_authorship,
                    actual: edge.authorship_kind,
                });
            }
        }

        for output in &self.outputs {
            match output {
                OperatorOutputManifest::Memory {
                    memory_id, kind, ..
                } => {
                    let Some(expected) = self.phase.output_memory_kind() else {
                        return Err(OperatorProofError::PhaseCannotOutputMemory {
                            phase: self.phase,
                        });
                    };
                    if *kind != expected {
                        return Err(OperatorProofError::InvalidOutputKind {
                            memory_id: *memory_id,
                            expected,
                            actual: *kind,
                        });
                    }
                    for input in &self.inputs {
                        if !self.output_edges.iter().any(|edge| {
                            edge.source == OutputNodeRef::Memory(*memory_id)
                                && edge.target_memory_id == input.memory_id
                                && edge.authorship_kind == expected_edge_authorship
                        }) {
                            return Err(OperatorProofError::MissingProvenanceEdge {
                                output_memory_id: *memory_id,
                                input_memory_id: input.memory_id,
                            });
                        }
                    }
                }
                OperatorOutputManifest::Goal { goal_id } => {
                    if self.phase != OperatorPhase::AtoGoal {
                        return Err(OperatorProofError::PhaseCannotOutputGoal {
                            phase: self.phase,
                        });
                    }
                    for input in &self.inputs {
                        if !self.output_edges.iter().any(|edge| {
                            edge.source == OutputNodeRef::Goal(*goal_id)
                                && edge.target_memory_id == input.memory_id
                                && edge.authorship_kind == expected_edge_authorship
                        }) {
                            return Err(OperatorProofError::MissingEvidenceEdge {
                                output_goal_id: *goal_id,
                                input_memory_id: input.memory_id,
                            });
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum OperatorProofError {
    #[error("operator invocation inputs must be nonempty")]
    EmptyInputs,
    #[error("operator invocation outputs must be nonempty")]
    EmptyOutputs,
    #[error("duplicate operator input {memory_id:?}")]
    DuplicateInput { memory_id: MemoryId },
    #[error("invalid input kind for {memory_id:?}: expected {expected:?}, got {actual:?}")]
    InvalidInputKind {
        memory_id: MemoryId,
        expected: EntityKind,
        actual: EntityKind,
    },
    #[error("invalid output kind for {memory_id:?}: expected {expected:?}, got {actual:?}")]
    InvalidOutputKind {
        memory_id: MemoryId,
        expected: EntityKind,
        actual: EntityKind,
    },
    #[error("phase cannot output memory: {phase:?}")]
    PhaseCannotOutputMemory { phase: OperatorPhase },
    #[error("phase cannot output goal: {phase:?}")]
    PhaseCannotOutputGoal { phase: OperatorPhase },
    #[error("invalid edge authorship: expected {expected:?}, got {actual:?}")]
    InvalidEdgeAuthorship {
        expected: EdgeAuthorshipKind,
        actual: EdgeAuthorshipKind,
    },
    #[error(
        "missing provenance edge from {output_memory_id:?} to declared input {input_memory_id:?}"
    )]
    MissingProvenanceEdge {
        output_memory_id: MemoryId,
        input_memory_id: MemoryId,
    },
    #[error("missing evidence edge from {output_goal_id:?} to declared input {input_memory_id:?}")]
    MissingEvidenceEdge {
        output_goal_id: GoalId,
        input_memory_id: MemoryId,
    },
}

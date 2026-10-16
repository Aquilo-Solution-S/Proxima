//! Shared entity-kind and endpoint types used by graph and embedding queries.

use uuid::Uuid;

use crate::{GoalId, MemoryId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum EntityKind {
    Fact,
    Abstraction,
    Perspective,
    Goal,
}

impl EntityKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fact => "Fact",
            Self::Abstraction => "Abstraction",
            Self::Perspective => "Perspective",
            Self::Goal => "Goal",
        }
    }
}

/// Entity kinds admitted to the vector infrastructure.
///
/// Fact entities and edges are not embeddable. Memory embeddings keep the
/// memory layer explicit; Goal embeddings use the Goal id directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum EmbeddableEntityRef {
    Memory {
        kind: EntityKind,
        memory_id: MemoryId,
    },
    Goal(GoalId),
}

impl EmbeddableEntityRef {
    #[must_use]
    pub const fn entity_kind(self) -> EntityKind {
        match self {
            Self::Memory { kind, .. } => kind,
            Self::Goal(_) => EntityKind::Goal,
        }
    }

    #[must_use]
    pub const fn entity_id(self) -> Uuid {
        match self {
            Self::Memory { memory_id, .. } => memory_id.into_inner(),
            Self::Goal(goal_id) => goal_id.into_inner(),
        }
    }
}

/// Tags the operator that produced a derived memory (Abstraction /
/// Perspective). Stored as text; there is no SQL enum behind it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum MemoryOperatorKind {
    FtoA,
    AtoA,
    AtoP,
}

impl MemoryOperatorKind {
    #[must_use]
    pub const fn phase(self) -> crate::OperatorPhase {
        match self {
            Self::FtoA => crate::OperatorPhase::FtoA,
            Self::AtoA => crate::OperatorPhase::AtoA,
            Self::AtoP => crate::OperatorPhase::AtoP,
        }
    }
}

/// Endpoint of a pin: a memory `t` or a Goal `t`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum EntityRef {
    Memory(MemoryId),
    Goal(GoalId),
}

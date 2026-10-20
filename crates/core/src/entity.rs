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

/// The kinds a memory row can be: an `EntityKind` without the Goal, which is
/// its own entity. Serialized as the lowercase words the database stores.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum MemoryKind {
    #[default]
    Fact,
    Abstraction,
    Perspective,
}

impl MemoryKind {
    /// The lowercase word the database stores and the wire carries.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fact => "fact",
            Self::Abstraction => "abstraction",
            Self::Perspective => "perspective",
        }
    }

    #[must_use]
    pub const fn entity_kind(self) -> EntityKind {
        match self {
            Self::Fact => EntityKind::Fact,
            Self::Abstraction => EntityKind::Abstraction,
            Self::Perspective => EntityKind::Perspective,
        }
    }

    /// The memory kind of an entity kind; a Goal is not a memory.
    #[must_use]
    pub const fn of(kind: EntityKind) -> Option<Self> {
        match kind {
            EntityKind::Fact => Some(Self::Fact),
            EntityKind::Abstraction => Some(Self::Abstraction),
            EntityKind::Perspective => Some(Self::Perspective),
            EntityKind::Goal => None,
        }
    }
}

impl From<MemoryKind> for EntityKind {
    fn from(kind: MemoryKind) -> Self {
        kind.entity_kind()
    }
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

#[cfg(test)]
mod memory_kind_tests {
    use super::*;

    #[test]
    fn memory_kind_keeps_the_words_the_database_stores() {
        for (kind, word) in [
            (MemoryKind::Fact, "fact"),
            (MemoryKind::Abstraction, "abstraction"),
            (MemoryKind::Perspective, "perspective"),
        ] {
            assert_eq!(kind.as_str(), word);
            let json = serde_json::to_string(&kind).expect("serializes");
            assert_eq!(json, format!("\"{word}\""));
            assert_eq!(serde_json::from_str::<MemoryKind>(&json).ok(), Some(kind));
        }
        assert_eq!(MemoryKind::default(), MemoryKind::Fact);
        assert!(serde_json::from_str::<MemoryKind>("\"goal\"").is_err());
        assert_eq!(MemoryKind::of(EntityKind::Goal), None);
    }
}

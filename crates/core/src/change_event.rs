//! Typed `ChangeEvent` — the hydrated form of an `announce` row
//! (`EntityAppend`, `EntityDelete`, or `EntityTransfer`), returned by the
//! pull reads (`ChangeHistory` / `list_change_events_*`). The LISTEN/NOTIFY
//! Subscribe push path was retired — the log is pull-only.
//! See docs/14 §`ChangeHistory` and §Consistency.
//!
//! Pins are not announced. They are columns on the node row, so a pin
//! change *is* the node append that carries it — there is no separate
//! edge event to emit.

use uuid::Uuid;

use crate::{GoalId, MemoryId, Owner, SchemaId, SchemaVersion};

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

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ChangeEventKind {
    EntityAppend {
        entity_kind: EntityKind,
        entity: EntityRef,
        schema_id: SchemaId,
        schema_version: SchemaVersion,
    },
    EntityDelete {
        entity_kind: EntityKind,
        entity: EntityRef,
        schema_id: SchemaId,
        schema_version: SchemaVersion,
    },
    /// Publish-to-World owner transfer of a memory series. Written in pairs
    /// under both lanes — the prior owner's (the series left their owned
    /// view) and World's (it arrived) — in the transferring transaction.
    EntityTransfer {
        entity_kind: EntityKind,
        entity: EntityRef,
        schema_id: SchemaId,
        schema_version: SchemaVersion,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ChangeEvent {
    pub seq: Uuid,
    pub owner: Owner,
    pub kind: ChangeEventKind,
}

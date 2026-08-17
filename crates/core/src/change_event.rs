//! Typed `ChangeEvent` — the hydrated form of a `change_event` row
//! (`EntityAppend`, `EntityDelete`, `EdgeAppend`, or `EdgeDelete`), returned by the
//! pull reads (`ChangeHistory` / `list_change_events_*`). The LISTEN/NOTIFY
//! Subscribe push path was retired — `change_event` is a pull-only log.
//! See docs/14 §`ChangeHistory` and §Consistency.

use uuid::Uuid;

use crate::edge::{EdgeEndpoint, EdgeKind, EdgeTargetProjection};
use crate::{GoalId, MemoryId, Owner, SchemaId, SchemaVersion};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, sqlx::Type,
)]
#[sqlx(type_name = "proxima_core.entity_kind")]
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

/// Rust mirror of `proxima_core.memory_operator_kind`. Tags the operator
/// that produced a derived memory (Abstraction / Perspective) and is also
/// stored on Goal authorship rows. Variants match the SQL enum labels.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, sqlx::Type,
)]
#[sqlx(type_name = "proxima_core.memory_operator_kind")]
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

/// Discriminant tag for `ChangeEventKind`, mirrors the SQL enum
/// `proxima_core.change_event_kind`. The rich `ChangeEventKind`
/// carries payload; this tag is what the `change_event.kind` column
/// stores and what `FromRow` decoders see.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, sqlx::Type,
)]
#[sqlx(type_name = "proxima_core.change_event_kind")]
pub enum ChangeEventKindTag {
    EntityAppend,
    EntityDelete,
    EdgeAppend,
    EdgeDelete,
}

/// Endpoint of an Edge or supersedes target: a memory `t` or a Goal `t`.
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
        supersedes: Option<EntityRef>,
    },
    EntityDelete {
        entity_kind: EntityKind,
        entity: EntityRef,
        schema_id: SchemaId,
        schema_version: SchemaVersion,
    },
    /// One index row asserted. There is no edge id and no relation to
    /// report — the row's content is its identity (docs/16 §The edge
    /// table is an index).
    EdgeAppend {
        source: EdgeEndpoint,
        target: EdgeTargetProjection,
        kind: EdgeKind,
    },
    EdgeDelete {
        source: EdgeEndpoint,
        target: EdgeTargetProjection,
        kind: EdgeKind,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ChangeEvent {
    pub seq: Uuid,
    pub owner: Owner,
    pub kind: ChangeEventKind,
}

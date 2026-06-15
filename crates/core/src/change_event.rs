//! Typed `ChangeEvent` — the hydrated form of a `change_event` row
//! (`EntityAppend`, `EntityDelete`, or `EdgeAppend`), returned by the
//! pull reads (`EventHistory` / `list_change_events_*`). The LISTEN/NOTIFY
//! Subscribe push path was retired — `change_event` is a pull-only log.
//! See docs/14 §`EventHistory` and §Consistency.

use uuid::Uuid;

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

/// Rust mirror of `proxima_core.memory_operator_kind`. Tags the operator
/// that produced a derived memory (Abstraction / Perspective) and is also
/// stored on Goal authorship rows. Variants match the SQL enum labels.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, sqlx::Type,
)]
#[sqlx(type_name = "proxima_core.memory_operator_kind")]
pub enum MemoryOperatorKind {
    FtoA,
    AtoP,
    ExternalAgent,
    Wake,
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
}

/// Endpoint of an Edge or supersedes target. Sum type matching
/// `change_event` columns: a memory or a goal.
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
    EdgeAppend {
        edge_id: Uuid,
        relation: String,
        source: EntityRef,
        target: EntityRef,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ChangeEvent {
    pub seq: Uuid,
    pub owner: Owner,
    pub kind: ChangeEventKind,
    /// `Some(...)` when an in-process personality authored this event;
    /// `None` for external/event-source ingestions.
    #[serde(default)]
    pub authoring_personality_instance_id: Option<Uuid>,
    /// Wake-chain depth at the time the row was authored. `0` for
    /// external events; `max(provenance.depth) + 1` for personality
    /// authoring (capped per `MAX_WAKE_CHAIN_DEPTH`).
    #[serde(default)]
    pub wake_chain_depth: u16,
}

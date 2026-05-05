//! Typed `ChangeEvent` — what subscribers see when an
//! EntityAppend or EdgeAppend lands. See docs/14
//! §Subscribe and §Consistency.

use uuid::Uuid;

use crate::{GoalId, MemoryId, Owner, SchemaId, SchemaVersion};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, specta::Type,
)]
pub enum EntityKind {
    Fact,
    Abstraction,
    Perspective,
    Goal,
}

/// Endpoint of an Edge or supersedes target. Sum type matching
/// `change_event` columns: a memory or a goal.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, specta::Type,
)]
pub enum EntityRef {
    Memory(MemoryId),
    Goal(GoalId),
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
pub enum ChangeEventKind {
    EntityAppend {
        entity_kind: EntityKind,
        entity: EntityRef,
        schema_id: SchemaId,
        schema_version: SchemaVersion,
        supersedes: Option<EntityRef>,
    },
    // EdgeAppend reserved for M3+ (the column shape is locked,
    // but no edges are emitted in M2).
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct ChangeEvent {
    pub seq: Uuid,
    pub owner: Owner,
    pub kind: ChangeEventKind,
}

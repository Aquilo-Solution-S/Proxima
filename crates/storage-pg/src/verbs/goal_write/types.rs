use super::{
    EntityKind, GoalId, GoalState, GoalWakeConfigWrite, MemoryId, SchemaId, SchemaVersion,
};

#[derive(Debug)]
pub(super) struct InsertedGoal {
    pub(super) goal_id: GoalId,
    pub(super) change_event_seq: uuid::Uuid,
    pub(super) idempotent_replay: bool,
}

#[derive(Debug, Clone)]
pub(super) struct StoredGoal {
    pub(super) schema_id: SchemaId,
    pub(super) schema_version: SchemaVersion,
    pub(super) title: String,
    pub(super) text: String,
    pub(super) payload: Vec<u8>,
    pub(super) state: GoalState,
    pub(super) assignment: MemoryId,
    pub(super) dependencies: Vec<GoalId>,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct EvidenceTarget {
    pub(super) kind: EntityKind,
    pub(super) memory_id: MemoryId,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum WakeWrite<'a> {
    Explicit(Option<&'a GoalWakeConfigWrite>),
    CarryFrom(GoalId),
}

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

#[derive(Debug, sqlx::FromRow)]
pub(super) struct StoredGoalRow {
    pub(super) schema_id: String,
    pub(super) schema_version: i32,
    pub(super) title: String,
    pub(super) text: String,
    pub(super) payload: Vec<u8>,
    pub(super) state: GoalState,
    pub(super) assignment_perspective_id: Option<uuid::Uuid>,
    pub(super) dependency_goal_ids: Vec<uuid::Uuid>,
}

#[derive(Debug, sqlx::FromRow)]
pub(super) struct ExistingGoalRow {
    pub(super) goal_id: uuid::Uuid,
    pub(super) seq: uuid::Uuid,
}

#[derive(Debug, sqlx::FromRow)]
pub(super) struct GoalBodyRow {
    pub(super) schema_id: String,
    pub(super) schema_version: i32,
    pub(super) title: String,
    pub(super) text: String,
    pub(super) payload: Vec<u8>,
    pub(super) state: GoalState,
    pub(super) supersedes: Option<uuid::Uuid>,
    pub(super) dependency_goal_ids: Vec<uuid::Uuid>,
}

#[derive(Debug, sqlx::FromRow)]
pub(super) struct AuthorshipRow {
    pub(super) authorship_kind: proxima_core::verbs::goal_write::GoalAuthorshipKind,
    pub(super) authorship_origin: Option<proxima_core::verbs::goal_write::GoalAuthorshipOrigin>,
    pub(super) authorship_operator_id: Option<uuid::Uuid>,
    pub(super) authorship_tool_id: Option<String>,
    pub(super) operator_kind: Option<proxima_core::verbs::goal_write::OperatorKind>,
    pub(super) input_contract_id: Option<uuid::Uuid>,
    pub(super) model_id: Option<String>,
    pub(super) prompt_version: Option<String>,
}

#[derive(Debug, sqlx::FromRow)]
pub(super) struct EvidenceRow {
    pub(super) kind: EntityKind,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct EvidenceTarget {
    pub(super) kind: EntityKind,
    pub(super) memory_id: MemoryId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WakeConfigShape {
    pub(super) trigger_kind: String,
    pub(super) trigger_schema_id: Option<String>,
    pub(super) trigger_schema_version: Option<i32>,
    pub(super) trigger_memory_id: Option<uuid::Uuid>,
    pub(super) tool_ids: Vec<String>,
    pub(super) prompt: String,
    pub(super) hard_memory_ids: Vec<uuid::Uuid>,
}

#[derive(Debug, sqlx::FromRow)]
pub(super) struct WakeConfigRow {
    pub(super) trigger_kind: String,
    pub(super) trigger_schema_id: Option<String>,
    pub(super) trigger_schema_version: Option<i32>,
    pub(super) trigger_memory_id: Option<uuid::Uuid>,
    pub(super) tool_ids: Vec<String>,
    pub(super) prompt: String,
    pub(super) hard_memory_ids: Vec<uuid::Uuid>,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum WakeWrite<'a> {
    Explicit(Option<&'a GoalWakeConfigWrite>),
    CarryFrom(GoalId),
}

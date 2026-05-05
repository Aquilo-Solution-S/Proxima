//! GoalWrite verb — typed surface only.
//!
//! See docs/14-protocol-surface.md §"GoalWrite" and
//! docs/06-goals-and-self.md §"Goal entity". The
//! storage-side body lives in proxima-storage-pg.

use crate::{
    GoalId, ModelId, OperatorId, Owner, PersonalityId, PersonalityStateHash, PromptVersion,
    SchemaId, SchemaVersion, ToolId,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
pub enum GoalState {
    Active,
    Paused,
    Achieved,
    Abandoned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
pub enum OperatorKind {
    AtoGoal,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
pub enum SystemOrigin {
    Operator {
        operator_id: OperatorId,
        operator_kind: OperatorKind,
        model_id: ModelId,
        prompt_version: PromptVersion,
        personality_id: PersonalityId,
        personality_state_hash: PersonalityStateHash,
    },
    Tool {
        tool_id: ToolId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
pub enum GoalAuthorship {
    User,
    System(SystemOrigin),
    External,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct GoalDraft {
    pub owner: Owner,
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub text: String,
    pub state: GoalState,
    pub parent_goal_ids: Vec<GoalId>,
    pub authorship: GoalAuthorship,
    pub request_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct GoalWriteOutcome {
    pub goal_id: GoalId,
    pub change_event_seq: uuid::Uuid,
    /// True when the same `(owner, request_id)` existed and the
    /// body matched — see docs/14 §GoalWrite.
    pub idempotent_replay: bool,
}

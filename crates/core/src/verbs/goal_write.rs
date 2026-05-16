//! GoalWrite verb — typed surface only.
//!
//! See docs/14-protocol-surface.md §"GoalWrite" and
//! docs/06-goals-and-self.md §"Goal entity". The
//! storage-side body lives in proxima-storage-pg.

use crate::{
    GoalId, ModelId, OperatorId, Owner, PersonalityInstanceId, PromptVersion, SchemaId,
    SchemaVersion, ToolId,
};

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    specta::Type,
    sqlx::Type,
)]
#[sqlx(type_name = "proxima_core.goal_state")]
pub enum GoalState {
    Proposed,
    Active,
    Paused,
    Achieved,
    Abandoned,
    Rejected,
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    specta::Type,
    sqlx::Type,
)]
#[sqlx(type_name = "proxima_core.goal_operator_kind")]
pub enum OperatorKind {
    AtoGoal,
}

/// Rust mirror of `proxima_core.goal_authorship_kind`.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    specta::Type,
    sqlx::Type,
)]
#[sqlx(type_name = "proxima_core.goal_authorship_kind")]
pub enum GoalAuthorshipKind {
    User,
    System,
    External,
}

/// Rust mirror of `proxima_core.goal_authorship_origin`.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    specta::Type,
    sqlx::Type,
)]
#[sqlx(type_name = "proxima_core.goal_authorship_origin")]
pub enum GoalAuthorshipOrigin {
    Operator,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
pub enum SystemOrigin {
    Operator {
        operator_id: OperatorId,
        operator_kind: OperatorKind,
        model_id: ModelId,
        prompt_version: PromptVersion,
        personality_instance_id: PersonalityInstanceId,
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
    pub title: String,
    pub text: String,
    pub payload: Vec<u8>,
    pub state: GoalState,
    pub parent_goal_ids: Vec<GoalId>,
    pub supersedes_goal_id: Option<GoalId>,
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

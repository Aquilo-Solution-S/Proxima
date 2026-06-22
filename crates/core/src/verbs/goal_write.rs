//! `GoalWrite` verb — typed surface only.
//!
//! See docs/14-protocol-surface.md §"`GoalWrite`" and
//! docs/06-goals-and-self.md §"Goal entity". The
//! storage-side body lives in proxima-storage-pg.

use crate::{
    GoalId, MemoryId, ModelId, OperatorId, Owner, PersonalityInstanceId, Principal, PromptVersion,
    SchemaId, SchemaVersion, SidecarPayload, ToolId,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, sqlx::Type)]
#[sqlx(type_name = "proxima_core.goal_state")]
pub enum GoalState {
    Active,
    Paused,
    Achieved,
    Abandoned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, sqlx::Type)]
#[sqlx(type_name = "proxima_core.goal_operator_kind")]
pub enum OperatorKind {
    AtoGoal,
}

/// Rust mirror of `proxima_core.goal_authorship_kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, sqlx::Type)]
#[sqlx(type_name = "proxima_core.goal_authorship_kind")]
pub enum GoalAuthorshipKind {
    User,
    System,
    External,
}

/// Rust mirror of `proxima_core.goal_authorship_origin`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, sqlx::Type)]
#[sqlx(type_name = "proxima_core.goal_authorship_origin")]
pub enum GoalAuthorshipOrigin {
    Operator,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum GoalAuthorship {
    User,
    System(SystemOrigin),
    External,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GoalDraft {
    pub principal: Principal,
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub title: String,
    pub text: String,
    pub payload: Vec<u8>,
    #[serde(skip)]
    pub sidecar_payload: Option<SidecarPayload>,
    pub state: GoalState,
    pub parent_goal_ids: Vec<GoalId>,
    pub supersedes_goal_id: Option<GoalId>,
    pub authorship: GoalAuthorship,
    pub request_id: String,
}

impl GoalDraft {
    /// The storage `Owner` (= principal) for this draft.
    #[must_use]
    pub fn owner(&self) -> Owner {
        self.principal.clone()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GoalWriteOutcome {
    pub goal_id: GoalId,
    pub change_event_seq: uuid::Uuid,
    pub lifecycle_memory_id: Option<MemoryId>,
    pub edge_ids: Vec<uuid::Uuid>,
    /// True when the same `(owner, request_id)` existed and the
    /// body matched — see docs/14 §`GoalWrite`.
    pub idempotent_replay: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdempotencyKey(String);

impl IdempotencyKey {
    pub const MAX_CHARS: usize = 180;

    /// # Errors
    ///
    /// Returns an error when the trimmed key is empty or longer than
    /// `MAX_CHARS`.
    pub fn new(raw: impl Into<String>) -> Result<Self, String> {
        let trimmed = raw.into().trim().to_string();
        let count = trimmed.chars().count();
        if count == 0 || count > Self::MAX_CHARS {
            return Err(format!(
                "idempotency_key must be 1..={} chars",
                Self::MAX_CHARS
            ));
        }
        Ok(Self(trimmed))
    }

    #[must_use]
    pub fn generated(prefix: &str) -> Self {
        Self(format!("{prefix}:{}", uuid::Uuid::now_v7()))
    }

    /// # Errors
    ///
    /// Returns an error when `raw` is present but fails [`Self::new`].
    pub fn optional_or_generated(prefix: &str, raw: Option<String>) -> Result<Self, String> {
        raw.map_or_else(|| Ok(Self::generated(prefix)), Self::new)
    }

    /// # Errors
    ///
    /// Returns an error when the child request key would exceed the
    /// common idempotency-key bound.
    pub fn child(&self, prefix: &str, index: usize) -> Result<Self, String> {
        Self::new(format!("{prefix}:{}:{index}", self.0))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GoalEvidenceRef {
    pub memory_id: MemoryId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalPayloadWrite {
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub title: String,
    pub text: String,
    pub payload: Vec<u8>,
    pub sidecar_payload: Option<SidecarPayload>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalLifecycleFact {
    Activated,
    Paused,
    Achieved,
    Abandoned,
}

impl GoalLifecycleFact {
    #[must_use]
    pub const fn for_state(state: GoalState) -> Self {
        match state {
            GoalState::Active => Self::Activated,
            GoalState::Paused => Self::Paused,
            GoalState::Achieved => Self::Achieved,
            GoalState::Abandoned => Self::Abandoned,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct GoalAtomicContext<'a> {
    pub registry: &'a crate::FlavorRegistryFrozen,
    pub embedding_model_id: Option<&'a str>,
    pub author_self_perspective_id: Option<MemoryId>,
}

#[derive(Debug)]
pub struct CreateGoalAtomicRequest<'a> {
    pub draft: GoalDraft,
    pub context: GoalAtomicContext<'a>,
    pub target_self_perspective_id: MemoryId,
    pub evidence: Vec<GoalEvidenceRef>,
}

#[derive(Debug)]
pub struct TransitionGoalAtomicRequest<'a> {
    pub owner: Owner,
    pub prior_goal_id: GoalId,
    pub next_state: GoalState,
    pub authorship: GoalAuthorship,
    pub request_id: IdempotencyKey,
    pub context: GoalAtomicContext<'a>,
}

#[derive(Debug)]
pub struct AchieveGoalAtomicRequest<'a> {
    pub owner: Owner,
    pub prior_goal_id: GoalId,
    pub authorship: GoalAuthorship,
    pub request_id: IdempotencyKey,
    pub context: GoalAtomicContext<'a>,
    pub evidence: Vec<GoalEvidenceRef>,
}

#[derive(Debug)]
pub struct ModifyGoalAtomicRequest<'a> {
    pub owner: Owner,
    pub prior_goal_id: GoalId,
    pub replacement: GoalPayloadWrite,
    pub authorship: GoalAuthorship,
    pub request_id: IdempotencyKey,
    pub context: GoalAtomicContext<'a>,
    pub evidence: Option<Vec<GoalEvidenceRef>>,
}

#[derive(Debug)]
pub struct ChildGoalDraft {
    pub payload: GoalPayloadWrite,
    pub evidence: Vec<GoalEvidenceRef>,
    pub request_id: IdempotencyKey,
}

#[derive(Debug)]
pub struct DecomposeGoalAtomicRequest<'a> {
    pub owner: Owner,
    pub parent_goal_id: GoalId,
    pub authorship: GoalAuthorship,
    pub context: GoalAtomicContext<'a>,
    pub target_self_perspective_id: MemoryId,
    pub children: Vec<ChildGoalDraft>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecomposedGoalOutcome {
    pub outcome: GoalWriteOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecomposeGoalOutcome {
    pub children: Vec<DecomposedGoalOutcome>,
    pub idempotent_replay: bool,
}

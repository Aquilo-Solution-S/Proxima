//! `GoalWrite` verb — typed surface only.
//!
//! See docs/14-protocol-surface.md §"`GoalWrite`" and
//! docs/06-goals-and-self.md §"Goal entity". The
//! storage-side body lives in proxima-storage-pg.

use crate::{
    GoalId, GoalPayload, MemoryId, ModelId, OperatorId, Owner, PersonalityInstanceId, Principal,
    PromptVersion, SchemaId, SchemaVersion, SidecarPayload, ToolId,
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

pub const MAX_GOAL_TITLE_CHARS: usize = 240;
pub const MAX_GOAL_TEXT_CHARS: usize = 20_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GoalWriteBuildError {
    #[error("goal title must be 1..={MAX_GOAL_TITLE_CHARS} chars")]
    InvalidTitle,
    #[error("goal text must be 1..={MAX_GOAL_TEXT_CHARS} chars")]
    InvalidText,
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
    /// Build an Active Goal draft from an already-encoded typed payload.
    #[must_use]
    pub fn active_from_payload_write(
        principal: Principal,
        payload: GoalPayloadWrite,
        parent_goal_ids: Vec<GoalId>,
        authorship: GoalAuthorship,
        request_id: IdempotencyKey,
    ) -> Self {
        Self {
            principal,
            schema_id: payload.schema_id,
            schema_version: payload.schema_version,
            title: payload.title,
            text: payload.text,
            payload: payload.payload,
            sidecar_payload: payload.sidecar_payload,
            state: GoalState::Active,
            parent_goal_ids,
            supersedes_goal_id: None,
            authorship,
            request_id: request_id.into_string(),
        }
    }

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

impl GoalPayloadWrite {
    /// Encode a typed [`GoalPayload`] for `GoalWrite` without exposing
    /// storage table shape to host applications.
    ///
    /// # Errors
    ///
    /// Returns [`GoalWriteBuildError::InvalidTitle`] when `title` is
    /// blank after trimming or exceeds [`MAX_GOAL_TITLE_CHARS`]. Returns
    /// [`GoalWriteBuildError::InvalidText`] when `text` is blank after
    /// trimming or exceeds [`MAX_GOAL_TEXT_CHARS`].
    pub fn from_payload<P>(
        title: impl AsRef<str>,
        text: impl AsRef<str>,
        payload: P,
    ) -> Result<Self, GoalWriteBuildError>
    where
        P: GoalPayload,
    {
        let title = normalize_goal_display_field(
            title.as_ref(),
            MAX_GOAL_TITLE_CHARS,
            GoalWriteBuildError::InvalidTitle,
        )?;
        let text = normalize_goal_display_field(
            text.as_ref(),
            MAX_GOAL_TEXT_CHARS,
            GoalWriteBuildError::InvalidText,
        )?;
        let key = payload.goal_key();
        Ok(Self {
            schema_id: P::schema_id(),
            schema_version: SchemaVersion::new(P::SCHEMA_VERSION),
            title,
            text,
            payload: key,
            sidecar_payload: Some(SidecarPayload::goal(payload)),
        })
    }
}

fn normalize_goal_display_field(
    value: &str,
    max_chars: usize,
    err: GoalWriteBuildError,
) -> Result<String, GoalWriteBuildError> {
    let trimmed = value.trim();
    let count = trimmed.chars().count();
    if count == 0 || count > max_chars {
        return Err(err);
    }
    Ok(trimmed.to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalCreateRequest<P> {
    pub principal: Principal,
    pub target_self_perspective_id: MemoryId,
    pub title: String,
    pub text: String,
    pub payload: P,
    pub request_id: IdempotencyKey,
    pub evidence: Vec<GoalEvidenceRef>,
    pub parent_goal_ids: Vec<GoalId>,
    pub authorship: GoalAuthorship,
    pub author_self_perspective_id: Option<MemoryId>,
}

impl<P> GoalCreateRequest<P> {
    /// Product/app Active Goal create request for an authenticated
    /// user flow.
    ///
    /// Defaults to [`GoalAuthorship::User`]: the product UX is acting
    /// for the signed-in owner. Use [`Self::with_authorship`] for
    /// system-originated goals.
    ///
    /// `target_self_perspective_id` is explicit by design: current
    /// Proxima Goal assignment is a `Goal --core/inspires--> Self`
    /// edge, not an unassigned owner-scoped row.
    #[must_use]
    pub fn product(
        principal: Principal,
        target_self_perspective_id: MemoryId,
        request_id: IdempotencyKey,
        title: impl Into<String>,
        text: impl Into<String>,
        payload: P,
    ) -> Self {
        Self {
            principal,
            target_self_perspective_id,
            title: title.into(),
            text: text.into(),
            payload,
            request_id,
            evidence: Vec::new(),
            parent_goal_ids: Vec::new(),
            authorship: GoalAuthorship::User,
            author_self_perspective_id: None,
        }
    }

    #[must_use]
    pub fn with_evidence(mut self, evidence: Vec<GoalEvidenceRef>) -> Self {
        self.evidence = evidence;
        self
    }

    #[must_use]
    pub fn with_parent_goal(mut self, parent_goal_id: GoalId) -> Self {
        self.parent_goal_ids.push(parent_goal_id);
        self
    }

    #[must_use]
    pub fn with_authorship(mut self, authorship: GoalAuthorship) -> Self {
        self.authorship = authorship;
        self
    }

    #[must_use]
    pub fn with_author_self_perspective_id(
        mut self,
        author_self_perspective_id: Option<MemoryId>,
    ) -> Self {
        self.author_self_perspective_id = author_self_perspective_id;
        self
    }
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

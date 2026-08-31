//! `GoalWrite` verb — typed surface only.
//!
//! See docs/14-protocol-surface.md §"`GoalWrite`" and
//! docs/06-goals-and-self.md §"Goal entity". The
//! storage-side body lives in proxima-storage-pg.

use crate::text_bounds::{TrimmedLenViolation, check_trimmed_len};
use crate::{FlavorRegistryFrozen, ProtocolError};
use crate::{
    GoalId, GoalPayload, InputContractId, MemoryId, ModelId, OperatorId, Owner, OwnerRef,
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

impl GoalState {
    /// Legality of a plain lifecycle transition (the `GoalWrite` transition
    /// verb) from `self` to `next`.
    ///
    /// Matrix: `Active → {Active, Paused, Abandoned}`, `Paused → {Active}`.
    /// `Achieved` is **never** a legal target here — achievement carries
    /// mandatory evidence and flows through the dedicated achieve path, not a
    /// plain transition. `Achieved`/`Abandoned` are terminal (no outgoing
    /// transition).
    ///
    /// This predicate and [`Self::may_achieve`] are the authoritative matrix
    /// consulted by the engine routing gate and the storage transition guards.
    #[must_use]
    pub const fn may_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Active, Self::Active | Self::Paused | Self::Abandoned)
                | (Self::Paused, Self::Active)
        )
    }

    /// Whether `self` may enter `Achieved` through the dedicated achieve verb.
    ///
    /// See [`Self::may_transition_to`] for the authoritative plain-transition
    /// matrix and its engine/storage callers.
    #[must_use]
    pub const fn may_achieve(self) -> bool {
        matches!(self, Self::Active)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum OperatorKind {
    AtoGoal,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SystemOrigin {
    Operator {
        operator_id: OperatorId,
        operator_kind: OperatorKind,
        input_contract_id: InputContractId,
        model_id: ModelId,
        prompt_version: PromptVersion,
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

/// Why a typed goal payload could not be built.
///
/// Each variant carries the [`TrimmedLenViolation`] that produced it, so
/// blank and over-long get different `#[error]` strings. Still [`Copy`]
/// and still matchable by variant: hosts that match `InvalidTitle` need
/// only add `(_)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GoalWriteBuildError {
    #[error("{}", .0.reason("goal title"))]
    InvalidTitle(TrimmedLenViolation),
    #[error("{}", .0.reason("goal text"))]
    InvalidText(TrimmedLenViolation),
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GoalDraft {
    pub owner: OwnerRef,
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub title: String,
    pub text: String,
    pub payload: Vec<u8>,
    #[serde(skip)]
    pub sidecar_payload: Option<SidecarPayload>,
    pub state: GoalState,
    pub topology: GoalTopologyWrite,
    pub wake: Option<GoalWakeConfigWrite>,
    pub supersedes_goal_id: Option<GoalId>,
    pub authorship: GoalAuthorship,
    pub request_id: String,
}

impl GoalDraft {
    /// Build an Active Goal draft from an already-encoded typed payload.
    #[must_use]
    pub fn active_from_payload_write(
        owner: OwnerRef,
        payload: GoalPayloadWrite,
        topology: GoalTopologyWrite,
        wake: Option<GoalWakeConfigWrite>,
        authorship: GoalAuthorship,
        request_id: IdempotencyKey,
    ) -> Self {
        Self {
            owner,
            schema_id: payload.schema_id,
            schema_version: payload.schema_version,
            title: payload.title,
            text: payload.text,
            payload: payload.payload,
            sidecar_payload: payload.sidecar_payload,
            state: GoalState::Active,
            topology,
            wake,
            supersedes_goal_id: None,
            authorship,
            request_id: request_id.into_string(),
        }
    }

    /// The storage `Owner` for this draft.
    #[must_use]
    pub fn owner(&self) -> Owner {
        self.owner
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GoalWriteOutcome {
    pub goal_id: GoalId,
    pub change_event_seq: uuid::Uuid,
    pub lifecycle_memory_id: Option<MemoryId>,
    /// Index rows the goal write asserted — one `reference` per
    /// declared topology entry (assignment Perspective, dependency
    /// Goals, evidence memories). A count, not ids: an edge has no
    /// identity beyond its content.
    pub edge_count: usize,
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
    /// Returns the rejection reason when the trimmed key is empty or longer
    /// than `MAX_CHARS`.
    pub fn new(raw: impl Into<String>) -> Result<Self, String> {
        Self::new_named("idempotency_key", raw)
    }

    /// Same contract as [`IdempotencyKey::new`], but the rejection names
    /// `field` rather than `idempotency_key`.
    ///
    /// `core_remember`'s `source_batch_key` is the same 1..=180 trimmed key
    /// under another name; passing the field name keeps one sentence.
    ///
    /// # Errors
    ///
    /// Returns the rejection reason when the trimmed key is empty or longer
    /// than `MAX_CHARS`.
    pub fn new_named(field: &str, raw: impl Into<String>) -> Result<Self, String> {
        let raw = raw.into();
        match check_trimmed_len(&raw, Self::MAX_CHARS) {
            Ok(trimmed) => Ok(Self(trimmed.to_string())),
            Err(violation) => Err(violation.reason(field)),
        }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GoalEvidenceRef {
    memory_id: MemoryId,
}

impl GoalEvidenceRef {
    #[must_use]
    pub const fn new(memory_id: MemoryId) -> Self {
        Self { memory_id }
    }

    #[must_use]
    pub const fn memory_id(self) -> MemoryId {
        self.memory_id
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GoalAssignmentTarget {
    perspective_id: MemoryId,
}

impl GoalAssignmentTarget {
    #[must_use]
    pub const fn perspective(perspective_id: MemoryId) -> Self {
        Self { perspective_id }
    }

    #[must_use]
    pub const fn perspective_id(self) -> MemoryId {
        self.perspective_id
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct GoalDependencyRef {
    goal_id: GoalId,
}

impl GoalDependencyRef {
    #[must_use]
    pub const fn new(goal_id: GoalId) -> Self {
        Self { goal_id }
    }

    #[must_use]
    pub const fn goal_id(self) -> GoalId {
        self.goal_id
    }
}

/// What a Goal row points at, declared by its creating write.
///
/// Every entry here is a reference the Goal itself owns the statement
/// for — the Goal knows the Perspective it inspires, the Goals it waits
/// on, and the evidence it rests on — so storage derives one
/// [`crate::EdgeKind::Reference`] index row per entry inside the goal
/// write's own transaction (docs/16-edges.md). No relation is
/// named and no kind is chosen: the declaration is the whole statement.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GoalTopologyWrite {
    assignment: GoalAssignmentTarget,
    dependencies: Vec<GoalDependencyRef>,
    evidence: Vec<GoalEvidenceRef>,
}

impl GoalTopologyWrite {
    /// # Errors
    ///
    /// Returns `InvalidArgument` when dependency or evidence refs contain duplicates.
    pub fn new(
        assignment: GoalAssignmentTarget,
        dependencies: Vec<GoalDependencyRef>,
        evidence: Vec<GoalEvidenceRef>,
    ) -> Result<Self, ProtocolError> {
        let mut dependency_set = std::collections::BTreeSet::new();
        for dependency in &dependencies {
            if !dependency_set.insert(dependency.goal_id()) {
                return Err(ProtocolError::invalid_argument(
                    "dependencies",
                    "duplicate goal dependency",
                ));
            }
        }
        let mut evidence_set = std::collections::BTreeSet::new();
        for item in &evidence {
            if !evidence_set.insert(item.memory_id().into_inner()) {
                return Err(ProtocolError::invalid_argument(
                    "evidence",
                    "duplicate goal evidence",
                ));
            }
        }
        Ok(Self {
            assignment,
            dependencies,
            evidence,
        })
    }

    #[must_use]
    pub const fn assignment(&self) -> GoalAssignmentTarget {
        self.assignment
    }

    #[must_use]
    pub fn dependencies(&self) -> &[GoalDependencyRef] {
        &self.dependencies
    }

    #[must_use]
    pub fn evidence(&self) -> &[GoalEvidenceRef] {
        &self.evidence
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum GoalWakeTrigger {
    FactSchema {
        schema_id: SchemaId,
        schema_version: SchemaVersion,
    },
    FactMemory {
        memory_id: MemoryId,
    },
}

/// Longest wake tool id, in characters. Generous: the point of the bound is
/// to refuse something that is clearly not a tool id, and the registry
/// lookup below is what actually decides.
pub const MAX_WAKE_TOOL_ID_CHARS: usize = 200;

#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct GoalWakeToolId(String);

impl GoalWakeToolId {
    /// # Errors
    ///
    /// Returns `InvalidArgument` when the id is not provider-safe or does not
    /// resolve to a registered tool/action leaf in the frozen registry.
    pub fn parse(
        raw: impl Into<String>,
        registry: &FlavorRegistryFrozen,
    ) -> Result<Self, ProtocolError> {
        let raw = raw.into();
        // The cap counts characters, matching the message the caller sees.
        // A byte count would refuse a short non-ASCII id for exceeding
        // something the caller cannot measure.
        let value = check_trimmed_len(&raw, MAX_WAKE_TOOL_ID_CHARS)
            .map_err(|violation| {
                ProtocolError::invalid_argument("tool_id", violation.reason("tool id"))
            })?
            .to_string();
        if value.contains('/') {
            return Err(ProtocolError::invalid_argument(
                "tool_id",
                "tool id must be provider-safe canonical id",
            ));
        }
        // Both halves resolve through the descriptor's own `action_arg_specs`
        // rather than the substrate `CoreActionMeta` tables, so a flavor
        // dispatcher's leaf is nameable in a wake config.
        if let Some((tool, action)) = value.split_once(':') {
            if value.matches(':').count() == 1
                && crate::provider_safe_tool_name(tool) == tool
                && crate::provider_safe_tool_name(action) == action
                && registry.mcp_tool(tool).is_some_and(|descriptor| {
                    descriptor
                        .action_arg_specs
                        .iter()
                        .any(|spec| spec.action == action)
                })
            {
                return Ok(Self(value));
            }
            return Err(ProtocolError::invalid_argument(
                "tool_id",
                "leaf action scope required and must be registered",
            ));
        }
        if crate::provider_safe_tool_name(&value) != value {
            return Err(ProtocolError::invalid_argument(
                "tool_id",
                "tool id must be provider-safe canonical id",
            ));
        }
        if let Some(descriptor) = registry.mcp_tool(&value) {
            if !descriptor.action_arg_specs.is_empty() {
                return Err(ProtocolError::invalid_argument(
                    "tool_id",
                    "leaf action scope required for grouped tools",
                ));
            }
            return Ok(Self(value));
        }
        Err(ProtocolError::invalid_argument(
            "tool_id",
            "tool id is not registered",
        ))
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

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GoalWakeConfigWrite {
    trigger: GoalWakeTrigger,
    tool_ids: Vec<GoalWakeToolId>,
    prompt: String,
    hard_memory_ids: Vec<MemoryId>,
}

impl GoalWakeConfigWrite {
    pub const MAX_PROMPT_CHARS: usize = 20_000;

    /// # Errors
    ///
    /// Returns `InvalidArgument` when prompt/tool/memory shape is invalid.
    pub fn new(
        trigger: GoalWakeTrigger,
        tool_ids: Vec<GoalWakeToolId>,
        prompt: impl Into<String>,
        hard_memory_ids: &[MemoryId],
    ) -> Result<Self, ProtocolError> {
        let raw = prompt.into();
        let prompt = check_trimmed_len(&raw, Self::MAX_PROMPT_CHARS)
            .map_err(|violation| {
                ProtocolError::invalid_argument("prompt", violation.reason("wake prompt"))
            })?
            .to_string();
        if tool_ids.is_empty() {
            return Err(ProtocolError::invalid_argument(
                "tool_ids",
                "wake toolset must be nonempty",
            ));
        }
        let mut tools = std::collections::BTreeSet::new();
        for tool in tool_ids {
            tools.insert(tool);
        }
        let mut memory_ids = std::collections::BTreeSet::new();
        for memory_id in hard_memory_ids {
            if !memory_ids.insert(*memory_id) {
                return Err(ProtocolError::invalid_argument(
                    "hard_memory_ids",
                    "duplicate hard memory id",
                ));
            }
        }
        Ok(Self {
            trigger,
            tool_ids: tools.into_iter().collect(),
            prompt,
            hard_memory_ids: memory_ids.into_iter().collect(),
        })
    }

    #[must_use]
    pub const fn trigger(&self) -> &GoalWakeTrigger {
        &self.trigger
    }

    #[must_use]
    pub fn tool_ids(&self) -> &[GoalWakeToolId] {
        &self.tool_ids
    }

    #[must_use]
    pub fn prompt(&self) -> &str {
        &self.prompt
    }

    #[must_use]
    pub fn hard_memory_ids(&self) -> &[MemoryId] {
        &self.hard_memory_ids
    }
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

/// Trim and bound one goal display field, tagging the violation with the
/// field it came from.
///
/// `wrap` is the tuple variant itself — `GoalWriteBuildError::InvalidTitle`
/// used as a function — so the caller names the field once and the rule
/// stays in [`check_trimmed_len`].
fn normalize_goal_display_field(
    value: &str,
    max_chars: usize,
    wrap: fn(TrimmedLenViolation) -> GoalWriteBuildError,
) -> Result<String, GoalWriteBuildError> {
    check_trimmed_len(value, max_chars)
        .map(str::to_string)
        .map_err(wrap)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalCreateRequest<P> {
    pub owner: OwnerRef,
    pub topology: GoalTopologyWrite,
    pub wake: Option<GoalWakeConfigWrite>,
    pub title: String,
    pub text: String,
    pub payload: P,
    pub request_id: IdempotencyKey,
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
    /// The assignment Perspective is explicit by design: current
    /// Proxima Goal assignment is a reference the Goal row declares at
    /// creation — the Goal knows the Perspective it inspires — not an
    /// unassigned owner-scoped row.
    ///
    /// # Panics
    ///
    /// Panics only if an empty dependency/evidence topology is rejected,
    /// which would be a programming error in the constructor invariant.
    #[must_use]
    pub fn product(
        owner: OwnerRef,
        assignment: GoalAssignmentTarget,
        request_id: IdempotencyKey,
        title: impl Into<String>,
        text: impl Into<String>,
        payload: P,
    ) -> Self {
        Self {
            owner,
            topology: GoalTopologyWrite::new(assignment, Vec::new(), Vec::new())
                .expect("empty topology is valid"),
            wake: None,
            title: title.into(),
            text: text.into(),
            payload,
            request_id,
            authorship: GoalAuthorship::User,
            author_self_perspective_id: None,
        }
    }

    #[must_use]
    pub fn with_evidence(mut self, evidence: Vec<GoalEvidenceRef>) -> Self {
        self.topology.evidence = evidence;
        self
    }

    #[must_use]
    pub fn with_dependency(mut self, dependency: GoalDependencyRef) -> Self {
        self.topology.dependencies.push(dependency);
        self
    }

    #[must_use]
    pub fn with_topology(mut self, topology: GoalTopologyWrite) -> Self {
        self.topology = topology;
        self
    }

    #[must_use]
    pub fn with_wake(mut self, wake: Option<GoalWakeConfigWrite>) -> Self {
        self.wake = wake;
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
    /// When set, the new Goal row pins this write-act Fact (`Goal.write_act_t`).
    /// Replay does not rewrite the column; a bound episode must fail instead.
    pub write_act_t: Option<MemoryId>,
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
    pub wake: Option<Option<GoalWakeConfigWrite>>,
    pub authorship: GoalAuthorship,
    pub request_id: IdempotencyKey,
    pub context: GoalAtomicContext<'a>,
    pub evidence: Option<Vec<GoalEvidenceRef>>,
}

#[derive(Debug)]
pub struct ChildGoalDraft {
    pub payload: GoalPayloadWrite,
    pub evidence: Vec<GoalEvidenceRef>,
    pub wake: Option<GoalWakeConfigWrite>,
    pub request_id: IdempotencyKey,
}

#[derive(Debug)]
pub struct DecomposeGoalAtomicRequest<'a> {
    pub owner: Owner,
    pub parent_goal_id: GoalId,
    pub authorship: GoalAuthorship,
    pub context: GoalAtomicContext<'a>,
    pub topology: GoalTopologyWrite,
    pub children: Vec<ChildGoalDraft>,
}

/// One already-normalized Goal command whose request-id replay may be
/// resolved without renewing admission of the mutable rows it originally
/// referenced.
#[derive(Debug, Clone, Copy)]
pub enum GoalReplayRequest<'request, 'context> {
    Create(&'request CreateGoalAtomicRequest<'context>),
    Transition(&'request TransitionGoalAtomicRequest<'context>),
    Achieve(&'request AchieveGoalAtomicRequest<'context>),
    Modify(&'request ModifyGoalAtomicRequest<'context>),
    Decompose(&'request DecomposeGoalAtomicRequest<'context>),
}

impl GoalReplayRequest<'_, '_> {
    /// The Owner namespace in which the request id is interpreted.
    #[must_use]
    pub fn owner(self) -> Owner {
        match self {
            Self::Create(req) => req.draft.owner(),
            Self::Transition(req) => req.owner,
            Self::Achieve(req) => req.owner,
            Self::Modify(req) => req.owner,
            Self::Decompose(req) => req.owner,
        }
    }
}

/// Stored response recovered by an exact Goal-command replay probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoalReplayOutcome {
    Goal(GoalWriteOutcome),
    Decompose(DecomposeGoalOutcome),
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

#[cfg(test)]
mod goal_state_matrix_tests {
    use super::GoalState;

    const ALL: [GoalState; 4] = [
        GoalState::Active,
        GoalState::Paused,
        GoalState::Achieved,
        GoalState::Abandoned,
    ];

    #[test]
    fn active_may_move_to_active_paused_abandoned_only() {
        assert!(GoalState::Active.may_transition_to(GoalState::Active));
        assert!(GoalState::Active.may_transition_to(GoalState::Paused));
        assert!(GoalState::Active.may_transition_to(GoalState::Abandoned));
        assert!(!GoalState::Active.may_transition_to(GoalState::Achieved));
    }

    #[test]
    fn paused_may_only_resume_to_active() {
        assert!(GoalState::Paused.may_transition_to(GoalState::Active));
        assert!(!GoalState::Paused.may_transition_to(GoalState::Paused));
        assert!(!GoalState::Paused.may_transition_to(GoalState::Abandoned));
        assert!(!GoalState::Paused.may_transition_to(GoalState::Achieved));
    }

    #[test]
    fn achieved_is_never_a_legal_transition_target() {
        for prior in ALL {
            assert!(
                !prior.may_transition_to(GoalState::Achieved),
                "{prior:?} -> Achieved must be rejected as a plain transition"
            );
        }
    }

    #[test]
    fn only_active_may_achieve() {
        assert!(GoalState::Active.may_achieve());
        assert!(!GoalState::Paused.may_achieve());
        assert!(!GoalState::Achieved.may_achieve());
        assert!(!GoalState::Abandoned.may_achieve());
    }

    #[test]
    fn terminal_states_have_no_outgoing_transition() {
        for next in ALL {
            assert!(!GoalState::Achieved.may_transition_to(next));
            assert!(!GoalState::Abandoned.may_transition_to(next));
        }
    }
}

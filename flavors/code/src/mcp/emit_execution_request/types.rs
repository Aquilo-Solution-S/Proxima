use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::payloads::AcceptanceCriterionV1;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CodeEmitExecutionRequestArgs {
    #[schemars(
        description = "Repository the work is for: `R:<uuid>` from proxima-code_list_repos or search output."
    )]
    pub repo_handle: String,
    #[schemars(
        length(max = 240),
        description = "Short human-readable execution-request title, 1 to 240 chars."
    )]
    pub title: String,
    #[schemars(
        length(max = 20_000),
        description = "Concrete implementation instructions for the worker wake, 1 to 20000 chars."
    )]
    pub instructions: String,
    #[schemars(
        length(max = 240),
        description = "Stable key for this request; reuse only for an exact replay."
    )]
    pub idempotency_key: String,
    #[schemars(
        description = "`F:<uuid>` goal-activated Fact for the Active Goal that caused this planner wake (not a `G:<uuid>` Goal handle). Recorded as the request's origin."
    )]
    pub goal_activated_memory: String,
    #[serde(default)]
    #[schemars(description = "Extra evidence Facts (`F:<uuid>` only).")]
    pub evidence: Vec<String>,
    #[serde(default)]
    #[schemars(
        description = "Optional acceptance criteria for worker/verifier evaluation. Use `[]` when no criteria are needed."
    )]
    pub acceptance_criteria: Vec<AcceptanceCriterionV1>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct CodeEmitExecutionRequestOutput {
    pub handle: String,
    /// How many `origin` index rows the write asserted — the activation
    /// Fact plus each evidence Fact. A count, not handles: an edge has no
    /// id, and replaying the emit re-asserts the same rows.
    pub origin_count: usize,
    pub acceptance_criteria_handle: Option<String>,
    pub idempotent_replay: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[schemars(description = "Execution plan item category.")]
#[serde(rename_all = "snake_case")]
pub enum ExecutionPlanItemKind {
    #[default]
    Implementation,
    Test,
}

impl ExecutionPlanItemKind {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Implementation => "implementation",
            Self::Test => "test",
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ExecutionPlanItemArgs {
    #[serde(default)]
    #[schemars(description = "Plan item kind. Defaults to `implementation`.")]
    pub kind: ExecutionPlanItemKind,
    #[schemars(description = "Unique within the plan: 1 to 80 ASCII letters, digits, `-` or `_`.")]
    pub key: String,
    #[schemars(description = "Short human-readable execution-request title, 1 to 240 chars.")]
    pub title: String,
    #[schemars(description = "Concrete implementation instructions for this work slice.")]
    pub instructions: String,
    #[schemars(description = "Stable idempotency key for this work slice.")]
    pub idempotency_key: String,
    #[serde(default)]
    #[schemars(description = "Item keys that must complete before this item can dispatch.")]
    pub depends_on: Vec<String>,
    #[serde(default)]
    #[schemars(description = "kind=implementation only.")]
    pub acceptance_criteria: Vec<AcceptanceCriterionV1>,
    #[serde(default)]
    #[schemars(
        description = "kind=test only, and then required: at least one with required=true."
    )]
    pub test_criteria: Vec<AcceptanceCriterionV1>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CodeEmitExecutionPlanArgs {
    #[schemars(description = "`R:<uuid>` repo handle from code search or list output.")]
    pub repo_handle: String,
    #[schemars(description = "`F:<uuid>` goal-activated Fact for the Active Goal.")]
    pub goal_activated_memory: String,
    #[schemars(description = "`A:<uuid>` of the planning Abstraction this plan is derived from.")]
    pub plan_source_memory: String,
    #[serde(default)]
    #[schemars(
        description = "Optional stable idempotency key for the plan Abstraction. Defaults to a deterministic key from goal + item keys."
    )]
    pub plan_key: Option<String>,
    #[serde(default)]
    #[schemars(description = "Optional concise summary of the plan synthesis.")]
    pub plan_summary: Option<String>,
    #[serde(default)]
    #[schemars(description = "Extra evidence Facts (`F:<uuid>` only) for every item.")]
    pub evidence: Vec<String>,
    #[schemars(
        description = "1 to 20 items, in order; depends_on may name only earlier item keys."
    )]
    pub items: Vec<ExecutionPlanItemArgs>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ExecutionPlanItemOutput {
    pub key: String,
    pub kind: ExecutionPlanItemKind,
    pub handle: String,
    pub idempotent_replay: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct CodeEmitExecutionPlanOutput {
    pub plan_handle: String,
    /// Index rows the plan write asserted: one `origin` to its Abstraction
    /// input plus one `reference` per target its payload names — the
    /// activation Fact, the evidence Facts, and each item's request Fact.
    pub plan_edge_count: usize,
    pub plan_idempotent_replay: bool,
    pub items: Vec<ExecutionPlanItemOutput>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CodeRetryExecutionRequestArgs {
    #[schemars(description = "`F:<uuid>` of the prior proxima-code/work-requested-v1 Fact.")]
    pub prior_execution_request: String,
    #[schemars(description = "`P:<uuid>` of the worker Perspective receiving the retry.")]
    pub target_perspective: String,
    #[schemars(
        description = "Stable idempotency key for this retry request. Reuse only for exact replay."
    )]
    pub idempotency_key: String,
    #[serde(default)]
    #[schemars(
        description = "Optional replacement title for the retry request. Omit or null to derive from the prior request."
    )]
    pub title: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Text appended to the prior instructions; the combined instructions must stay within 20000 chars."
    )]
    pub instructions_append: Option<String>,
    #[serde(default)]
    #[schemars(description = "Extra evidence Facts (`F:<uuid>` only).")]
    pub evidence: Vec<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct CodeRetryExecutionRequestOutput {
    pub handle: String,
    /// `P:` handle of the assignment Perspective that names the target
    /// worker and this request. A memory handle, because the claim is a node
    /// rather than an edge.
    pub assignment_handle: Option<String>,
    /// `origin` rows asserted by the retry: the prior request, everything
    /// it was made from, and any extra evidence.
    pub origin_count: usize,
    pub idempotent_replay: bool,
}

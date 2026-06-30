use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::payloads::AcceptanceCriterionV1;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CodeEmitExecutionRequestArgs {
    #[schemars(
        description = "Repo handle from code search/list context, typically `R...` in wake output. This selects the repo for the execution request."
    )]
    pub repo_handle: String,
    #[schemars(description = "Short human-readable execution-request title, 1 to 240 chars.")]
    pub title: String,
    #[schemars(
        description = "Concrete implementation instructions for the worker wake, 1 to 20000 chars."
    )]
    pub instructions: String,
    #[schemars(
        description = "Stable idempotency key for this requested work slice, 1 to 240 chars. Reuse only for exact replay."
    )]
    pub idempotency_key: String,
    #[schemars(
        description = "`F...` goal-activated Fact memory handle for the Active Goal that caused this planner wake. This is not a `G...` Goal handle."
    )]
    pub goal_activated_memory: String,
    #[serde(default)]
    #[schemars(
        description = "Optional additional Fact memory handles (`F...`) used as evidence for the execution request. Use `[]` when no separate Fact evidence is needed; never Goal, Abstraction, or Perspective handles."
    )]
    pub evidence: Vec<String>,
    #[serde(default)]
    #[schemars(
        description = "Optional acceptance criteria for worker/verifier evaluation. Use `[]` when no criteria are needed."
    )]
    pub acceptance_criteria: Vec<AcceptanceCriterionV1>,
}

#[derive(Debug, Serialize)]
pub struct CodeEmitExecutionRequestOutput {
    pub handle: String,
    pub authored_edge_handle: Option<String>,
    pub derived_edge_handles: Vec<String>,
    pub acceptance_criteria_handle: Option<String>,
    pub acceptance_criteria_edge_handle: Option<String>,
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
    #[schemars(description = "Plan item kind. Defaults to `implementation` for compatibility.")]
    pub kind: ExecutionPlanItemKind,
    #[schemars(description = "Unique item key inside this plan, 1 to 80 ASCII chars.")]
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
    #[schemars(description = "Optional acceptance criteria for this work slice.")]
    pub acceptance_criteria: Vec<AcceptanceCriterionV1>,
    #[serde(default)]
    #[schemars(description = "Required criteria for a `test` item.")]
    pub test_criteria: Vec<AcceptanceCriterionV1>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CodeEmitExecutionPlanArgs {
    #[schemars(description = "Repo handle from code search/list context.")]
    pub repo_handle: String,
    #[schemars(description = "`F...` goal-activated Fact memory handle for the Active Goal.")]
    pub goal_activated_memory: String,
    #[schemars(
        description = "`A...` Abstraction proof input for the A→A execution-plan derivation. This should be the planning context/synthesis Abstraction grounded in the active Goal."
    )]
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
    #[schemars(
        description = "Optional additional Fact memory handles used as evidence for every item."
    )]
    pub evidence: Vec<String>,
    #[schemars(
        description = "Ordered implementation/test items. Dependencies may reference only earlier item keys."
    )]
    pub items: Vec<ExecutionPlanItemArgs>,
}

#[derive(Debug, Serialize)]
pub struct ExecutionPlanItemOutput {
    pub key: String,
    pub kind: ExecutionPlanItemKind,
    pub handle: String,
    pub dependency_edge_handles: Vec<String>,
    pub idempotent_replay: bool,
}

#[derive(Debug, Serialize)]
pub struct CodeEmitExecutionPlanOutput {
    pub plan_handle: String,
    pub plan_derived_edge_handles: Vec<String>,
    pub plan_idempotent_replay: bool,
    pub items: Vec<ExecutionPlanItemOutput>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CodeRetryExecutionRequestArgs {
    #[schemars(
        description = "`F...` memory handle for the prior proxima-code/work-requested-v1 Fact being retried."
    )]
    pub prior_execution_request: String,
    #[schemars(
        description = "`P...` Perspective memory handle for the worker context that should receive the retry assignment."
    )]
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
        description = "Optional instructions to append to the prior request. Omit or null when the retry needs no extra guidance."
    )]
    pub instructions_append: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Optional additional Fact memory handles (`F...`) for retry evidence. Use `[]` when no extra evidence is needed; never Goal, Abstraction, or Perspective handles."
    )]
    pub evidence: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct CodeRetryExecutionRequestOutput {
    pub handle: String,
    pub authored_edge_handle: Option<String>,
    pub target_edge_handle: Option<String>,
    pub derived_edge_handles: Vec<String>,
    pub idempotent_replay: bool,
}

use std::collections::{HashMap, HashSet};

use proxima_core::mcp::{McpTool, McpToolCtx, McpToolError};
use proxima_core::personality::{PersonalityInstanceId, PersonalityStatus};
use proxima_core::relation::{
    CORE_AUTHORED_RELATION, CORE_DEPENDS_ON_RELATION, CORE_DERIVED_FROM_RELATION,
};
use proxima_core::verbs::event_ingest::{CitationSpec, EventIngestOutcome};
use proxima_core::{
    AbstractionPayload, EdgeAuthorshipKind, EdgeId, EntityKind, FactPayload, MemoryId,
    MemoryOperatorKind, SchemaVersion, SourceBatchId,
};
use proxima_storage_pg::sidecars::PgMemorySidecar;
use proxima_storage_pg::verbs::derive_append::{DerivedDraft, append_derived_in_tx};
use proxima_storage_pg::verbs::edge_append::{Endpoint, append_edge};
use proxima_storage_pg::verbs::event_ingest::{FactIngestContext, ingest_fact_with_sidecar};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::ingest::{
    ACCEPTANCE_CRITERIA_OBJECT_SCHEMA, ACCEPTANCE_CRITERIA_WHOLE_SCHEMA,
    EXECUTION_REQUEST_OBJECT_SCHEMA, EXECUTION_REQUEST_WHOLE_SCHEMA, TEST_REQUEST_OBJECT_SCHEMA,
    TEST_REQUEST_WHOLE_SCHEMA,
};
use crate::payloads::{
    AcceptanceCriteriaV1, AcceptanceCriterionV1, AcceptanceVerifierKind, CodeExecutionPlanItemKind,
    CodeExecutionPlanItemV1, CodeExecutionPlanV1, ExecutionRequestV1, TestRequestV1,
};

use super::pg_pool;
use super::sql::{map_storage, owner_principal, resolve_repo_identifier};

const EXECUTION_REQUEST_SOURCE_ID: &str = "proxima-code/execution-request";
pub const CODE_TARGETS_EXECUTION_REQUEST_RELATION: &str = "proxima-code/targets-execution-request";
pub const CODE_HAS_ACCEPTANCE_CRITERIA_RELATION: &str = "proxima-code/has-acceptance-criteria";
const ACCEPTANCE_CRITERIA_SOURCE_ID: &str = "proxima-code/acceptance-criteria";
const TEST_REQUEST_SOURCE_ID: &str = "proxima-code/test-request";

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
        description = "Optional additional Fact memory handles (`F...`) used as evidence for the execution request. Use `[]` when no separate Fact evidence is needed; never `G...`, `A...`, `P...`, or `I...` handles."
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
    fn as_str(self) -> &'static str {
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
        description = "`I...` Personality handle for the worker that should receive the retry assignment."
    )]
    pub target_personality: String,
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
        description = "Optional additional Fact memory handles (`F...`) for retry evidence. Use `[]` when no extra evidence is needed; never `G...`, `A...`, `P...`, or `I...` handles."
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

#[derive(Debug)]
pub struct CodeEmitExecutionRequestTool;

impl McpTool for CodeEmitExecutionRequestTool {
    const NAME: &'static str = "proxima-code/code_emit_execution_request";
    const DESCRIPTION: &'static str =
        "Emit a repo-scoped proxima-code/work-requested-v1 Fact for an Active Goal.";
    const PRODUCES_SCHEMA_IDS: &'static [&'static str] = &[ExecutionRequestV1::SCHEMA_ID];

    type Args = CodeEmitExecutionRequestArgs;
    type Output = CodeEmitExecutionRequestOutput;

    fn call(
        ctx: McpToolCtx,
        args: CodeEmitExecutionRequestArgs,
    ) -> futures::future::BoxFuture<'static, Result<CodeEmitExecutionRequestOutput, McpToolError>>
    {
        Box::pin(async move {
            let repo_id = resolve_repo_identifier(&ctx, &args.repo_handle).await?;
            validate_repo(&ctx, repo_id).await?;

            let title = normalize_text("title", &args.title, 1, 240)?;
            let instructions = normalize_text("instructions", &args.instructions, 1, 20_000)?;
            let request_key = normalize_text("idempotency_key", &args.idempotency_key, 1, 240)?;
            let acceptance_criteria = validate_acceptance_criteria(args.acceptance_criteria)?;

            let planner_root = ctx.caller_self_perspective.ok_or_else(|| {
                McpToolError::InvalidInput(
                    "caller_self_perspective is required to author an execution request".into(),
                )
            })?;
            let goal_activated_memory_id = ctx.resolve_fact_memory(&args.goal_activated_memory)?;
            let evidence = resolve_evidence(&ctx, &args.evidence)?;

            let pool = pg_pool(&ctx)?;
            let mut tx = pool.begin().await.map_err(map_storage)?;
            let goal_id =
                validate_goal_activated_fact(&mut tx, &ctx, goal_activated_memory_id).await?;
            validate_active_goal_context(&mut tx, &ctx, goal_id, planner_root).await?;
            validate_evidence_in_owner(&mut tx, &ctx, &evidence).await?;

            let payload = ExecutionRequestV1 {
                repo_id,
                title,
                instructions,
                request_key,
            };
            let outcome = ingest_execution_request(&mut tx, &ctx, &payload).await?;
            let (authored_edge_id, derived_edge_ids, acceptance_memory_id, acceptance_edge_id) =
                if outcome.idempotent_replay {
                    (None, Vec::new(), None, None)
                } else {
                    let authored_edge_id =
                        append_authored_edge(&mut tx, &ctx, planner_root, outcome.memory_id)
                            .await?;
                    let mut derived_edge_ids = Vec::with_capacity(1 + evidence.len());
                    derived_edge_ids.push(
                        append_derived_edge(
                            &mut tx,
                            &ctx,
                            outcome.memory_id,
                            goal_activated_memory_id,
                        )
                        .await?,
                    );
                    for memory_id in evidence {
                        derived_edge_ids.push(
                            append_derived_edge(&mut tx, &ctx, outcome.memory_id, memory_id)
                                .await?,
                        );
                    }
                    let (acceptance_memory_id, acceptance_edge_id) = if acceptance_criteria
                        .is_empty()
                    {
                        (None, None)
                    } else {
                        let criteria_payload = AcceptanceCriteriaV1 {
                            work_item_memory_id: outcome.memory_id.into_inner(),
                            criteria: acceptance_criteria,
                        };
                        let criteria_outcome =
                            ingest_acceptance_criteria(&mut tx, &ctx, &criteria_payload).await?;
                        if criteria_outcome.idempotent_replay {
                            (Some(criteria_outcome.memory_id), None)
                        } else {
                            let edge_id = append_acceptance_criteria_edge(
                                &mut tx,
                                &ctx,
                                outcome.memory_id,
                                criteria_outcome.memory_id,
                            )
                            .await?;
                            (Some(criteria_outcome.memory_id), Some(edge_id))
                        }
                    };
                    (
                        Some(authored_edge_id),
                        derived_edge_ids,
                        acceptance_memory_id,
                        acceptance_edge_id,
                    )
                };
            tx.commit().await.map_err(map_storage)?;

            Ok(CodeEmitExecutionRequestOutput {
                handle: ctx.format_fact_memory(outcome.memory_id),
                authored_edge_handle: authored_edge_id
                    .map(|edge_id| ctx.format_edge(EdgeId::new(edge_id))),
                derived_edge_handles: derived_edge_ids
                    .into_iter()
                    .map(|edge_id| ctx.format_edge(EdgeId::new(edge_id)))
                    .collect(),
                acceptance_criteria_handle: acceptance_memory_id
                    .map(|id| ctx.format_fact_memory(id)),
                acceptance_criteria_edge_handle: acceptance_edge_id
                    .map(|edge_id| ctx.format_edge(EdgeId::new(edge_id))),
                idempotent_replay: outcome.idempotent_replay,
            })
        })
    }
}

#[derive(Debug)]
pub struct CodeEmitExecutionPlanTool;

impl McpTool for CodeEmitExecutionPlanTool {
    const NAME: &'static str = "proxima-code/code_emit_execution_plan";
    const DESCRIPTION: &'static str = "Atomically emit an ordered set of repo-scoped implementation/test request Facts plus core/depends-on edges.";
    const PRODUCES_SCHEMA_IDS: &'static [&'static str] = &[
        CodeExecutionPlanV1::SCHEMA_ID,
        ExecutionRequestV1::SCHEMA_ID,
        TestRequestV1::SCHEMA_ID,
    ];

    type Args = CodeEmitExecutionPlanArgs;
    type Output = CodeEmitExecutionPlanOutput;

    #[allow(clippy::too_many_lines)]
    fn call(
        ctx: McpToolCtx,
        args: CodeEmitExecutionPlanArgs,
    ) -> futures::future::BoxFuture<'static, Result<CodeEmitExecutionPlanOutput, McpToolError>>
    {
        Box::pin(async move {
            let repo_id = resolve_repo_identifier(&ctx, &args.repo_handle).await?;
            validate_repo(&ctx, repo_id).await?;
            let plan_items = validate_plan_items(args.items)?;

            let planner_root = ctx.caller_self_perspective.ok_or_else(|| {
                McpToolError::InvalidInput(
                    "caller_self_perspective is required to author an execution plan".into(),
                )
            })?;
            let goal_activated_memory_id = ctx.resolve_fact_memory(&args.goal_activated_memory)?;
            let evidence = resolve_evidence(&ctx, &args.evidence)?;

            let pool = pg_pool(&ctx)?;
            let mut tx = pool.begin().await.map_err(map_storage)?;
            let goal_id =
                validate_goal_activated_fact(&mut tx, &ctx, goal_activated_memory_id).await?;
            validate_active_goal_context(&mut tx, &ctx, goal_id, planner_root).await?;
            validate_evidence_in_owner(&mut tx, &ctx, &evidence).await?;

            let plan_key = match args.plan_key {
                Some(value) => normalize_text("plan_key", &value, 1, 240)?,
                None => default_plan_key(goal_activated_memory_id, &plan_items),
            };
            let plan_summary = match args.plan_summary {
                Some(value) => normalize_text("plan_summary", &value, 1, 4_000)?,
                None => format!("Plan with {} work/test item(s)", plan_items.len()),
            };
            let plan_payload = CodeExecutionPlanV1 {
                repo_id,
                plan_key: plan_key.clone(),
                goal_activated_memory_id: goal_activated_memory_id.into_inner(),
                summary: plan_summary.clone(),
                items: plan_items
                    .iter()
                    .map(|item| CodeExecutionPlanItemV1 {
                        key: item.key.clone(),
                        kind: match item.kind {
                            ExecutionPlanItemKind::Implementation => {
                                CodeExecutionPlanItemKind::Work
                            }
                            ExecutionPlanItemKind::Test => CodeExecutionPlanItemKind::Test,
                        },
                        title: item.title.clone(),
                        depends_on: item.depends_on.clone(),
                        request_key: item.idempotency_key.clone(),
                    })
                    .collect(),
                evidence_memory_ids: evidence.iter().map(|id| id.into_inner()).collect(),
            };
            let plan_outcome = append_execution_plan(
                &mut tx,
                &ctx,
                planner_root,
                goal_activated_memory_id,
                &evidence,
                &plan_key,
                &plan_summary,
                &plan_payload,
            )
            .await?;
            let plan_memory_id = plan_outcome.memory_id;
            let mut plan_edge_ids = plan_outcome.edge_ids;

            let mut emitted: HashMap<String, MemoryId> = HashMap::new();
            let mut outputs = Vec::with_capacity(plan_items.len());
            for item in plan_items {
                let kind = item.kind;
                let outcome = match kind {
                    ExecutionPlanItemKind::Implementation => {
                        let payload = ExecutionRequestV1 {
                            repo_id,
                            title: item.title,
                            instructions: item.instructions,
                            request_key: item.idempotency_key,
                        };
                        let outcome = ingest_execution_request(&mut tx, &ctx, &payload).await?;
                        if !outcome.idempotent_replay {
                            append_authored_edge(&mut tx, &ctx, planner_root, outcome.memory_id)
                                .await?;
                            append_derived_edge(
                                &mut tx,
                                &ctx,
                                outcome.memory_id,
                                goal_activated_memory_id,
                            )
                            .await?;
                            for memory_id in &evidence {
                                append_derived_edge(&mut tx, &ctx, outcome.memory_id, *memory_id)
                                    .await?;
                            }
                            if !item.acceptance_criteria.is_empty() {
                                let criteria_payload = AcceptanceCriteriaV1 {
                                    work_item_memory_id: outcome.memory_id.into_inner(),
                                    criteria: item.acceptance_criteria,
                                };
                                let criteria_outcome =
                                    ingest_acceptance_criteria(&mut tx, &ctx, &criteria_payload)
                                        .await?;
                                if !criteria_outcome.idempotent_replay {
                                    append_acceptance_criteria_edge(
                                        &mut tx,
                                        &ctx,
                                        outcome.memory_id,
                                        criteria_outcome.memory_id,
                                    )
                                    .await?;
                                }
                            }
                        }
                        outcome
                    }
                    ExecutionPlanItemKind::Test => {
                        let payload = TestRequestV1 {
                            repo_id,
                            title: item.title,
                            instructions: item.instructions,
                            test_key: item.idempotency_key,
                            criteria: item.test_criteria,
                        };
                        let outcome = ingest_test_request(&mut tx, &ctx, &payload).await?;
                        if !outcome.idempotent_replay {
                            append_authored_edge(&mut tx, &ctx, planner_root, outcome.memory_id)
                                .await?;
                            append_derived_edge(
                                &mut tx,
                                &ctx,
                                outcome.memory_id,
                                goal_activated_memory_id,
                            )
                            .await?;
                            for memory_id in &evidence {
                                append_derived_edge(&mut tx, &ctx, outcome.memory_id, *memory_id)
                                    .await?;
                            }
                        }
                        outcome
                    }
                };
                plan_edge_ids.push(
                    append_plan_derived_edge(&mut tx, &ctx, plan_memory_id, outcome.memory_id)
                        .await?,
                );
                let mut dependency_edges = Vec::new();
                for dependency_key in &item.depends_on {
                    let dependency_memory_id =
                        emitted.get(dependency_key).copied().ok_or_else(|| {
                            McpToolError::InvalidInput(format!(
                                "depends_on references unavailable item key: {dependency_key}"
                            ))
                        })?;
                    let edge_id = append_dependency_edge(
                        &mut tx,
                        &ctx,
                        outcome.memory_id,
                        dependency_memory_id,
                    )
                    .await?;
                    dependency_edges.push(ctx.format_edge(EdgeId::new(edge_id)));
                }
                emitted.insert(item.key.clone(), outcome.memory_id);
                outputs.push(ExecutionPlanItemOutput {
                    key: item.key,
                    kind,
                    handle: ctx.format_fact_memory(outcome.memory_id),
                    dependency_edge_handles: dependency_edges,
                    idempotent_replay: outcome.idempotent_replay,
                });
            }
            tx.commit().await.map_err(map_storage)?;
            Ok(CodeEmitExecutionPlanOutput {
                plan_handle: ctx.format_abstraction_memory(plan_memory_id),
                plan_derived_edge_handles: plan_edge_ids
                    .into_iter()
                    .map(|edge_id| ctx.format_edge(EdgeId::new(edge_id)))
                    .collect(),
                plan_idempotent_replay: plan_outcome.idempotent_replay,
                items: outputs,
            })
        })
    }
}

#[derive(Debug)]
struct PlanAppendOutcome {
    memory_id: MemoryId,
    edge_ids: Vec<Uuid>,
    idempotent_replay: bool,
}

fn default_plan_key(goal_activated_memory_id: MemoryId, items: &[ExecutionPlanItemArgs]) -> String {
    let item_keys = items
        .iter()
        .map(|item| item.key.as_str())
        .collect::<Vec<_>>()
        .join(",");
    let candidate = format!("plan:{}:{item_keys}", goal_activated_memory_id.into_inner());
    if candidate.len() <= 240 {
        candidate
    } else {
        format!(
            "plan:{}",
            Uuid::new_v5(&Uuid::NAMESPACE_OID, candidate.as_bytes())
        )
    }
}

#[allow(clippy::too_many_arguments)]
async fn append_execution_plan(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    planner_root: MemoryId,
    goal_activated_memory_id: MemoryId,
    evidence: &[MemoryId],
    plan_key: &str,
    plan_summary: &str,
    payload: &CodeExecutionPlanV1,
) -> Result<PlanAppendOutcome, McpToolError> {
    let mut key = Vec::new();
    key.extend_from_slice(ctx.owner.principal.columns().1.as_bytes());
    key.extend_from_slice(ctx.owner.org_id.into_inner().as_bytes());
    key.extend_from_slice(payload.repo_id.as_bytes());
    key.extend_from_slice(goal_activated_memory_id.into_inner().as_bytes());
    key.extend_from_slice(plan_key.as_bytes());
    let memory_id = MemoryId::new(Uuid::new_v5(&Uuid::NAMESPACE_OID, &key));
    let draft = DerivedDraft {
        memory_id: memory_id.into_inner(),
        owner: ctx.owner.clone(),
        kind: EntityKind::Abstraction,
        author_personality_instance_id: ctx.author.personality_instance_id,
        schema_id: <CodeExecutionPlanV1 as AbstractionPayload>::schema_id(),
        schema_version: SchemaVersion::new(CodeExecutionPlanV1::SCHEMA_VERSION),
        text: plan_summary.to_string(),
        operator_kind: MemoryOperatorKind::ExternalAgent,
        model_id: &ctx.author.model_id,
        prompt_version: "proxima-code/code_emit_execution_plan-v1",
        supersedes: None,
        embedding: None,
        embedding_model_id: None,
    };
    let sidecar_payload = payload.clone();
    let outcome = append_derived_in_tx(tx, &draft, move |tx, outcome| {
        Box::pin(async move {
            sidecar_payload
                .insert_memory_sidecar(tx, outcome.memory_id)
                .await
        })
    })
    .await
    .map_err(McpToolError::Storage)?;
    let mut edge_ids = Vec::new();
    if !outcome.idempotent_replay {
        edge_ids.push(append_plan_authored_edge(tx, ctx, planner_root, memory_id).await?);
        edge_ids
            .push(append_plan_derived_edge(tx, ctx, memory_id, goal_activated_memory_id).await?);
        for memory_id in evidence {
            edge_ids.push(append_plan_derived_edge(tx, ctx, outcome.memory_id, *memory_id).await?);
        }
    }
    Ok(PlanAppendOutcome {
        memory_id: outcome.memory_id,
        edge_ids,
        idempotent_replay: outcome.idempotent_replay,
    })
}

async fn append_plan_authored_edge(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    planner_root: MemoryId,
    plan_memory_id: MemoryId,
) -> Result<Uuid, McpToolError> {
    let relation = ctx
        .registry
        .resolve_relation(CORE_AUTHORED_RELATION)
        .ok_or_else(|| McpToolError::Other("core/authored relation not registered".into()))?;
    let edge_id = Uuid::now_v7();
    append_edge(
        tx.as_mut(),
        edge_id,
        relation,
        Endpoint::perspective(planner_root),
        Endpoint::abstraction(plan_memory_id),
        EdgeAuthorshipKind::ExternalAgent,
        Some(planner_root),
        &ctx.owner,
    )
    .await
    .map_err(McpToolError::Storage)?;
    Ok(edge_id)
}

async fn append_plan_derived_edge(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    plan_memory_id: MemoryId,
    target_memory_id: MemoryId,
) -> Result<Uuid, McpToolError> {
    let relation = ctx
        .registry
        .resolve_relation(CORE_DERIVED_FROM_RELATION)
        .ok_or_else(|| McpToolError::Other("core/derived-from relation not registered".into()))?;
    let edge_id = Uuid::now_v7();
    append_edge(
        tx.as_mut(),
        edge_id,
        relation,
        Endpoint::abstraction(plan_memory_id),
        Endpoint::fact(target_memory_id),
        EdgeAuthorshipKind::ExternalAgent,
        ctx.caller_self_perspective,
        &ctx.owner,
    )
    .await
    .map_err(McpToolError::Storage)?;
    Ok(edge_id)
}

#[derive(Debug)]
pub struct CodeRetryExecutionRequestTool;

impl McpTool for CodeRetryExecutionRequestTool {
    const NAME: &'static str = "proxima-code/code_retry_execution_request";
    const DESCRIPTION: &'static str = "Shell-author override: retry a prior proxima-code/work-requested-v1 Fact for a target worker.";
    const PRODUCES_SCHEMA_IDS: &'static [&'static str] = &[ExecutionRequestV1::SCHEMA_ID];

    type Args = CodeRetryExecutionRequestArgs;
    type Output = CodeRetryExecutionRequestOutput;

    #[allow(clippy::too_many_lines)]
    fn call(
        ctx: McpToolCtx,
        args: CodeRetryExecutionRequestArgs,
    ) -> futures::future::BoxFuture<'static, Result<CodeRetryExecutionRequestOutput, McpToolError>>
    {
        Box::pin(async move {
            if ctx.master_token_id.is_none() {
                return Err(McpToolError::InvalidInput(
                    "code_retry_execution_request requires a master-token shell-author call".into(),
                ));
            }
            let shell_author_root = ctx.caller_self_perspective.ok_or_else(|| {
                McpToolError::InvalidInput(
                    "caller_self_perspective is required for shell-author retry provenance".into(),
                )
            })?;
            let prior_memory_id = ctx.resolve_fact_memory(&args.prior_execution_request)?;
            let target_personality_id = resolve_personality_id(&ctx, &args.target_personality)?;
            let request_key = normalize_text("idempotency_key", &args.idempotency_key, 1, 240)?;
            let explicit_evidence = resolve_evidence(&ctx, &args.evidence)?;

            let pool = pg_pool(&ctx)?;
            let mut tx = pool.begin().await.map_err(map_storage)?;
            let prior = load_execution_request(&mut tx, &ctx, prior_memory_id).await?;
            if let Some(existing) =
                find_execution_request_by_key(&mut tx, &ctx, prior.repo_id, &request_key).await?
            {
                tx.commit().await.map_err(map_storage)?;
                return Ok(CodeRetryExecutionRequestOutput {
                    handle: ctx.format_fact_memory(existing),
                    authored_edge_handle: None,
                    target_edge_handle: None,
                    derived_edge_handles: Vec::new(),
                    idempotent_replay: true,
                });
            }
            let target_root =
                validate_target_personality(&mut tx, &ctx, target_personality_id).await?;
            validate_target_execution_wake(&mut tx, &ctx, target_personality_id).await?;
            validate_evidence_in_owner(&mut tx, &ctx, &explicit_evidence).await?;

            let title = match args.title {
                Some(value) => normalize_text("title", &value, 1, 240)?,
                None => prior.title,
            };
            let instructions = retry_instructions(
                &prior.instructions,
                prior_memory_id,
                &request_key,
                args.instructions_append.as_deref(),
            )?;

            let payload = ExecutionRequestV1 {
                repo_id: prior.repo_id,
                title,
                instructions,
                request_key,
            };
            let outcome = ingest_execution_request(&mut tx, &ctx, &payload).await?;
            let (authored_edge_id, target_edge_id, derived_edge_ids) = if outcome.idempotent_replay
            {
                (None, None, Vec::new())
            } else {
                let authored_edge_id =
                    append_authored_edge(&mut tx, &ctx, shell_author_root, outcome.memory_id)
                        .await?;
                let target_edge_id =
                    append_target_edge(&mut tx, &ctx, target_root, outcome.memory_id).await?;
                let mut derived_edge_ids = Vec::new();
                let mut seen = HashSet::new();
                push_derived_edge(
                    &mut tx,
                    &ctx,
                    outcome.memory_id,
                    prior_memory_id,
                    &mut seen,
                    &mut derived_edge_ids,
                )
                .await?;
                for memory_id in load_prior_derived_targets(&mut tx, &ctx, prior_memory_id).await? {
                    push_derived_edge(
                        &mut tx,
                        &ctx,
                        outcome.memory_id,
                        memory_id,
                        &mut seen,
                        &mut derived_edge_ids,
                    )
                    .await?;
                }
                for memory_id in explicit_evidence {
                    push_derived_edge(
                        &mut tx,
                        &ctx,
                        outcome.memory_id,
                        memory_id,
                        &mut seen,
                        &mut derived_edge_ids,
                    )
                    .await?;
                }
                (
                    Some(authored_edge_id),
                    Some(target_edge_id),
                    derived_edge_ids,
                )
            };
            tx.commit().await.map_err(map_storage)?;

            Ok(CodeRetryExecutionRequestOutput {
                handle: ctx.format_fact_memory(outcome.memory_id),
                authored_edge_handle: authored_edge_id
                    .map(|edge_id| ctx.format_edge(EdgeId::new(edge_id))),
                target_edge_handle: target_edge_id
                    .map(|edge_id| ctx.format_edge(EdgeId::new(edge_id))),
                derived_edge_handles: derived_edge_ids
                    .into_iter()
                    .map(|edge_id| ctx.format_edge(EdgeId::new(edge_id)))
                    .collect(),
                idempotent_replay: outcome.idempotent_replay,
            })
        })
    }
}

pub(super) fn normalize_text(
    field: &str,
    value: &str,
    min: usize,
    max: usize,
) -> Result<String, McpToolError> {
    let trimmed = value.trim();
    let len = trimmed.chars().count();
    if len < min || len > max {
        return Err(McpToolError::InvalidInput(format!(
            "{field} must be {min}..={max} chars"
        )));
    }
    Ok(trimmed.to_string())
}

pub(super) fn validate_acceptance_criteria(
    criteria: Vec<AcceptanceCriterionV1>,
) -> Result<Vec<AcceptanceCriterionV1>, McpToolError> {
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(criteria.len());
    for mut criterion in criteria {
        criterion.key = normalize_text("acceptance_criteria.key", &criterion.key, 1, 80)?;
        criterion.description = normalize_text(
            "acceptance_criteria.description",
            &criterion.description,
            1,
            1000,
        )?;
        if !criterion
            .key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
        {
            return Err(McpToolError::InvalidInput(
                "acceptance_criteria.key must contain only ASCII letters, digits, '-' or '_'"
                    .into(),
            ));
        }
        if !seen.insert(criterion.key.clone()) {
            return Err(McpToolError::InvalidInput(format!(
                "duplicate acceptance criterion key: {}",
                criterion.key
            )));
        }
        validate_acceptance_verifier_spec(&criterion)?;
        out.push(criterion);
    }
    Ok(out)
}

fn validate_plan_items(
    items: Vec<ExecutionPlanItemArgs>,
) -> Result<Vec<ExecutionPlanItemArgs>, McpToolError> {
    if items.is_empty() || items.len() > 20 {
        return Err(McpToolError::InvalidInput(
            "items must contain 1..=20 plan requests".into(),
        ));
    }
    let mut seen = HashSet::new();
    let mut prior = HashSet::new();
    let mut out = Vec::with_capacity(items.len());
    for mut item in items {
        item.key = normalize_text("items.key", &item.key, 1, 80)?;
        if !item
            .key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
        {
            return Err(McpToolError::InvalidInput(
                "items.key must contain only ASCII letters, digits, '-' or '_'".into(),
            ));
        }
        if !seen.insert(item.key.clone()) {
            return Err(McpToolError::InvalidInput(format!(
                "duplicate item key: {}",
                item.key
            )));
        }
        item.title = normalize_text("items.title", &item.title, 1, 240)?;
        item.instructions = normalize_text("items.instructions", &item.instructions, 1, 20_000)?;
        item.idempotency_key =
            normalize_text("items.idempotency_key", &item.idempotency_key, 1, 240)?;
        item.acceptance_criteria = validate_acceptance_criteria(item.acceptance_criteria)?;
        item.test_criteria = validate_acceptance_criteria(item.test_criteria)?;
        match item.kind {
            ExecutionPlanItemKind::Implementation => {
                if !item.test_criteria.is_empty() {
                    return Err(McpToolError::InvalidInput(format!(
                        "implementation item {} must not set test_criteria",
                        item.key
                    )));
                }
            }
            ExecutionPlanItemKind::Test => {
                if !item.acceptance_criteria.is_empty() {
                    return Err(McpToolError::InvalidInput(format!(
                        "test item {} must not set acceptance_criteria",
                        item.key
                    )));
                }
                if item.test_criteria.is_empty() {
                    return Err(McpToolError::InvalidInput(format!(
                        "test item {} must set test_criteria",
                        item.key
                    )));
                }
                if !item
                    .test_criteria
                    .iter()
                    .any(|criterion| criterion.required)
                {
                    return Err(McpToolError::InvalidInput(format!(
                        "test item {} must include at least one required test criterion",
                        item.key
                    )));
                }
            }
        }
        let mut item_deps = HashSet::new();
        let mut normalized_deps = Vec::with_capacity(item.depends_on.len());
        for dep in &item.depends_on {
            let dep = normalize_text("items.depends_on[]", dep, 1, 80)?;
            if !prior.contains(&dep) {
                return Err(McpToolError::InvalidInput(format!(
                    "{} item {} depends on {}, but dependencies must reference earlier item keys",
                    item.kind.as_str(),
                    item.key,
                    dep
                )));
            }
            if !item_deps.insert(dep.clone()) {
                return Err(McpToolError::InvalidInput(format!(
                    "item {} repeats dependency {}",
                    item.key, dep
                )));
            }
            normalized_deps.push(dep);
        }
        item.depends_on = normalized_deps;
        prior.insert(item.key.clone());
        out.push(item);
    }
    Ok(out)
}

fn validate_acceptance_verifier_spec(
    criterion: &AcceptanceCriterionV1,
) -> Result<(), McpToolError> {
    match criterion.verifier_kind {
        AcceptanceVerifierKind::FileExists => {
            let path = criterion.verifier_spec.path.as_deref().ok_or_else(|| {
                McpToolError::InvalidInput(format!(
                    "acceptance_criteria.{}.verifier_spec.path is required for file_exists",
                    criterion.key
                ))
            })?;
            let _ = normalize_text("acceptance_criteria.verifier_spec.path", path, 1, 1000)?;
        }
        AcceptanceVerifierKind::Command => {
            let command = criterion.verifier_spec.command.as_ref().ok_or_else(|| {
                McpToolError::InvalidInput(format!(
                    "acceptance_criteria.{}.verifier_spec.command is required for command",
                    criterion.key
                ))
            })?;
            if command.is_empty() {
                return Err(McpToolError::InvalidInput(format!(
                    "acceptance_criteria.{}.verifier_spec.command must not be empty",
                    criterion.key
                )));
            }
            for part in command {
                let _ =
                    normalize_text("acceptance_criteria.verifier_spec.command[]", part, 1, 2000)?;
            }
        }
        AcceptanceVerifierKind::BrowserSmoke
        | AcceptanceVerifierKind::DiffScope
        | AcceptanceVerifierKind::ReviewerOnly => {}
    }
    Ok(())
}

fn retry_instructions(
    prior_instructions: &str,
    prior_memory_id: MemoryId,
    request_key: &str,
    instructions_append: Option<&str>,
) -> Result<String, McpToolError> {
    let mut instructions = format!(
        "{}\n\nRetry context:\nprior_execution_request: {}\nretry_key: {}",
        prior_instructions.trim(),
        prior_memory_id.into_inner(),
        request_key
    );
    if let Some(extra) = instructions_append {
        let extra = normalize_text("instructions_append", extra, 1, 20_000)?;
        instructions.push_str("\n\nRetry instructions:\n");
        instructions.push_str(&extra);
    }
    normalize_text("instructions", &instructions, 1, 20_000)
}

pub(super) fn resolve_personality_id(
    ctx: &McpToolCtx,
    raw: &str,
) -> Result<PersonalityInstanceId, McpToolError> {
    ctx.resolve_personality(raw)
}

fn resolve_evidence(ctx: &McpToolCtx, raw: &[String]) -> Result<Vec<MemoryId>, McpToolError> {
    raw.iter()
        .map(|value| ctx.resolve_fact_memory(value))
        .collect()
}

#[derive(Debug)]
pub(super) struct PriorExecutionRequest {
    pub repo_id: Uuid,
    pub title: String,
    pub instructions: String,
}

pub(super) async fn load_execution_request(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    memory_id: MemoryId,
) -> Result<PriorExecutionRequest, McpToolError> {
    let (owner_kind, owner_principal_id) = owner_principal(&ctx.owner);
    #[allow(clippy::type_complexity)]
    let row: Option<(
        EntityKind,
        String,
        Option<Uuid>,
        Option<String>,
        Option<String>,
    )> = sqlx::query_as(
        "SELECT COALESCE(m.kind, 'Fact'::proxima_core.entity_kind) AS kind,
                m.schema_id,
                r.repo_id,
                r.title,
                r.instructions
         FROM proxima_core.memories m
         LEFT JOIN proxima_code.work_requested_v1 r USING (memory_id)
         WHERE m.memory_id = $1
           AND m.owner_principal_kind = $2
           AND m.owner_principal_id = $3",
    )
    .bind(memory_id.into_inner())
    .bind(owner_kind)
    .bind(owner_principal_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_storage)?;
    let Some((kind, schema_id, repo_id, title, instructions)) = row else {
        return Err(McpToolError::InvalidInput(format!(
            "prior_execution_request is not visible: {}",
            memory_id.into_inner()
        )));
    };
    if kind != EntityKind::Fact || schema_id != ExecutionRequestV1::SCHEMA_ID {
        return Err(McpToolError::InvalidInput(
            "prior_execution_request must be a proxima-code/work-requested-v1 Fact".into(),
        ));
    }
    let (Some(repo_id), Some(title), Some(instructions)) = (repo_id, title, instructions) else {
        return Err(McpToolError::InvalidInput(
            "prior_execution_request sidecar row is missing".into(),
        ));
    };
    Ok(PriorExecutionRequest {
        repo_id,
        title,
        instructions,
    })
}

pub(super) async fn find_execution_request_by_key(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    repo_id: Uuid,
    request_key: &str,
) -> Result<Option<MemoryId>, McpToolError> {
    let (owner_kind, owner_principal_id) = owner_principal(&ctx.owner);
    let existing: Option<Uuid> = sqlx::query_scalar(
        "SELECT r.memory_id
         FROM proxima_code.work_requested_v1 r
         JOIN proxima_core.memories m USING (memory_id)
         WHERE r.repo_id = $1
           AND r.request_key = $2
           AND m.owner_principal_kind = $3
           AND m.owner_principal_id = $4",
    )
    .bind(repo_id)
    .bind(request_key)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_storage)?;
    Ok(existing.map(MemoryId::new))
}

pub(super) async fn validate_target_personality(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    target_personality: PersonalityInstanceId,
) -> Result<MemoryId, McpToolError> {
    let (owner_kind, owner_principal_id) = owner_principal(&ctx.owner);
    let row: Option<(Uuid, PersonalityStatus)> = sqlx::query_as(
        "SELECT current_root_perspective_memory_id, status
         FROM proxima_core.personality
         WHERE owner_principal_kind = $1
           AND owner_principal_id = $2
           AND personality_instance_id = $3",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(target_personality.into_inner())
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_storage)?;
    let Some((root, status)) = row else {
        return Err(McpToolError::InvalidInput(format!(
            "target_personality not found: {}",
            target_personality.into_inner()
        )));
    };
    if status != PersonalityStatus::Active {
        return Err(McpToolError::InvalidInput(format!(
            "target_personality is not active: {}",
            status.as_str()
        )));
    }
    Ok(MemoryId::new(root))
}

pub(super) async fn validate_target_execution_wake(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    target_personality: PersonalityInstanceId,
) -> Result<(), McpToolError> {
    let (owner_kind, owner_principal_id) = owner_principal(&ctx.owner);
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(
             SELECT 1
             FROM proxima_core.personality_wake_entries
             WHERE owner_principal_kind = $1
               AND owner_principal_id = $2
               AND personality_instance_id = $3
               AND tombstoned_at IS NULL
               AND enabled
               AND trigger_kind = 'on_memory'
               AND trigger_id = $4
         )",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(target_personality.into_inner())
    .bind(ExecutionRequestV1::SCHEMA_ID)
    .fetch_one(&mut **tx)
    .await
    .map_err(map_storage)?;
    if !exists {
        return Err(McpToolError::InvalidInput(
            "target_personality has no enabled wake entry for proxima-code/work-requested-v1"
                .into(),
        ));
    }
    Ok(())
}

pub(super) async fn load_prior_derived_targets(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    prior_memory_id: MemoryId,
) -> Result<Vec<MemoryId>, McpToolError> {
    let (owner_kind, owner_principal_id) = owner_principal(&ctx.owner);
    let rows: Vec<Uuid> = sqlx::query_scalar(
        "SELECT e.target_memory_id
         FROM proxima_core.edges e
         JOIN proxima_core.memories m
           ON m.memory_id = e.target_memory_id
          AND m.owner_principal_kind = e.owner_principal_kind
          AND m.owner_principal_id = e.owner_principal_id
         WHERE e.owner_principal_kind = $1
           AND e.owner_principal_id = $2
           AND e.relation = $3
           AND e.source_kind = 'Fact'
           AND e.source_memory_id = $4
           AND e.target_memory_id IS NOT NULL
         ORDER BY e.created_at, e.edge_id",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(CORE_DERIVED_FROM_RELATION)
    .bind(prior_memory_id.into_inner())
    .fetch_all(&mut **tx)
    .await
    .map_err(map_storage)?;
    Ok(rows.into_iter().map(MemoryId::new).collect())
}

pub(super) async fn push_derived_edge(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    request_memory_id: MemoryId,
    evidence_memory_id: MemoryId,
    seen: &mut HashSet<MemoryId>,
    edge_ids: &mut Vec<Uuid>,
) -> Result<(), McpToolError> {
    if seen.insert(evidence_memory_id) {
        edge_ids.push(append_derived_edge(tx, ctx, request_memory_id, evidence_memory_id).await?);
    }
    Ok(())
}

async fn validate_repo(ctx: &McpToolCtx, repo_id: Uuid) -> Result<(), McpToolError> {
    let (owner_kind, owner_principal_id) = owner_principal(&ctx.owner);
    let pool = pg_pool(ctx)?;
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(
             SELECT 1
             FROM proxima_code.repos
             WHERE owner_principal_kind = $1
               AND owner_principal_id = $2
               AND repo_id = $3
         )",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(repo_id)
    .fetch_one(pool.as_ref())
    .await
    .map_err(map_storage)?;
    if !exists {
        return Err(McpToolError::InvalidInput(format!(
            "repo not found for owner: {repo_id}"
        )));
    }
    Ok(())
}

async fn validate_goal_activated_fact(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    memory_id: MemoryId,
) -> Result<Uuid, McpToolError> {
    let (owner_kind, owner_principal_id) = owner_principal(&ctx.owner);
    let row: Option<(EntityKind, String, Uuid)> = sqlx::query_as(
        "SELECT COALESCE(m.kind, 'Fact'::proxima_core.entity_kind) AS kind,
                m.schema_id, g.goal_id
         FROM proxima_core.memories m
         JOIN proxima_core.goal_activated_v1 g USING (memory_id)
         WHERE m.memory_id = $1
           AND m.owner_principal_kind = $2
           AND m.owner_principal_id = $3",
    )
    .bind(memory_id.into_inner())
    .bind(owner_kind)
    .bind(owner_principal_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_storage)?;
    let Some((kind, schema_id, goal_id)) = row else {
        return Err(McpToolError::InvalidInput(format!(
            "goal_activated_memory is not visible: {}",
            memory_id.into_inner()
        )));
    };
    if kind != EntityKind::Fact || schema_id != "core/goal-activated-v1" {
        return Err(McpToolError::InvalidInput(
            "goal_activated_memory must be a core/goal-activated-v1 Fact".into(),
        ));
    }
    Ok(goal_id)
}

async fn validate_active_goal_context(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    goal_id: Uuid,
    planner_root: MemoryId,
) -> Result<(), McpToolError> {
    let (owner_kind, owner_principal_id) = owner_principal(&ctx.owner);
    let active: bool = sqlx::query_scalar(
        "SELECT EXISTS(
             SELECT 1
             FROM proxima_core.goals g
             WHERE g.goal_id = $3
               AND g.owner_principal_kind = $1
               AND g.owner_principal_id = $2
               AND g.state = 'Active'
         )",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(goal_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(map_storage)?;
    if !active {
        return Err(McpToolError::InvalidInput(
            "activated goal is not Active".into(),
        ));
    }

    let assigned: bool = sqlx::query_scalar(
        "WITH RECURSIVE lineage(goal_id) AS (
             SELECT $3::uuid
             UNION
             SELECT g.supersedes
               FROM proxima_core.goals g
               JOIN lineage l ON g.goal_id = l.goal_id
              WHERE g.supersedes IS NOT NULL
                AND g.owner_principal_kind = $1
                AND g.owner_principal_id = $2
         )
         SELECT EXISTS(
             SELECT 1
             FROM proxima_core.edges e
             JOIN lineage l ON l.goal_id = e.source_goal_id
             WHERE e.owner_principal_kind = $1
               AND e.owner_principal_id = $2
               AND e.relation = 'core/inspires'
               AND e.source_kind = 'Goal'
               AND e.target_kind = 'Perspective'
               AND e.target_memory_id = $4
         )",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(goal_id)
    .bind(planner_root.into_inner())
    .fetch_one(&mut **tx)
    .await
    .map_err(map_storage)?;
    if !assigned {
        return Err(McpToolError::InvalidInput(
            "activated goal is Active but not assigned to caller Root Perspective".into(),
        ));
    }
    Ok(())
}

async fn validate_evidence_in_owner(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    evidence: &[MemoryId],
) -> Result<(), McpToolError> {
    let (owner_kind, owner_principal_id) = owner_principal(&ctx.owner);
    for memory_id in evidence {
        let row: Option<EntityKind> = sqlx::query_scalar(
            "SELECT COALESCE(kind, 'Fact'::proxima_core.entity_kind) AS kind
             FROM proxima_core.memories
             WHERE memory_id = $1
               AND owner_principal_kind = $2
               AND owner_principal_id = $3",
        )
        .bind(memory_id.into_inner())
        .bind(owner_kind)
        .bind(owner_principal_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_storage)?;
        match row {
            Some(EntityKind::Fact) => {}
            Some(kind) => {
                return Err(McpToolError::LayeringViolation(format!(
                    "evidence {} must be a Fact memory handle; got {kind:?}",
                    memory_id.into_inner(),
                )));
            }
            None => {
                return Err(McpToolError::InvalidInput(format!(
                    "evidence not visible: {}",
                    memory_id.into_inner()
                )));
            }
        }
    }
    Ok(())
}

async fn ingest_mcp_fact<P>(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    source_id: &'static str,
    cited_object_schema: &'static str,
    mapping_schema: &'static str,
    payload: &P,
) -> Result<EventIngestOutcome, McpToolError>
where
    P: FactPayload + proxima_storage_pg::verbs::event_ingest::PgFactSidecar + Clone,
{
    let embedding_client = ctx.engine().and_then(proxima_core::Engine::embed_client);
    let ingest_ctx =
        FactIngestContext::new(&ctx.owner, source_id, SourceBatchId::new(Uuid::now_v7()))
            .embedding_model_id(embedding_client.as_ref().map(|client| client.model_id()));
    let citation = CitationSpec::v1_for_payload(cited_object_schema, payload, mapping_schema);
    ingest_fact_with_sidecar(tx, &ingest_ctx, payload, citation)
        .await
        .map_err(McpToolError::Storage)
}

pub(super) async fn ingest_execution_request(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    payload: &ExecutionRequestV1,
) -> Result<EventIngestOutcome, McpToolError> {
    ingest_mcp_fact(
        tx,
        ctx,
        EXECUTION_REQUEST_SOURCE_ID,
        EXECUTION_REQUEST_OBJECT_SCHEMA,
        EXECUTION_REQUEST_WHOLE_SCHEMA,
        payload,
    )
    .await
}

pub(super) async fn ingest_acceptance_criteria(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    payload: &AcceptanceCriteriaV1,
) -> Result<EventIngestOutcome, McpToolError> {
    ingest_mcp_fact(
        tx,
        ctx,
        ACCEPTANCE_CRITERIA_SOURCE_ID,
        ACCEPTANCE_CRITERIA_OBJECT_SCHEMA,
        ACCEPTANCE_CRITERIA_WHOLE_SCHEMA,
        payload,
    )
    .await
}

pub(super) async fn ingest_test_request(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    payload: &TestRequestV1,
) -> Result<EventIngestOutcome, McpToolError> {
    ingest_mcp_fact(
        tx,
        ctx,
        TEST_REQUEST_SOURCE_ID,
        TEST_REQUEST_OBJECT_SCHEMA,
        TEST_REQUEST_WHOLE_SCHEMA,
        payload,
    )
    .await
}

pub(super) async fn append_acceptance_criteria_edge(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    request_memory_id: MemoryId,
    criteria_memory_id: MemoryId,
) -> Result<Uuid, McpToolError> {
    let relation = ctx
        .registry
        .resolve_relation(CODE_HAS_ACCEPTANCE_CRITERIA_RELATION)
        .ok_or_else(|| {
            McpToolError::Other(format!(
                "{CODE_HAS_ACCEPTANCE_CRITERIA_RELATION} relation not registered"
            ))
        })?;
    let edge_id = Uuid::now_v7();
    append_edge(
        tx.as_mut(),
        edge_id,
        relation,
        Endpoint::fact(request_memory_id),
        Endpoint::fact(criteria_memory_id),
        EdgeAuthorshipKind::ExternalAgent,
        ctx.caller_self_perspective,
        &ctx.owner,
    )
    .await
    .map_err(McpToolError::Storage)?;
    Ok(edge_id)
}

pub(super) async fn append_authored_edge(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    planner_root: MemoryId,
    request_memory_id: MemoryId,
) -> Result<Uuid, McpToolError> {
    let relation = ctx
        .registry
        .resolve_relation(CORE_AUTHORED_RELATION)
        .ok_or_else(|| McpToolError::Other("core/authored relation not registered".into()))?;
    let edge_id = Uuid::now_v7();
    append_edge(
        tx.as_mut(),
        edge_id,
        relation,
        Endpoint::perspective(planner_root),
        Endpoint::fact(request_memory_id),
        EdgeAuthorshipKind::ExternalAgent,
        Some(planner_root),
        &ctx.owner,
    )
    .await
    .map_err(McpToolError::Storage)?;
    Ok(edge_id)
}

pub(super) async fn append_target_edge(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    target_root: MemoryId,
    request_memory_id: MemoryId,
) -> Result<Uuid, McpToolError> {
    let relation = ctx
        .registry
        .resolve_relation(CODE_TARGETS_EXECUTION_REQUEST_RELATION)
        .ok_or_else(|| {
            McpToolError::Other(format!(
                "{CODE_TARGETS_EXECUTION_REQUEST_RELATION} relation not registered"
            ))
        })?;
    let edge_id = Uuid::now_v7();
    append_edge(
        tx.as_mut(),
        edge_id,
        relation,
        Endpoint::perspective(target_root),
        Endpoint::fact(request_memory_id),
        EdgeAuthorshipKind::ExternalAgent,
        ctx.caller_self_perspective,
        &ctx.owner,
    )
    .await
    .map_err(McpToolError::Storage)?;
    Ok(edge_id)
}

pub(super) async fn append_derived_edge(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    request_memory_id: MemoryId,
    evidence_memory_id: MemoryId,
) -> Result<Uuid, McpToolError> {
    let relation = ctx
        .registry
        .resolve_relation(CORE_DERIVED_FROM_RELATION)
        .ok_or_else(|| McpToolError::Other("core/derived-from relation not registered".into()))?;
    let edge_id = Uuid::now_v7();
    append_edge(
        tx.as_mut(),
        edge_id,
        relation,
        Endpoint::fact(request_memory_id),
        Endpoint::fact(evidence_memory_id),
        EdgeAuthorshipKind::ExternalAgent,
        ctx.caller_self_perspective,
        &ctx.owner,
    )
    .await
    .map_err(McpToolError::Storage)?;
    Ok(edge_id)
}

async fn append_dependency_edge(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    dependent_memory_id: MemoryId,
    dependency_memory_id: MemoryId,
) -> Result<Uuid, McpToolError> {
    let relation = ctx
        .registry
        .resolve_relation(CORE_DEPENDS_ON_RELATION)
        .ok_or_else(|| McpToolError::Other("core/depends-on relation not registered".into()))?;
    let mut name = Vec::with_capacity(32);
    name.extend_from_slice(dependent_memory_id.into_inner().as_bytes());
    name.extend_from_slice(dependency_memory_id.into_inner().as_bytes());
    let edge_id = Uuid::new_v5(&Uuid::NAMESPACE_OID, &name);
    append_edge(
        tx.as_mut(),
        edge_id,
        relation,
        Endpoint::fact(dependent_memory_id),
        Endpoint::fact(dependency_memory_id),
        EdgeAuthorshipKind::ExternalAgent,
        ctx.caller_self_perspective,
        &ctx.owner,
    )
    .await
    .map_err(McpToolError::Storage)?;
    Ok(edge_id)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use proxima_core::mcp::{HandleTable, McpAuthorContext, McpToolExtensions, OutputMode};
    use proxima_core::{AuthPath, AuthzContext, FlavorRegistry, GroupId, OrgId, Owner, Principal};
    use sqlx::postgres::PgPoolOptions;

    use super::*;

    fn test_ctx(handles: Arc<HandleTable>) -> McpToolCtx {
        let owner = Owner {
            principal: Principal::Group(GroupId::new(Uuid::now_v7())),
            org_id: OrgId::new(Uuid::now_v7()),
        };
        let pool = PgPoolOptions::new()
            .connect_lazy("postgres://proxima:proxima@localhost/proxima")
            .expect("lazy pool");
        McpToolCtx {
            owner: owner.clone(),
            authz: AuthzContext::single_owner(&owner, AuthPath::System),
            handles: Some(handles),
            mode: OutputMode::Handles,
            registry: Arc::new(FlavorRegistry::new().freeze()),
            author: McpAuthorContext {
                model_id: "test/model".into(),
                client_name: "test".into(),
                client_version: "test".into(),
                personality_instance_id: None,
                caller_self_perspective: None,
            },
            caller_self_perspective: None,
            master_token_id: None,
            extensions: McpToolExtensions::with(pool),
            engine: None,
        }
    }

    #[tokio::test]
    async fn execution_request_evidence_accepts_only_fact_handles() {
        let handles = Arc::new(HandleTable::new());
        let fact = MemoryId::new(Uuid::now_v7());
        let abstraction = MemoryId::new(Uuid::now_v7());
        let fact_handle = handles.assign_fact_memory(fact).as_str().to_string();
        let abstraction_handle = handles
            .assign_abstraction_memory(abstraction)
            .as_str()
            .to_string();
        let ctx = test_ctx(handles);

        assert_eq!(
            resolve_evidence(&ctx, &[fact_handle]).expect("fact evidence"),
            vec![fact]
        );
        let err = resolve_evidence(&ctx, &[abstraction_handle]).expect_err("A handle rejected");
        assert!(
            err.to_string().contains("expected Fact memory handle"),
            "{err}"
        );
    }

    fn criterion(key: &str, required: bool) -> AcceptanceCriterionV1 {
        AcceptanceCriterionV1 {
            key: key.into(),
            description: format!("{key} passes"),
            required,
            verifier_kind: AcceptanceVerifierKind::Command,
            verifier_spec: crate::payloads::AcceptanceVerifierSpecV1 {
                path: None,
                command: Some(vec!["true".into()]),
                pattern: None,
                note: None,
            },
        }
    }

    #[test]
    fn validate_plan_items_accepts_mixed_implementation_and_test_nodes() {
        let items = validate_plan_items(vec![
            ExecutionPlanItemArgs {
                kind: ExecutionPlanItemKind::Implementation,
                key: "impl".into(),
                title: "Implement".into(),
                instructions: "Create the feature.".into(),
                idempotency_key: "impl-key".into(),
                depends_on: vec![],
                acceptance_criteria: vec![criterion("build", true)],
                test_criteria: vec![],
            },
            ExecutionPlanItemArgs {
                kind: ExecutionPlanItemKind::Test,
                key: "test".into(),
                title: "Test".into(),
                instructions: "Verify the feature.".into(),
                idempotency_key: "test-key".into(),
                depends_on: vec!["impl".into()],
                acceptance_criteria: vec![],
                test_criteria: vec![criterion("smoke", true)],
            },
        ])
        .expect("mixed plan validates");

        assert_eq!(items[0].kind, ExecutionPlanItemKind::Implementation);
        assert_eq!(items[1].kind, ExecutionPlanItemKind::Test);
        assert_eq!(items[1].depends_on, vec!["impl"]);
    }

    #[test]
    fn validate_plan_items_rejects_test_without_required_criteria() {
        let err = validate_plan_items(vec![ExecutionPlanItemArgs {
            kind: ExecutionPlanItemKind::Test,
            key: "test".into(),
            title: "Test".into(),
            instructions: "Verify the feature.".into(),
            idempotency_key: "test-key".into(),
            depends_on: vec![],
            acceptance_criteria: vec![],
            test_criteria: vec![criterion("optional", false)],
        }])
        .expect_err("test must require one criterion");

        assert!(
            err.to_string()
                .contains("must include at least one required test criterion"),
            "{err}"
        );
    }
}

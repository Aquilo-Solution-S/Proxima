use std::collections::{HashMap, HashSet};

use proxima_core::relation::{
    CORE_AUTHORED_RELATION, CORE_DEPENDS_ON_RELATION, CORE_DERIVED_FROM_RELATION,
    CORE_INSPIRES_RELATION,
};
use proxima_core::verbs::fact_ingest::{CitationSpec, FactIngestOutcome};
use proxima_core::verbs::goal_write::GoalState;
use proxima_core::verbs::query::{EdgeFilter, EdgeReadRequest, EdgeTargetProjection, QueryRequest};
use proxima_core::{
    AbstractionPayload, DerivedEdgeSpec, EdgeAuthorshipKind, EdgeId, EntityKind, EntityRef,
    FactPayload, GoalActivatedV1, GoalId, InputContractId, MemoryId, MemoryOperatorKind,
    OperatorId, Owner, RegisteredRelation, SchemaVersion, SourceBatchId,
};
use proxima_core::{Tool, ToolCtx, ToolError};
use proxima_storage_pg::sidecars::PgMemorySidecar;
use proxima_storage_pg::verbs::derive_append::{DerivedDraft, append_derived_with_edges_in_tx};
use proxima_storage_pg::verbs::edge_write::{MemoryEndpoint, append_owner_checked_memory_edge};
use proxima_storage_pg::verbs::fact_ingest::{FactIngestContext, ingest_fact_with_sidecar};
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

use super::CodeToolCtxExt;
use super::code_store;
use super::sql::{map_storage, owner_columns, resolve_repo_identifier};

const EXECUTION_REQUEST_SOURCE_ID: &str = "proxima-code/execution-request";
pub const CODE_TARGETS_EXECUTION_REQUEST_RELATION: &str = "proxima-code/targets-execution-request";

const EXECUTION_PLAN_OPERATOR_NAMESPACE: Uuid = Uuid::from_bytes([
    0x65, 0xf8, 0x8d, 0xc6, 0x96, 0x8c, 0x45, 0x9b, 0x8d, 0x32, 0x9a, 0xde, 0x41, 0xfa, 0x5f, 0x21,
]);
const EXECUTION_PLAN_INPUT_CONTRACT_NAMESPACE: Uuid = Uuid::from_bytes([
    0xa5, 0x1e, 0xb1, 0x22, 0xad, 0x14, 0x41, 0xda, 0xa9, 0x25, 0x11, 0x4d, 0x91, 0xa0, 0xf0, 0xdd,
]);

fn execution_plan_operator_id() -> OperatorId {
    OperatorId::new(Uuid::new_v5(
        &EXECUTION_PLAN_OPERATOR_NAMESPACE,
        b"proxima-code/emit_execution_plan-v1",
    ))
}

fn execution_plan_input_contract_id() -> InputContractId {
    InputContractId::new(Uuid::new_v5(
        &EXECUTION_PLAN_INPUT_CONTRACT_NAMESPACE,
        b"proxima-code/execution-plan:plan-source-v1",
    ))
}
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

#[derive(Debug)]
pub struct CodeEmitExecutionRequestTool;

impl Tool for CodeEmitExecutionRequestTool {
    const NAME: &'static str = "proxima-code_emit_execution_request";
    const DESCRIPTION: &'static str =
        "Emit a repo-scoped proxima-code/work-requested-v1 Fact for an Active Goal.";
    const PRODUCES_SCHEMA_IDS: &'static [&'static str] = &[ExecutionRequestV1::SCHEMA_ID];

    type Args = CodeEmitExecutionRequestArgs;
    type Output = CodeEmitExecutionRequestOutput;

    fn call(
        ctx: ToolCtx,
        args: CodeEmitExecutionRequestArgs,
    ) -> futures::future::BoxFuture<'static, Result<CodeEmitExecutionRequestOutput, ToolError>>
    {
        Box::pin(async move {
            let repo_id = resolve_repo_identifier(&ctx, &args.repo_handle).await?;
            validate_repo(&ctx, repo_id).await?;

            let title = normalize_text("title", &args.title, 1, 240)?;
            let instructions = normalize_text("instructions", &args.instructions, 1, 20_000)?;
            let request_key = normalize_text("idempotency_key", &args.idempotency_key, 1, 240)?;
            let acceptance_criteria = validate_acceptance_criteria(args.acceptance_criteria)?;

            let planner_root = ctx.caller_self_perspective().ok_or_else(|| {
                ToolError::InvalidInput(
                    "caller_self_perspective is required to author an execution request".into(),
                )
            })?;
            let goal_activated_memory_id = ctx.resolve_fact_memory(&args.goal_activated_memory)?;
            let evidence = resolve_evidence(&ctx, &args.evidence)?;

            let pool = code_store(&ctx)?;
            let mut tx = pool.pool().begin().await.map_err(map_storage)?;
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

impl Tool for CodeEmitExecutionPlanTool {
    const NAME: &'static str = "proxima-code_emit_execution_plan";
    const DESCRIPTION: &'static str = "Atomically emit a repo-scoped execution-plan Abstraction plus implementation/test request Facts and core/depends-on edges.";
    const PRODUCES_SCHEMA_IDS: &'static [&'static str] = &[
        CodeExecutionPlanV1::SCHEMA_ID,
        ExecutionRequestV1::SCHEMA_ID,
        TestRequestV1::SCHEMA_ID,
    ];

    type Args = CodeEmitExecutionPlanArgs;
    type Output = CodeEmitExecutionPlanOutput;

    #[allow(clippy::too_many_lines)]
    fn call(
        ctx: ToolCtx,
        args: CodeEmitExecutionPlanArgs,
    ) -> futures::future::BoxFuture<'static, Result<CodeEmitExecutionPlanOutput, ToolError>> {
        Box::pin(async move {
            let repo_id = resolve_repo_identifier(&ctx, &args.repo_handle).await?;
            validate_repo(&ctx, repo_id).await?;
            let plan_items = validate_plan_items(args.items)?;

            let planner_root = ctx.caller_self_perspective().ok_or_else(|| {
                ToolError::InvalidInput(
                    "caller_self_perspective is required to author an execution plan".into(),
                )
            })?;
            let goal_activated_memory_id = ctx.resolve_fact_memory(&args.goal_activated_memory)?;
            let plan_source_memory_id = ctx.resolve_abstraction_memory(&args.plan_source_memory)?;
            let evidence = resolve_evidence(&ctx, &args.evidence)?;

            let pool = code_store(&ctx)?;
            let mut tx = pool.pool().begin().await.map_err(map_storage)?;
            let goal_id =
                validate_goal_activated_fact(&mut tx, &ctx, goal_activated_memory_id).await?;
            validate_active_goal_context(&mut tx, &ctx, goal_id, planner_root).await?;
            validate_plan_source_abstraction_in_owner(&mut tx, &ctx, plan_source_memory_id).await?;
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
                plan_source_memory_id,
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
                    append_plan_fact_evidence_edge(
                        &mut tx,
                        &ctx,
                        plan_memory_id,
                        outcome.memory_id,
                    )
                    .await?,
                );
                let mut dependency_edges = Vec::new();
                for dependency_key in &item.depends_on {
                    let dependency_memory_id =
                        emitted.get(dependency_key).copied().ok_or_else(|| {
                            ToolError::InvalidInput(format!(
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

/// Deterministic, idempotent `MemoryId` for an execution plan.
///
/// Org-free (Track B / S0): the key folds the owner *principal* id, the
/// repo, the activated-goal memory, and the plan key — never a tenant/org
/// scalar. Re-issuing the same plan under the same principal reproduces
/// this id by construction.
fn execution_plan_memory_id(
    owner: &Owner,
    repo_id: Uuid,
    goal_activated_memory_id: MemoryId,
    plan_key: &str,
) -> MemoryId {
    let mut key = Vec::new();
    key.extend_from_slice(owner.stable_key_uuid().as_bytes());
    key.extend_from_slice(repo_id.as_bytes());
    key.extend_from_slice(goal_activated_memory_id.into_inner().as_bytes());
    key.extend_from_slice(plan_key.as_bytes());
    MemoryId::new(Uuid::new_v5(&Uuid::NAMESPACE_OID, &key))
}

#[allow(clippy::too_many_arguments)]
async fn append_execution_plan(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &ToolCtx,
    planner_root: MemoryId,
    goal_activated_memory_id: MemoryId,
    plan_source_memory_id: MemoryId,
    evidence: &[MemoryId],
    plan_key: &str,
    plan_summary: &str,
    payload: &CodeExecutionPlanV1,
) -> Result<PlanAppendOutcome, ToolError> {
    let owner = ctx.owner();
    let caller = super::caller(ctx)?;
    let memory_id =
        execution_plan_memory_id(&owner, payload.repo_id, goal_activated_memory_id, plan_key);
    let draft = DerivedDraft {
        memory_id: memory_id.into_inner(),
        owner,
        kind: EntityKind::Abstraction,
        schema_id: <CodeExecutionPlanV1 as AbstractionPayload>::schema_id(),
        schema_version: SchemaVersion::new(CodeExecutionPlanV1::SCHEMA_VERSION),
        text: plan_summary.to_string(),
        operator_kind: MemoryOperatorKind::AtoA,
        operator_id: execution_plan_operator_id(),
        input_contract_id: execution_plan_input_contract_id(),
        source_batch_id: None,
        model_id: caller.model_id(),
        prompt_version: "proxima-code/emit_execution_plan-v1",
        supersedes: None,
        embedding: None,
        embedding_model_id: None,
    };
    let derived_relation = ctx
        .registry()
        .resolve_relation(CORE_DERIVED_FROM_RELATION)
        .ok_or_else(|| ToolError::Other("core/derived-from relation not registered".into()))?;
    let proof_edges = [execution_plan_proof_edge(
        &owner,
        memory_id,
        plan_source_memory_id,
        derived_relation,
    )];
    let sidecar_payload = payload.clone();
    let outcome = append_derived_with_edges_in_tx(tx, &draft, &proof_edges, move |tx, outcome| {
        Box::pin(async move {
            sidecar_payload
                .insert_memory_sidecar(tx, outcome.memory_id)
                .await
        })
    })
    .await
    .map_err(ToolError::Storage)?;
    let mut edge_ids = Vec::new();
    if !outcome.idempotent_replay {
        edge_ids.push(append_plan_authored_edge(tx, ctx, planner_root, memory_id).await?);
        for memory_id in evidence {
            edge_ids.push(
                append_plan_fact_evidence_edge(tx, ctx, outcome.memory_id, *memory_id).await?,
            );
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
    ctx: &ToolCtx,
    planner_root: MemoryId,
    plan_memory_id: MemoryId,
) -> Result<Uuid, ToolError> {
    let relation = ctx
        .registry()
        .resolve_relation(CORE_AUTHORED_RELATION)
        .ok_or_else(|| ToolError::Other("core/authored relation not registered".into()))?;
    let edge_id = Uuid::now_v7();
    append_owner_checked_memory_edge(
        tx.as_mut(),
        &ctx.owner(),
        EdgeId::new(edge_id),
        relation,
        MemoryEndpoint::perspective(planner_root),
        MemoryEndpoint::abstraction(plan_memory_id),
        EdgeAuthorshipKind::ExternalAgent,
        Some(planner_root),
    )
    .await
    .map_err(ToolError::Storage)?;
    Ok(edge_id)
}

fn execution_plan_proof_edge<'a>(
    owner: &'a Owner,
    plan_memory_id: MemoryId,
    plan_source_memory_id: MemoryId,
    relation: RegisteredRelation<'a>,
) -> DerivedEdgeSpec<'a> {
    DerivedEdgeSpec {
        owner,
        relation,
        source_kind: EntityKind::Abstraction,
        source_memory_id: plan_memory_id,
        target_kind: EntityKind::Abstraction,
        target_memory_id: plan_source_memory_id,
        authorship_kind: EdgeAuthorshipKind::OperatorAtoA,
        authorship_owner_memory_id: Some(plan_source_memory_id),
        sidecar_payload: None,
    }
}

async fn append_plan_fact_evidence_edge(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &ToolCtx,
    plan_memory_id: MemoryId,
    target_memory_id: MemoryId,
) -> Result<Uuid, ToolError> {
    let relation = ctx
        .registry()
        .resolve_relation(CORE_DERIVED_FROM_RELATION)
        .ok_or_else(|| ToolError::Other("core/derived-from relation not registered".into()))?;
    let edge_id = Uuid::now_v7();
    append_owner_checked_memory_edge(
        tx.as_mut(),
        &ctx.owner(),
        EdgeId::new(edge_id),
        relation,
        MemoryEndpoint::abstraction(plan_memory_id),
        MemoryEndpoint::fact(target_memory_id),
        EdgeAuthorshipKind::ExternalAgent,
        ctx.caller_self_perspective(),
    )
    .await
    .map_err(ToolError::Storage)?;
    Ok(edge_id)
}

#[derive(Debug)]
pub struct CodeRetryExecutionRequestTool;

impl Tool for CodeRetryExecutionRequestTool {
    const NAME: &'static str = "proxima-code_retry_execution_request";
    const DESCRIPTION: &'static str = "Shell-author override: retry a prior proxima-code/work-requested-v1 Fact for a target worker.";
    const PRODUCES_SCHEMA_IDS: &'static [&'static str] = &[ExecutionRequestV1::SCHEMA_ID];

    type Args = CodeRetryExecutionRequestArgs;
    type Output = CodeRetryExecutionRequestOutput;

    #[allow(clippy::too_many_lines)]
    fn call(
        ctx: ToolCtx,
        args: CodeRetryExecutionRequestArgs,
    ) -> futures::future::BoxFuture<'static, Result<CodeRetryExecutionRequestOutput, ToolError>>
    {
        Box::pin(async move {
            if !super::caller(&ctx)?.is_master_token() {
                return Err(ToolError::InvalidInput(
                    "code_retry_execution_request requires a master-token shell-author call".into(),
                ));
            }
            let shell_author_root = ctx.caller_self_perspective().ok_or_else(|| {
                ToolError::InvalidInput(
                    "caller_self_perspective is required for shell-author retry provenance".into(),
                )
            })?;
            let prior_memory_id = ctx.resolve_fact_memory(&args.prior_execution_request)?;
            let target_perspective_id =
                resolve_target_perspective_id(&ctx, &args.target_perspective)?;
            let request_key = normalize_text("idempotency_key", &args.idempotency_key, 1, 240)?;
            let explicit_evidence = resolve_evidence(&ctx, &args.evidence)?;

            let pool = code_store(&ctx)?;
            let mut tx = pool.pool().begin().await.map_err(map_storage)?;
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
            validate_target_perspective(&mut tx, &ctx, target_perspective_id).await?;
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
                    append_target_edge(&mut tx, &ctx, target_perspective_id, outcome.memory_id)
                        .await?;
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
) -> Result<String, ToolError> {
    let trimmed = value.trim();
    let len = trimmed.chars().count();
    if len < min || len > max {
        return Err(ToolError::InvalidInput(format!(
            "{field} must be {min}..={max} chars"
        )));
    }
    Ok(trimmed.to_string())
}

pub(super) fn validate_acceptance_criteria(
    criteria: Vec<AcceptanceCriterionV1>,
) -> Result<Vec<AcceptanceCriterionV1>, ToolError> {
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
            return Err(ToolError::InvalidInput(
                "acceptance_criteria.key must contain only ASCII letters, digits, '-' or '_'"
                    .into(),
            ));
        }
        if !seen.insert(criterion.key.clone()) {
            return Err(ToolError::InvalidInput(format!(
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
) -> Result<Vec<ExecutionPlanItemArgs>, ToolError> {
    if items.is_empty() || items.len() > 20 {
        return Err(ToolError::InvalidInput(
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
            return Err(ToolError::InvalidInput(
                "items.key must contain only ASCII letters, digits, '-' or '_'".into(),
            ));
        }
        if !seen.insert(item.key.clone()) {
            return Err(ToolError::InvalidInput(format!(
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
                    return Err(ToolError::InvalidInput(format!(
                        "implementation item {} must not set test_criteria",
                        item.key
                    )));
                }
            }
            ExecutionPlanItemKind::Test => {
                if !item.acceptance_criteria.is_empty() {
                    return Err(ToolError::InvalidInput(format!(
                        "test item {} must not set acceptance_criteria",
                        item.key
                    )));
                }
                if item.test_criteria.is_empty() {
                    return Err(ToolError::InvalidInput(format!(
                        "test item {} must set test_criteria",
                        item.key
                    )));
                }
                if !item
                    .test_criteria
                    .iter()
                    .any(|criterion| criterion.required)
                {
                    return Err(ToolError::InvalidInput(format!(
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
                return Err(ToolError::InvalidInput(format!(
                    "{} item {} depends on {}, but dependencies must reference earlier item keys",
                    item.kind.as_str(),
                    item.key,
                    dep
                )));
            }
            if !item_deps.insert(dep.clone()) {
                return Err(ToolError::InvalidInput(format!(
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

fn validate_acceptance_verifier_spec(criterion: &AcceptanceCriterionV1) -> Result<(), ToolError> {
    match criterion.verifier_kind {
        AcceptanceVerifierKind::FileExists => {
            let path = criterion.verifier_spec.path.as_deref().ok_or_else(|| {
                ToolError::InvalidInput(format!(
                    "acceptance_criteria.{}.verifier_spec.path is required for file_exists",
                    criterion.key
                ))
            })?;
            let _ = normalize_text("acceptance_criteria.verifier_spec.path", path, 1, 1000)?;
        }
        AcceptanceVerifierKind::Command => {
            let command = criterion.verifier_spec.command.as_ref().ok_or_else(|| {
                ToolError::InvalidInput(format!(
                    "acceptance_criteria.{}.verifier_spec.command is required for command",
                    criterion.key
                ))
            })?;
            if command.is_empty() {
                return Err(ToolError::InvalidInput(format!(
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
) -> Result<String, ToolError> {
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

pub(super) fn resolve_target_perspective_id(
    ctx: &ToolCtx,
    raw: &str,
) -> Result<MemoryId, ToolError> {
    ctx.resolve_perspective_memory(raw)
}

fn resolve_evidence(ctx: &ToolCtx, raw: &[String]) -> Result<Vec<MemoryId>, ToolError> {
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
    _tx: &mut Transaction<'_, Postgres>,
    ctx: &ToolCtx,
    memory_id: MemoryId,
) -> Result<PriorExecutionRequest, ToolError> {
    let pool = code_store(ctx)?;
    let engine = super::engine(ctx)?;
    let Some((_, row)) = pool
        .authorized_fact_payloads::<ExecutionRequestV1>(
            &engine,
            ctx.authz(),
            ctx.owner(),
            &[memory_id.into_inner()],
            1,
        )
        .await?
        .into_iter()
        .next()
    else {
        return Err(ToolError::InvalidInput(
            "prior_execution_request must be a visible proxima-code/work-requested-v1 Fact".into(),
        ));
    };
    Ok(PriorExecutionRequest {
        repo_id: row.repo_id,
        title: row.title,
        instructions: row.instructions,
    })
}

pub(super) async fn find_execution_request_by_key(
    _tx: &mut Transaction<'_, Postgres>,
    ctx: &ToolCtx,
    repo_id: Uuid,
    request_key: &str,
) -> Result<Option<MemoryId>, ToolError> {
    let pool = code_store(ctx)?;
    let engine = super::engine(ctx)?;
    let candidates: Vec<Uuid> = sqlx::query_scalar(
        "SELECT memory_id
           FROM proxima_code.work_requested_v1
          WHERE repo_id = $1
            AND request_key = $2
          ORDER BY memory_id DESC
          LIMIT 20",
    )
    .bind(repo_id)
    .bind(request_key)
    .fetch_all(pool.pool())
    .await
    .map_err(map_storage)?;
    Ok(pool
        .authorized_fact_payloads::<ExecutionRequestV1>(
            &engine,
            ctx.authz(),
            ctx.owner(),
            &candidates,
            1,
        )
        .await?
        .into_iter()
        .next()
        .map(|(id, _)| id))
}

pub(super) async fn validate_target_perspective(
    _tx: &mut Transaction<'_, Postgres>,
    ctx: &ToolCtx,
    target_perspective: MemoryId,
) -> Result<(), ToolError> {
    let pool = code_store(ctx)?;
    let engine = super::engine(ctx)?;
    let visible = pool
        .authorized_memory_ids(
            &engine,
            ctx.authz(),
            ctx.owner(),
            &[target_perspective.into_inner()],
            EntityKind::Perspective,
            None,
            1,
        )
        .await?;
    if visible.is_empty() {
        return Err(ToolError::InvalidInput(format!(
            "target_perspective not found: {}",
            target_perspective.into_inner()
        )));
    }
    Ok(())
}

pub(super) async fn load_prior_derived_targets(
    _tx: &mut Transaction<'_, Postgres>,
    ctx: &ToolCtx,
    prior_memory_id: MemoryId,
) -> Result<Vec<MemoryId>, ToolError> {
    let engine = super::engine(ctx)?;
    let response = engine
        .read_edges(
            ctx.authz(),
            &EdgeReadRequest {
                principal: ctx.owner(),
                edge_ids: Vec::new(),
                filter: EdgeFilter {
                    relation: Some(CORE_DERIVED_FROM_RELATION.to_string()),
                    source: Some(EntityRef::Memory(prior_memory_id)),
                    target: None,
                },
                limit: 500,
            },
        )
        .await?;
    Ok(response
        .edges
        .into_iter()
        .filter_map(|edge| match edge.target {
            EdgeTargetProjection::Visible {
                target: EntityRef::Memory(id),
            } => Some(id),
            EdgeTargetProjection::Visible { .. }
            | EdgeTargetProjection::Redacted
            | EdgeTargetProjection::Unavailable => None,
        })
        .collect())
}

pub(super) async fn push_derived_edge(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &ToolCtx,
    request_memory_id: MemoryId,
    evidence_memory_id: MemoryId,
    seen: &mut HashSet<MemoryId>,
    edge_ids: &mut Vec<Uuid>,
) -> Result<(), ToolError> {
    if seen.insert(evidence_memory_id) {
        edge_ids.push(append_derived_edge(tx, ctx, request_memory_id, evidence_memory_id).await?);
    }
    Ok(())
}

async fn validate_repo(ctx: &ToolCtx, repo_id: Uuid) -> Result<(), ToolError> {
    let (owner_kind, owner_id) = owner_columns(&ctx.owner());
    let pool = code_store(ctx)?;
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(
             SELECT 1
             FROM proxima_code.repos
             WHERE owner_kind = $1
               AND owner_id = $2
               AND repo_id = $3
         )",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .bind(repo_id)
    .fetch_one(pool.pool())
    .await
    .map_err(map_storage)?;
    if !exists {
        return Err(ToolError::InvalidInput(format!(
            "repo not found for owner: {repo_id}"
        )));
    }
    Ok(())
}

async fn validate_goal_activated_fact(
    _tx: &mut Transaction<'_, Postgres>,
    ctx: &ToolCtx,
    memory_id: MemoryId,
) -> Result<Uuid, ToolError> {
    let pool = code_store(ctx)?;
    let engine = super::engine(ctx)?;
    let Some((_, payload)) = pool
        .authorized_fact_payloads::<GoalActivatedV1>(
            &engine,
            ctx.authz(),
            ctx.owner(),
            &[memory_id.into_inner()],
            1,
        )
        .await?
        .into_iter()
        .next()
    else {
        return Err(ToolError::InvalidInput(format!(
            "goal_activated_memory is not visible: {}",
            memory_id.into_inner()
        )));
    };
    Ok(payload.goal_id)
}

async fn validate_active_goal_context(
    _tx: &mut Transaction<'_, Postgres>,
    ctx: &ToolCtx,
    goal_id: Uuid,
    planner_root: MemoryId,
) -> Result<(), ToolError> {
    let engine = super::engine(ctx)?;
    let mut req = QueryRequest::for_principal(ctx.owner());
    req.entity_kind = Some(EntityKind::Goal);
    req.goal_ids = vec![GoalId::new(goal_id)];
    req.limit = 1;
    let response = engine.query(ctx.authz(), &req).await?;
    let Some(goal) = response.goals.into_iter().next() else {
        return Err(ToolError::InvalidInput(
            "activated goal is not visible".into(),
        ));
    };
    if goal.state != GoalState::Active {
        return Err(ToolError::InvalidInput(
            "activated goal is not Active".into(),
        ));
    }

    if !goal_lineage_assigned_to(ctx, GoalId::new(goal_id), planner_root).await? {
        return Err(ToolError::InvalidInput(
            "activated goal is Active but not assigned to caller Root Perspective".into(),
        ));
    }
    Ok(())
}

async fn goal_lineage_assigned_to(
    ctx: &ToolCtx,
    start: GoalId,
    planner_root: MemoryId,
) -> Result<bool, ToolError> {
    let engine = super::engine(ctx)?;
    let mut current = Some(start);
    let mut seen = HashSet::new();
    for _ in 0..100 {
        let Some(goal_id) = current else {
            return Ok(false);
        };
        if !seen.insert(goal_id) {
            return Ok(false);
        }

        let edges = engine
            .read_edges(
                ctx.authz(),
                &EdgeReadRequest {
                    principal: ctx.owner(),
                    edge_ids: Vec::new(),
                    filter: EdgeFilter {
                        relation: Some(CORE_INSPIRES_RELATION.to_string()),
                        source: Some(EntityRef::Goal(goal_id)),
                        target: Some(EntityRef::Memory(planner_root)),
                    },
                    limit: 1,
                },
            )
            .await?;
        if !edges.edges.is_empty() {
            return Ok(true);
        }

        let mut req = QueryRequest::for_principal(ctx.owner());
        req.entity_kind = Some(EntityKind::Goal);
        req.goal_ids = vec![goal_id];
        req.limit = 1;
        req.include_payloads = false;
        current = engine
            .query(ctx.authz(), &req)
            .await?
            .goals
            .into_iter()
            .next()
            .and_then(|goal| goal.supersedes);
    }
    Ok(false)
}

async fn validate_plan_source_abstraction_in_owner(
    _tx: &mut Transaction<'_, Postgres>,
    ctx: &ToolCtx,
    memory_id: MemoryId,
) -> Result<(), ToolError> {
    let pool = code_store(ctx)?;
    let engine = super::engine(ctx)?;
    let visible = pool
        .authorized_memory_ids(
            &engine,
            ctx.authz(),
            ctx.owner(),
            &[memory_id.into_inner()],
            EntityKind::Abstraction,
            None,
            1,
        )
        .await?;
    if visible.is_empty() {
        return Err(ToolError::InvalidInput(format!(
            "plan_source_memory not visible: {}",
            memory_id.into_inner()
        )));
    }
    Ok(())
}

async fn validate_evidence_in_owner(
    _tx: &mut Transaction<'_, Postgres>,
    ctx: &ToolCtx,
    evidence: &[MemoryId],
) -> Result<(), ToolError> {
    let pool = code_store(ctx)?;
    let engine = super::engine(ctx)?;
    for memory_id in evidence {
        let visible = pool
            .authorized_memory_ids(
                &engine,
                ctx.authz(),
                ctx.owner(),
                &[memory_id.into_inner()],
                EntityKind::Fact,
                None,
                1,
            )
            .await?;
        if visible.is_empty() {
            return Err(ToolError::InvalidInput(format!(
                "evidence not visible or not a Fact: {}",
                memory_id.into_inner()
            )));
        }
    }
    Ok(())
}

async fn ingest_mcp_fact<P>(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &ToolCtx,
    source_id: &'static str,
    cited_object_schema: &'static str,
    mapping_schema: &'static str,
    payload: &P,
) -> Result<FactIngestOutcome, ToolError>
where
    P: FactPayload + PgMemorySidecar + Clone,
{
    let embedding_client = ctx.engine().and_then(|engine| engine.embed_client());
    let owner = ctx.owner();
    let ingest_ctx = FactIngestContext::new(&owner, source_id, SourceBatchId::new(Uuid::now_v7()))
        .embedding_model_id(embedding_client.as_ref().map(|client| client.model_id()));
    let citation = CitationSpec::v1_for_payload(cited_object_schema, payload, mapping_schema);
    ingest_fact_with_sidecar(tx, &ingest_ctx, payload, citation)
        .await
        .map_err(ToolError::Storage)
}

pub(super) async fn ingest_execution_request(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &ToolCtx,
    payload: &ExecutionRequestV1,
) -> Result<FactIngestOutcome, ToolError> {
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
    ctx: &ToolCtx,
    payload: &AcceptanceCriteriaV1,
) -> Result<FactIngestOutcome, ToolError> {
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
    ctx: &ToolCtx,
    payload: &TestRequestV1,
) -> Result<FactIngestOutcome, ToolError> {
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
    ctx: &ToolCtx,
    request_memory_id: MemoryId,
    criteria_memory_id: MemoryId,
) -> Result<Uuid, ToolError> {
    let relation = ctx
        .registry()
        .resolve_relation(CODE_HAS_ACCEPTANCE_CRITERIA_RELATION)
        .ok_or_else(|| {
            ToolError::Other(format!(
                "{CODE_HAS_ACCEPTANCE_CRITERIA_RELATION} relation not registered"
            ))
        })?;
    let edge_id = Uuid::now_v7();
    append_owner_checked_memory_edge(
        tx.as_mut(),
        &ctx.owner(),
        EdgeId::new(edge_id),
        relation,
        MemoryEndpoint::fact(request_memory_id),
        MemoryEndpoint::fact(criteria_memory_id),
        EdgeAuthorshipKind::ExternalAgent,
        ctx.caller_self_perspective(),
    )
    .await
    .map_err(ToolError::Storage)?;
    Ok(edge_id)
}

pub(super) async fn append_authored_edge(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &ToolCtx,
    planner_root: MemoryId,
    request_memory_id: MemoryId,
) -> Result<Uuid, ToolError> {
    let relation = ctx
        .registry()
        .resolve_relation(CORE_AUTHORED_RELATION)
        .ok_or_else(|| ToolError::Other("core/authored relation not registered".into()))?;
    let edge_id = Uuid::now_v7();
    append_owner_checked_memory_edge(
        tx.as_mut(),
        &ctx.owner(),
        EdgeId::new(edge_id),
        relation,
        MemoryEndpoint::perspective(planner_root),
        MemoryEndpoint::fact(request_memory_id),
        EdgeAuthorshipKind::ExternalAgent,
        Some(planner_root),
    )
    .await
    .map_err(ToolError::Storage)?;
    Ok(edge_id)
}

pub(super) async fn append_target_edge(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &ToolCtx,
    target_root: MemoryId,
    request_memory_id: MemoryId,
) -> Result<Uuid, ToolError> {
    let relation = ctx
        .registry()
        .resolve_relation(CODE_TARGETS_EXECUTION_REQUEST_RELATION)
        .ok_or_else(|| {
            ToolError::Other(format!(
                "{CODE_TARGETS_EXECUTION_REQUEST_RELATION} relation not registered"
            ))
        })?;
    let edge_id = Uuid::now_v7();
    append_owner_checked_memory_edge(
        tx.as_mut(),
        &ctx.owner(),
        EdgeId::new(edge_id),
        relation,
        MemoryEndpoint::perspective(target_root),
        MemoryEndpoint::fact(request_memory_id),
        EdgeAuthorshipKind::ExternalAgent,
        ctx.caller_self_perspective(),
    )
    .await
    .map_err(ToolError::Storage)?;
    Ok(edge_id)
}

pub(super) async fn append_derived_edge(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &ToolCtx,
    request_memory_id: MemoryId,
    evidence_memory_id: MemoryId,
) -> Result<Uuid, ToolError> {
    let relation = ctx
        .registry()
        .resolve_relation(CORE_DERIVED_FROM_RELATION)
        .ok_or_else(|| ToolError::Other("core/derived-from relation not registered".into()))?;
    let edge_id = Uuid::now_v7();
    append_owner_checked_memory_edge(
        tx.as_mut(),
        &ctx.owner(),
        EdgeId::new(edge_id),
        relation,
        MemoryEndpoint::fact(request_memory_id),
        MemoryEndpoint::fact(evidence_memory_id),
        EdgeAuthorshipKind::ExternalAgent,
        ctx.caller_self_perspective(),
    )
    .await
    .map_err(ToolError::Storage)?;
    Ok(edge_id)
}

async fn append_dependency_edge(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &ToolCtx,
    dependent_memory_id: MemoryId,
    dependency_memory_id: MemoryId,
) -> Result<Uuid, ToolError> {
    let relation = ctx
        .registry()
        .resolve_relation(CORE_DEPENDS_ON_RELATION)
        .ok_or_else(|| ToolError::Other("core/depends-on relation not registered".into()))?;
    let mut name = Vec::with_capacity(32);
    name.extend_from_slice(dependent_memory_id.into_inner().as_bytes());
    name.extend_from_slice(dependency_memory_id.into_inner().as_bytes());
    let edge_id = Uuid::new_v5(&Uuid::NAMESPACE_OID, &name);
    append_owner_checked_memory_edge(
        tx.as_mut(),
        &ctx.owner(),
        EdgeId::new(edge_id),
        relation,
        MemoryEndpoint::fact(dependent_memory_id),
        MemoryEndpoint::fact(dependency_memory_id),
        EdgeAuthorshipKind::ExternalAgent,
        ctx.caller_self_perspective(),
    )
    .await
    .map_err(ToolError::Storage)?;
    Ok(edge_id)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use proxima_core::mcp::{HandleTable, McpToolCaller, McpToolPresentation, OutputMode};
    use proxima_core::{
        AuthPath, AuthzContext, FlavorRegistry, GroupId, OwnerRef, ToolServices, UserId,
    };

    use super::*;

    /// Pins the org-free execution-plan `MemoryId` against drift. Track B
    /// / S0: the v5 key folds the owner *principal* id ‖ repo ‖ goal
    /// memory ‖ plan key — no org. A fixed input must reproduce exactly
    /// this uuid so re-issued plans stay idempotent.
    #[test]
    fn execution_plan_memory_id_golden_is_org_free() {
        let owner = OwnerRef::Personal(UserId::new(
            Uuid::parse_str("00000000-0000-0000-0000-000000000001").expect("uuid literal"),
        ));
        let repo_id =
            Uuid::parse_str("00000000-0000-0000-0000-0000000000aa").expect("uuid literal");
        let goal_activated = MemoryId::new(
            Uuid::parse_str("00000000-0000-0000-0000-0000000000bb").expect("uuid literal"),
        );
        let id = execution_plan_memory_id(&owner, repo_id, goal_activated, "plan:golden");
        assert_eq!(
            id.into_inner(),
            Uuid::parse_str("ec0bf05d-c797-559d-bdf8-9583028201cf").expect("uuid literal")
        );
    }

    fn test_ctx(handles: Arc<HandleTable>) -> ToolCtx {
        let owner = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
        let mut services = ToolServices::new();
        services.insert(McpToolPresentation::new(Some(handles), OutputMode::Handles));
        services.insert(McpToolCaller::new("test/model".into(), false));
        ToolCtx::new(
            owner,
            AuthzContext::single_owner(&owner, AuthPath::System),
            Arc::new(FlavorRegistry::new().freeze_or_panic_for_tests()),
            services,
        )
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

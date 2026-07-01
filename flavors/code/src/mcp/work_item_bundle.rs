use proxima_core::relation::{CORE_DEPENDS_ON_RELATION, CORE_DERIVED_FROM_RELATION};
use proxima_core::verbs::query::{EdgeFilter, EdgeReadRequest, EdgeTargetProjection, EntityKind};
use proxima_core::{AbstractionPayload, EntityRef, FactPayload, GoalActivatedV1, MemoryId};
use proxima_core::{Tool, ToolCtx, ToolError};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::payloads::{
    AcceptanceCriterionV1, AcceptanceVerifierKind, AcceptanceVerifierSpecV1, CodeExecutionPlanV1,
    ExecutionRequestV1, TestRequestV1,
};

use super::CodeToolCtxExt;
use super::code_store;
use super::emit_execution_request::CODE_TARGETS_EXECUTION_REQUEST_RELATION;
use super::sql::{map_storage, owner_columns};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CodeWorkItemBundleArgs {
    #[schemars(
        description = "`F...` handle for a proxima-code/work-requested-v1 or proxima-code/test-requested-v1 Fact."
    )]
    pub handle: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkItemBundleKind {
    Work,
    Test,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkItemPayloadBundle {
    Work {
        repo_id: Uuid,
        title: String,
        instructions: String,
        request_key: String,
    },
    Test {
        repo_id: Uuid,
        title: String,
        instructions: String,
        test_key: String,
        criteria: Vec<AcceptanceCriterionV1>,
    },
}

#[derive(Debug, Serialize)]
pub struct RepoBundle {
    pub repo_id: Uuid,
    pub display_name: Option<String>,
    pub canonical_path: Option<String>,
    pub target_branch: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CriteriaBundle {
    pub handle: String,
    pub criteria: Vec<AcceptanceCriterionV1>,
}

#[derive(Debug, Serialize)]
pub struct ResultBundle {
    pub handle: String,
    pub status: String,
    pub summary: String,
    pub artifact_refs: Vec<String>,
    pub log_excerpt: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct AcceptanceVerificationBundle {
    pub handle: String,
    pub criterion_key: String,
    pub status: String,
    pub summary: String,
    pub artifact_refs: Vec<String>,
    pub verifier_handle: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ActiveGoalProvenanceBundle {
    pub goal_activated_handle: String,
    pub goal_handle: String,
}

#[derive(Debug, Serialize)]
pub struct CodeWorkItemBundleOutput {
    pub handle: String,
    pub kind: WorkItemBundleKind,
    pub repo: RepoBundle,
    pub payload: WorkItemPayloadBundle,
    pub criteria: Vec<CriteriaBundle>,
    pub dependency_handles: Vec<String>,
    pub dependent_handles: Vec<String>,
    pub evidence_handles: Vec<String>,
    pub target_perspective_handles: Vec<String>,
    pub plan_handles: Vec<String>,
    pub active_goal_provenance: Vec<ActiveGoalProvenanceBundle>,
    pub result_handles: Vec<ResultBundle>,
    pub acceptance_verifications: Vec<AcceptanceVerificationBundle>,
    pub acceptance_summary_handles: Vec<String>,
}

#[derive(Debug)]
pub struct CodeWorkItemBundleTool;

impl Tool for CodeWorkItemBundleTool {
    const NAME: &'static str = "proxima-code_work_item_bundle";
    const DESCRIPTION: &'static str = "Read a Goal-native Code work/test item bundle: request, repo, criteria, dependencies, evidence, target Perspectives, active-goal provenance, and results.";
    const PRODUCES_SCHEMA_IDS: &'static [&'static str] = &[];

    type Args = CodeWorkItemBundleArgs;
    type Output = CodeWorkItemBundleOutput;

    fn call(
        ctx: ToolCtx,
        args: CodeWorkItemBundleArgs,
    ) -> futures::future::BoxFuture<'static, Result<CodeWorkItemBundleOutput, ToolError>> {
        Box::pin(async move {
            let memory_id = ctx.resolve_fact_memory(&args.handle)?;
            let item = load_work_item(&ctx, memory_id).await?;
            let repo = load_repo(&ctx, item.repo_id).await?;
            let criteria = load_criteria(&ctx, memory_id).await?;
            let dependency_handles = load_memory_edge_targets(
                &ctx,
                memory_id,
                CORE_DEPENDS_ON_RELATION,
                EdgeDirection::Outgoing,
            )
            .await?
            .into_iter()
            .map(|id| ctx.format_fact_memory(id))
            .collect();
            let dependent_handles = load_memory_edge_targets(
                &ctx,
                memory_id,
                CORE_DEPENDS_ON_RELATION,
                EdgeDirection::Incoming,
            )
            .await?
            .into_iter()
            .map(|id| ctx.format_fact_memory(id))
            .collect();
            let derived_targets = load_memory_edge_targets(
                &ctx,
                memory_id,
                CORE_DERIVED_FROM_RELATION,
                EdgeDirection::Outgoing,
            )
            .await?;
            let mut evidence_handles = Vec::new();
            let mut active_goal_provenance = Vec::new();
            for target in derived_targets {
                if let Some(goal_id) = load_goal_activation(&ctx, target).await? {
                    active_goal_provenance.push(ActiveGoalProvenanceBundle {
                        goal_activated_handle: ctx.format_fact_memory(target),
                        goal_handle: ctx.format_goal(proxima_core::GoalId::new(goal_id)),
                    });
                } else {
                    evidence_handles.push(ctx.format_fact_memory(target));
                }
            }
            let target_perspective_handles = load_target_perspectives(&ctx, memory_id)
                .await?
                .into_iter()
                .map(|id| ctx.format_perspective_memory(id))
                .collect();
            let plan_handles = load_plan_sources(&ctx, memory_id)
                .await?
                .into_iter()
                .map(|id| ctx.format_abstraction_memory(id))
                .collect();
            let result_handles = load_results(&ctx, memory_id, item.kind).await?;
            let acceptance_verifications = load_acceptance_verifications(&ctx, memory_id).await?;
            let acceptance_summary_handles = load_acceptance_summaries(&ctx, memory_id)
                .await?
                .into_iter()
                .map(|id| ctx.format_abstraction_memory(id))
                .collect();

            Ok(CodeWorkItemBundleOutput {
                handle: ctx.format_fact_memory(memory_id),
                kind: item.kind,
                repo,
                payload: item.payload,
                criteria,
                dependency_handles,
                dependent_handles,
                evidence_handles,
                target_perspective_handles,
                plan_handles,
                active_goal_provenance,
                result_handles,
                acceptance_verifications,
                acceptance_summary_handles,
            })
        })
    }
}

#[derive(Debug)]
struct WorkItemRow {
    kind: WorkItemBundleKind,
    repo_id: Uuid,
    payload: WorkItemPayloadBundle,
}

async fn load_work_item(ctx: &ToolCtx, memory_id: MemoryId) -> Result<WorkItemRow, ToolError> {
    let pool = code_store(ctx)?;
    let engine = super::engine(ctx)?;
    let candidates = [memory_id.into_inner()];
    if let Some((_, row)) = pool
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
    {
        let repo_id = row.repo_id;
        return Ok(WorkItemRow {
            kind: WorkItemBundleKind::Work,
            repo_id,
            payload: WorkItemPayloadBundle::Work {
                repo_id,
                title: row.title,
                instructions: row.instructions,
                request_key: row.request_key,
            },
        });
    }
    if let Some((_, row)) = pool
        .authorized_fact_payloads::<TestRequestV1>(
            &engine,
            ctx.authz(),
            ctx.owner(),
            &candidates,
            1,
        )
        .await?
        .into_iter()
        .next()
    {
        let repo_id = row.repo_id;
        let criteria = load_criterion_rows(
            ctx,
            "proxima_code.test_requested_criterion_v1",
            "test_requested_memory_id",
            memory_id,
        )
        .await?;
        return Ok(WorkItemRow {
            kind: WorkItemBundleKind::Test,
            repo_id,
            payload: WorkItemPayloadBundle::Test {
                repo_id,
                title: row.title,
                instructions: row.instructions,
                test_key: row.test_key,
                criteria,
            },
        });
    }
    Err(ToolError::InvalidInput(format!(
        "handle must reference {} or {} and be visible",
        ExecutionRequestV1::SCHEMA_ID,
        TestRequestV1::SCHEMA_ID,
    )))
}

async fn load_repo(ctx: &ToolCtx, repo_id: Uuid) -> Result<RepoBundle, ToolError> {
    let (owner_kind, owner_id) = owner_columns(&ctx.owner());
    let pool = code_store(ctx)?;
    let row: Option<(String, String, Option<String>)> = sqlx::query_as(
        "SELECT display_name, canonical_path, target_branch
           FROM proxima_code.repos
          WHERE owner_kind = $1
            AND owner_id = $2
            AND repo_id = $3",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .bind(repo_id)
    .fetch_optional(pool.pool())
    .await
    .map_err(map_storage)?;
    Ok(match row {
        Some((display_name, canonical_path, target_branch)) => RepoBundle {
            repo_id,
            display_name: Some(display_name),
            canonical_path: Some(canonical_path),
            target_branch,
        },
        None => RepoBundle {
            repo_id,
            display_name: None,
            canonical_path: None,
            target_branch: None,
        },
    })
}

async fn load_criteria(
    ctx: &ToolCtx,
    memory_id: MemoryId,
) -> Result<Vec<CriteriaBundle>, ToolError> {
    let pool = code_store(ctx)?;
    let rows: Vec<Uuid> = sqlx::query_scalar(
        "SELECT memory_id
           FROM proxima_code.acceptance_criteria_v1
          WHERE work_item_memory_id = $1
          ORDER BY created_at ASC",
    )
    .bind(memory_id.into_inner())
    .fetch_all(pool.pool())
    .await
    .map_err(map_storage)?;
    let mut bundles = Vec::with_capacity(rows.len());
    for id in rows {
        let memory_id = MemoryId::new(id);
        let criteria = load_criterion_rows(
            ctx,
            "proxima_code.acceptance_criterion_v1",
            "criteria_memory_id",
            memory_id,
        )
        .await?;
        bundles.push(CriteriaBundle {
            handle: ctx.format_fact_memory(memory_id),
            criteria,
        });
    }
    Ok(bundles)
}

#[derive(Debug, sqlx::FromRow)]
struct CriterionSqlRow {
    criterion_key: String,
    description: String,
    required: bool,
    verifier_kind: AcceptanceVerifierKind,
    verifier_path: Option<String>,
    verifier_command: Option<Vec<String>>,
    verifier_pattern: Option<String>,
    verifier_note: Option<String>,
}

async fn load_criterion_rows(
    ctx: &ToolCtx,
    table: &'static str,
    parent_column: &'static str,
    parent_id: MemoryId,
) -> Result<Vec<AcceptanceCriterionV1>, ToolError> {
    let sql = format!(
        "SELECT criterion_key, description, required, verifier_kind,
                verifier_path, verifier_command, verifier_pattern, verifier_note
           FROM {table}
          WHERE {parent_column} = $1
          ORDER BY criterion_index ASC"
    );
    let pool = code_store(ctx)?;
    let rows: Vec<CriterionSqlRow> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .bind(parent_id.into_inner())
        .fetch_all(pool.pool())
        .await
        .map_err(map_storage)?;
    Ok(rows
        .into_iter()
        .map(|row| AcceptanceCriterionV1 {
            key: row.criterion_key,
            description: row.description,
            required: row.required,
            verifier_kind: row.verifier_kind,
            verifier_spec: AcceptanceVerifierSpecV1 {
                path: row.verifier_path,
                command: row.verifier_command,
                pattern: row.verifier_pattern,
                note: row.verifier_note,
            },
        })
        .collect())
}

#[derive(Debug, Clone, Copy)]
enum EdgeDirection {
    Outgoing,
    Incoming,
}

async fn load_memory_edge_targets(
    ctx: &ToolCtx,
    memory_id: MemoryId,
    relation: &str,
    direction: EdgeDirection,
) -> Result<Vec<MemoryId>, ToolError> {
    let engine = super::engine(ctx)?;
    let endpoint = EntityRef::Memory(memory_id);
    let filter = match direction {
        EdgeDirection::Outgoing => EdgeFilter {
            relation: Some(relation.to_string()),
            source: Some(endpoint),
            target: None,
        },
        EdgeDirection::Incoming => EdgeFilter {
            relation: Some(relation.to_string()),
            source: None,
            target: Some(endpoint),
        },
    };
    let response = engine
        .read_edges(
            ctx.authz(),
            &EdgeReadRequest {
                owner: ctx.owner(),
                edge_ids: Vec::new(),
                filter,
                limit: 500,
            },
        )
        .await?;
    let mut out = Vec::new();
    for edge in response.edges {
        match direction {
            EdgeDirection::Outgoing => {
                if let EdgeTargetProjection::Visible {
                    target: EntityRef::Memory(id),
                } = edge.target
                {
                    out.push(id);
                }
            }
            EdgeDirection::Incoming => {
                if let EntityRef::Memory(id) = edge.source {
                    out.push(id);
                }
            }
        }
    }
    Ok(out)
}

async fn load_goal_activation(
    ctx: &ToolCtx,
    memory_id: MemoryId,
) -> Result<Option<Uuid>, ToolError> {
    let pool = code_store(ctx)?;
    let engine = super::engine(ctx)?;
    Ok(pool
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
        .map(|(_, payload)| payload.goal_id))
}

async fn load_target_perspectives(
    ctx: &ToolCtx,
    memory_id: MemoryId,
) -> Result<Vec<MemoryId>, ToolError> {
    let candidates = load_memory_edge_targets(
        ctx,
        memory_id,
        CODE_TARGETS_EXECUTION_REQUEST_RELATION,
        EdgeDirection::Incoming,
    )
    .await?;
    let pool = code_store(ctx)?;
    let engine = super::engine(ctx)?;
    pool.authorized_memory_ids(
        &engine,
        ctx.authz(),
        ctx.owner(),
        &candidates
            .into_iter()
            .map(MemoryId::into_inner)
            .collect::<Vec<_>>(),
        EntityKind::Perspective,
        None,
        500,
    )
    .await
}

async fn load_plan_sources(ctx: &ToolCtx, memory_id: MemoryId) -> Result<Vec<MemoryId>, ToolError> {
    let candidates = load_memory_edge_targets(
        ctx,
        memory_id,
        CORE_DERIVED_FROM_RELATION,
        EdgeDirection::Incoming,
    )
    .await?;
    let pool = code_store(ctx)?;
    let engine = super::engine(ctx)?;
    pool.authorized_memory_ids(
        &engine,
        ctx.authz(),
        ctx.owner(),
        &candidates
            .into_iter()
            .map(MemoryId::into_inner)
            .collect::<Vec<_>>(),
        EntityKind::Abstraction,
        Some(<CodeExecutionPlanV1 as AbstractionPayload>::schema_id()),
        500,
    )
    .await
}

async fn load_results(
    ctx: &ToolCtx,
    memory_id: MemoryId,
    kind: WorkItemBundleKind,
) -> Result<Vec<ResultBundle>, ToolError> {
    let (table, fk) = match kind {
        WorkItemBundleKind::Work => (
            "proxima_code.execution_result_v1",
            "work_requested_memory_id",
        ),
        WorkItemBundleKind::Test => ("proxima_code.test_result_v1", "test_requested_memory_id"),
    };
    let sql = format!(
        "SELECT memory_id, status::text, summary, artifact_refs, log_excerpt
           FROM {table}
          WHERE {fk} = $1
          ORDER BY created_at ASC"
    );
    let pool = code_store(ctx)?;
    let rows: Vec<ResultSqlRow> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .bind(memory_id.into_inner())
        .fetch_all(pool.pool())
        .await
        .map_err(map_storage)?;
    Ok(rows
        .into_iter()
        .map(|row| ResultBundle {
            handle: ctx.format_fact_memory(MemoryId::new(row.memory_id)),
            status: row.status,
            summary: row.summary,
            artifact_refs: row.artifact_refs,
            log_excerpt: row.log_excerpt,
        })
        .collect())
}

#[derive(Debug, sqlx::FromRow)]
struct ResultSqlRow {
    memory_id: Uuid,
    status: String,
    summary: String,
    artifact_refs: Vec<String>,
    log_excerpt: Option<String>,
}

async fn load_acceptance_verifications(
    ctx: &ToolCtx,
    memory_id: MemoryId,
) -> Result<Vec<AcceptanceVerificationBundle>, ToolError> {
    let pool = code_store(ctx)?;
    let rows: Vec<AcceptanceVerificationSqlRow> = sqlx::query_as(
        "SELECT memory_id, criterion_key, status::text, summary, artifact_refs, verifier_memory_id
           FROM proxima_code.acceptance_verification_v1
          WHERE work_item_memory_id = $1
          ORDER BY created_at ASC",
    )
    .bind(memory_id.into_inner())
    .fetch_all(pool.pool())
    .await
    .map_err(map_storage)?;
    Ok(rows
        .into_iter()
        .map(|row| AcceptanceVerificationBundle {
            handle: ctx.format_fact_memory(MemoryId::new(row.memory_id)),
            criterion_key: row.criterion_key,
            status: row.status,
            summary: row.summary,
            artifact_refs: row.artifact_refs,
            verifier_handle: row
                .verifier_memory_id
                .map(|id| ctx.format_fact_memory(MemoryId::new(id))),
        })
        .collect())
}

#[derive(Debug, sqlx::FromRow)]
struct AcceptanceVerificationSqlRow {
    memory_id: Uuid,
    criterion_key: String,
    status: String,
    summary: String,
    artifact_refs: Vec<String>,
    verifier_memory_id: Option<Uuid>,
}

async fn load_acceptance_summaries(
    ctx: &ToolCtx,
    memory_id: MemoryId,
) -> Result<Vec<MemoryId>, ToolError> {
    let pool = code_store(ctx)?;
    let rows: Vec<Uuid> = sqlx::query_scalar(
        "SELECT memory_id
           FROM proxima_code.acceptance_summary_v1
          WHERE work_item_memory_id = $1
          ORDER BY created_at ASC",
    )
    .bind(memory_id.into_inner())
    .fetch_all(pool.pool())
    .await
    .map_err(map_storage)?;
    Ok(rows.into_iter().map(MemoryId::new).collect())
}

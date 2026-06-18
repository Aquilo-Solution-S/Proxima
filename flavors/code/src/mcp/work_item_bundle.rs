use proxima_core::mcp::{McpTool, McpToolCtx, McpToolError};
use proxima_core::relation::{CORE_DEPENDS_ON_RELATION, CORE_DERIVED_FROM_RELATION};
use proxima_core::{FactPayload, MemoryId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::payloads::{ExecutionRequestV1, TestRequestV1};

use super::emit_execution_request::CODE_TARGETS_EXECUTION_REQUEST_RELATION;
use super::sql::{map_storage, owner_principal};

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
pub struct RepoBundle {
    pub repo_id: Uuid,
    pub display_name: Option<String>,
    pub canonical_path: Option<String>,
    pub target_branch: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CriteriaBundle {
    pub handle: String,
    pub criteria: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct ResultBundle {
    pub handle: String,
    pub status: String,
    pub summary: String,
    pub artifact_refs: serde_json::Value,
    pub log_excerpt: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct AcceptanceVerificationBundle {
    pub handle: String,
    pub criterion_key: String,
    pub status: String,
    pub summary: String,
    pub artifact_refs: serde_json::Value,
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
    pub payload: serde_json::Value,
    pub criteria: Vec<CriteriaBundle>,
    pub dependency_handles: Vec<String>,
    pub dependent_handles: Vec<String>,
    pub evidence_handles: Vec<String>,
    pub target_personality_handles: Vec<String>,
    pub plan_handles: Vec<String>,
    pub active_goal_provenance: Vec<ActiveGoalProvenanceBundle>,
    pub result_handles: Vec<ResultBundle>,
    pub acceptance_verifications: Vec<AcceptanceVerificationBundle>,
    pub acceptance_summary_handles: Vec<String>,
}

#[derive(Debug)]
pub struct CodeWorkItemBundleTool;

impl McpTool for CodeWorkItemBundleTool {
    const NAME: &'static str = "proxima-code/code_work_item_bundle";
    const DESCRIPTION: &'static str = "Read a Goal-native Code work/test item bundle: request, repo, criteria, dependencies, evidence, target personality, active-goal provenance, and results.";
    const PRODUCES_SCHEMA_IDS: &'static [&'static str] = &[];

    type Args = CodeWorkItemBundleArgs;
    type Output = CodeWorkItemBundleOutput;

    fn call(
        ctx: McpToolCtx,
        args: CodeWorkItemBundleArgs,
    ) -> futures::future::BoxFuture<'static, Result<CodeWorkItemBundleOutput, McpToolError>> {
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
            let target_personality_handles = load_target_personalities(&ctx, memory_id)
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
                target_personality_handles,
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
    payload: serde_json::Value,
}

#[derive(Debug, sqlx::FromRow)]
struct WorkItemSqlRow {
    schema_id: String,
    work_repo_id: Option<Uuid>,
    work_title: Option<String>,
    work_instructions: Option<String>,
    request_key: Option<String>,
    test_repo_id: Option<Uuid>,
    test_title: Option<String>,
    test_instructions: Option<String>,
    test_key: Option<String>,
    criteria_json: Option<serde_json::Value>,
}

async fn load_work_item(
    ctx: &McpToolCtx,
    memory_id: MemoryId,
) -> Result<WorkItemRow, McpToolError> {
    let (owner_kind, owner_principal_id) = owner_principal(&ctx.owner);
    let row: Option<WorkItemSqlRow> = sqlx::query_as(
        "SELECT m.schema_id,
                w.repo_id AS work_repo_id,
                w.title AS work_title,
                w.instructions AS work_instructions,
                w.request_key,
                t.repo_id AS test_repo_id,
                t.title AS test_title,
                t.instructions AS test_instructions,
                t.test_key,
                t.criteria_json
           FROM proxima_core.memories m
           LEFT JOIN proxima_code.work_requested_v1 w USING (memory_id)
           LEFT JOIN proxima_code.test_requested_v1 t USING (memory_id)
          WHERE m.memory_id = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4
            AND m.tombstoned_at IS NULL",
    )
    .bind(memory_id.into_inner())
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(ctx.owner.org_id.into_inner())
    .fetch_optional(&ctx.pool)
    .await
    .map_err(map_storage)?;

    let Some(row) = row else {
        return Err(McpToolError::InvalidInput("work item not visible".into()));
    };
    if row.schema_id == ExecutionRequestV1::SCHEMA_ID {
        let repo_id = row
            .work_repo_id
            .ok_or_else(|| McpToolError::Other("missing work sidecar".into()))?;
        return Ok(WorkItemRow {
            kind: WorkItemBundleKind::Work,
            repo_id,
            payload: serde_json::json!({
                "repo_id": repo_id,
                "title": row.work_title,
                "instructions": row.work_instructions,
                "request_key": row.request_key,
            }),
        });
    }
    if row.schema_id == TestRequestV1::SCHEMA_ID {
        let repo_id = row
            .test_repo_id
            .ok_or_else(|| McpToolError::Other("missing test sidecar".into()))?;
        return Ok(WorkItemRow {
            kind: WorkItemBundleKind::Test,
            repo_id,
            payload: serde_json::json!({
                "repo_id": repo_id,
                "title": row.test_title,
                "instructions": row.test_instructions,
                "test_key": row.test_key,
                "criteria": row.criteria_json,
            }),
        });
    }
    Err(McpToolError::InvalidInput(format!(
        "handle must reference {} or {}; got {}",
        ExecutionRequestV1::SCHEMA_ID,
        TestRequestV1::SCHEMA_ID,
        row.schema_id,
    )))
}

async fn load_repo(ctx: &McpToolCtx, repo_id: Uuid) -> Result<RepoBundle, McpToolError> {
    let (owner_kind, owner_principal_id) = owner_principal(&ctx.owner);
    let row: Option<(String, String, Option<String>)> = sqlx::query_as(
        "SELECT display_name, canonical_path, target_branch
           FROM proxima_code.repos
          WHERE owner_principal_kind = $1
            AND owner_principal_id = $2
            AND owner_org_id = $3
            AND repo_id = $4",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(ctx.owner.org_id.into_inner())
    .bind(repo_id)
    .fetch_optional(&ctx.pool)
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
    ctx: &McpToolCtx,
    memory_id: MemoryId,
) -> Result<Vec<CriteriaBundle>, McpToolError> {
    let rows: Vec<(Uuid, serde_json::Value)> = sqlx::query_as(
        "SELECT memory_id, criteria_json
           FROM proxima_code.acceptance_criteria_v1
          WHERE work_item_memory_id = $1
          ORDER BY created_at ASC",
    )
    .bind(memory_id.into_inner())
    .fetch_all(&ctx.pool)
    .await
    .map_err(map_storage)?;
    Ok(rows
        .into_iter()
        .map(|(id, criteria)| CriteriaBundle {
            handle: ctx.format_fact_memory(MemoryId::new(id)),
            criteria,
        })
        .collect())
}

#[derive(Debug, Clone, Copy)]
enum EdgeDirection {
    Outgoing,
    Incoming,
}

async fn load_memory_edge_targets(
    ctx: &McpToolCtx,
    memory_id: MemoryId,
    relation: &str,
    direction: EdgeDirection,
) -> Result<Vec<MemoryId>, McpToolError> {
    let (owner_kind, owner_principal_id) = owner_principal(&ctx.owner);
    let (source_predicate, target_column) = match direction {
        EdgeDirection::Outgoing => ("source_memory_id = $1", "target_memory_id"),
        EdgeDirection::Incoming => ("target_memory_id = $1", "source_memory_id"),
    };
    let sql = format!(
        "SELECT {target_column}
           FROM proxima_core.edges
          WHERE {source_predicate}
            AND relation = $2
            AND owner_principal_kind = $3
            AND owner_principal_id = $4
            AND owner_org_id = $5
            AND {target_column} IS NOT NULL
          ORDER BY created_at ASC"
    );
    let rows: Vec<Uuid> = sqlx::query_scalar(&sql)
        .bind(memory_id.into_inner())
        .bind(relation)
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(ctx.owner.org_id.into_inner())
        .fetch_all(&ctx.pool)
        .await
        .map_err(map_storage)?;
    Ok(rows.into_iter().map(MemoryId::new).collect())
}

async fn load_goal_activation(
    ctx: &McpToolCtx,
    memory_id: MemoryId,
) -> Result<Option<Uuid>, McpToolError> {
    let row: Option<Uuid> = sqlx::query_scalar(
        "SELECT goal_id
           FROM proxima_core.goal_activated_v1
          WHERE memory_id = $1",
    )
    .bind(memory_id.into_inner())
    .fetch_optional(&ctx.pool)
    .await
    .map_err(map_storage)?;
    Ok(row)
}

async fn load_target_personalities(
    ctx: &McpToolCtx,
    memory_id: MemoryId,
) -> Result<Vec<MemoryId>, McpToolError> {
    load_memory_edge_targets(
        ctx,
        memory_id,
        CODE_TARGETS_EXECUTION_REQUEST_RELATION,
        EdgeDirection::Incoming,
    )
    .await
}

async fn load_plan_sources(
    ctx: &McpToolCtx,
    memory_id: MemoryId,
) -> Result<Vec<MemoryId>, McpToolError> {
    let candidates = load_memory_edge_targets(
        ctx,
        memory_id,
        CORE_DERIVED_FROM_RELATION,
        EdgeDirection::Incoming,
    )
    .await?;
    let mut plans = Vec::new();
    for candidate in candidates {
        let schema_id: Option<String> =
            sqlx::query_scalar("SELECT schema_id FROM proxima_core.memories WHERE memory_id = $1")
                .bind(candidate.into_inner())
                .fetch_optional(&ctx.pool)
                .await
                .map_err(map_storage)?;
        if schema_id.as_deref() == Some("proxima-code/execution-plan-v1") {
            plans.push(candidate);
        }
    }
    Ok(plans)
}

async fn load_results(
    ctx: &McpToolCtx,
    memory_id: MemoryId,
    kind: WorkItemBundleKind,
) -> Result<Vec<ResultBundle>, McpToolError> {
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
    let rows: Vec<(Uuid, String, String, serde_json::Value, Option<String>)> = sqlx::query_as(&sql)
        .bind(memory_id.into_inner())
        .fetch_all(&ctx.pool)
        .await
        .map_err(map_storage)?;
    Ok(rows
        .into_iter()
        .map(
            |(id, status, summary, artifact_refs, log_excerpt)| ResultBundle {
                handle: ctx.format_fact_memory(MemoryId::new(id)),
                status,
                summary,
                artifact_refs,
                log_excerpt,
            },
        )
        .collect())
}

async fn load_acceptance_verifications(
    ctx: &McpToolCtx,
    memory_id: MemoryId,
) -> Result<Vec<AcceptanceVerificationBundle>, McpToolError> {
    let rows: Vec<(
        Uuid,
        String,
        String,
        String,
        serde_json::Value,
        Option<Uuid>,
    )> = sqlx::query_as(
        "SELECT memory_id, criterion_key, status::text, summary, artifact_refs, verifier_memory_id
           FROM proxima_code.acceptance_verification_v1
          WHERE work_item_memory_id = $1
          ORDER BY created_at ASC",
    )
    .bind(memory_id.into_inner())
    .fetch_all(&ctx.pool)
    .await
    .map_err(map_storage)?;
    Ok(rows
        .into_iter()
        .map(
            |(id, criterion_key, status, summary, artifact_refs, verifier)| {
                AcceptanceVerificationBundle {
                    handle: ctx.format_fact_memory(MemoryId::new(id)),
                    criterion_key,
                    status,
                    summary,
                    artifact_refs,
                    verifier_handle: verifier.map(|id| ctx.format_fact_memory(MemoryId::new(id))),
                }
            },
        )
        .collect())
}

async fn load_acceptance_summaries(
    ctx: &McpToolCtx,
    memory_id: MemoryId,
) -> Result<Vec<MemoryId>, McpToolError> {
    let rows: Vec<Uuid> = sqlx::query_scalar(
        "SELECT memory_id
           FROM proxima_code.acceptance_summary_v1
          WHERE work_item_memory_id = $1
          ORDER BY created_at ASC",
    )
    .bind(memory_id.into_inner())
    .fetch_all(&ctx.pool)
    .await
    .map_err(map_storage)?;
    Ok(rows.into_iter().map(MemoryId::new).collect())
}

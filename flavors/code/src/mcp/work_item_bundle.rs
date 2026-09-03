use proxima_core::verbs::query::{EdgeFilter, EdgeReadRequest, EntityKind};
use proxima_core::{
    AbstractionPayload, EdgeEndpoint, EdgeKind, EntityRef, FactPayload, MemoryId,
    PerspectivePayload,
};
use proxima_core::{Tool, ToolCtx, ToolError};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::payloads::{
    AcceptanceCriteriaV1, AcceptanceCriterionV1, CodeExecutionPlanV1, CodeWorkAssignmentV1,
    ExecutionRequestV1, TestRequestV1,
};

use super::CodeToolCtxExt;
use super::code_store;
use super::sql::{map_storage, owner_columns};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CodeWorkItemBundleArgs {
    #[schemars(
        description = "`F...` handle for a proxima-code/work-requested-v1 or proxima-code/test-requested-v1 Fact."
    )]
    pub handle: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkItemBundleKind {
    Work,
    Test,
}

#[derive(Debug, Serialize, JsonSchema)]
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

#[derive(Debug, Serialize, JsonSchema)]
pub struct RepoBundle {
    pub repo_id: Uuid,
    pub display_name: Option<String>,
    pub canonical_path: Option<String>,
    pub target_branch: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct CriteriaBundle {
    pub handle: String,
    pub criteria: Vec<AcceptanceCriterionV1>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ResultBundle {
    pub handle: String,
    pub status: String,
    pub summary: String,
    pub artifact_refs: Vec<String>,
    pub log_excerpt: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct AcceptanceVerificationBundle {
    pub handle: String,
    pub criterion_key: String,
    pub status: String,
    pub summary: String,
    pub artifact_refs: Vec<String>,
    pub verifier_handle: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ActiveGoalProvenanceBundle {
    pub goal_activated_handle: String,
    pub goal_handle: String,
}

#[derive(Debug, Serialize, JsonSchema)]
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
    const ANNOTATIONS: Option<proxima_core::mcp::McpToolAnnotations> = Some(super::READ_ONLY);
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
            // A work item's only outgoing references are the items it
            // depends on — the dependency is a property of the depending
            // row, so it is the item's own payload that declares it.
            let dependency_handles = load_work_item_neighbours(
                &ctx,
                memory_id,
                EdgeKind::Reference,
                EdgeDirection::Outgoing,
            )
            .await?
            .into_iter()
            .map(|id| ctx.format_fact_memory(id))
            .collect();
            // Incoming references reach this item from several kinds of
            // node — the criteria Fact, the plan Abstraction, an assignment
            // Perspective — so the dependents are the ones that are
            // themselves work/test requests.
            let dependent_handles = load_work_item_neighbours(
                &ctx,
                memory_id,
                EdgeKind::Reference,
                EdgeDirection::Incoming,
            )
            .await?
            .into_iter()
            .map(|id| ctx.format_fact_memory(id))
            .collect();
            let derived_targets = load_memory_edge_targets(
                &ctx,
                memory_id,
                EdgeKind::Origin,
                EdgeDirection::Outgoing,
            )
            .await?;
            let activations = code_store(&ctx)?
                .active_goal_activations(ctx.owner(), &derived_targets)
                .await?;
            let activated: std::collections::HashSet<MemoryId> =
                activations.iter().map(|(target, _)| *target).collect();
            let active_goal_provenance = activations
                .into_iter()
                .map(|(target, goal_id)| ActiveGoalProvenanceBundle {
                    goal_activated_handle: ctx.format_fact_memory(target),
                    goal_handle: ctx.format_goal(goal_id),
                })
                .collect();
            let evidence_handles = derived_targets
                .into_iter()
                .filter(|target| !activated.contains(target))
                .map(|id| ctx.format_fact_memory(id))
                .collect();
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
    if let Some((_, row)) = proxima::flavor::authorized_fact_payloads::<ExecutionRequestV1>(
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
    if let Some((_, row)) = proxima::flavor::authorized_fact_payloads::<TestRequestV1>(
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
        let criteria = pool.test_requested_criteria(memory_id.into_inner()).await?;
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
    let engine = super::engine(ctx)?;
    let groups = pool
        .acceptance_criteria_for_work_item(memory_id.into_inner())
        .await?;
    if groups.is_empty() {
        return Ok(Vec::new());
    }
    let candidate_ts: Vec<Uuid> = groups
        .iter()
        .map(|group| group.memory_id.into_inner())
        .collect();
    let visible = proxima::flavor::authorized_memory_ids(
        &engine,
        ctx.authz(),
        ctx.owner(),
        &candidate_ts,
        EntityKind::Fact,
        Some(AcceptanceCriteriaV1::schema_id()),
        candidate_ts.len(),
    )
    .await?;
    Ok(groups
        .into_iter()
        .filter(|group| visible.contains(&group.memory_id))
        .map(|group| CriteriaBundle {
            handle: ctx.format_fact_memory(group.memory_id),
            criteria: group.criteria,
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
    kind: EdgeKind,
    direction: EdgeDirection,
) -> Result<Vec<MemoryId>, ToolError> {
    let engine = super::engine(ctx)?;
    let endpoint = EntityRef::Memory(memory_id);
    let filter = match direction {
        EdgeDirection::Outgoing => EdgeFilter {
            kind: Some(kind),
            source: Some(endpoint),
            target: None,
        },
        EdgeDirection::Incoming => EdgeFilter {
            kind: Some(kind),
            source: None,
            target: Some(endpoint),
        },
    };
    let response = engine
        .read_edges(
            ctx.authz(),
            &EdgeReadRequest {
                owner: ctx.owner(),
                filter,
                limit: 500,
                cursor: None,
            },
        )
        .await?;
    let mut out = Vec::new();
    for edge in response.edges {
        let endpoint = match direction {
            EdgeDirection::Outgoing => edge.target.endpoint(),
            EdgeDirection::Incoming => Some(edge.source),
        };
        if let Some(id) = endpoint.and_then(EdgeEndpoint::memory_id) {
            out.push(id);
        }
    }
    Ok(out)
}

/// Neighbours that are themselves work/test requests.
///
/// Kind alone is not enough: a `reference` row reaching a request comes
/// from whichever node declared it (criteria Fact, plan Abstraction,
/// assignment Perspective). Filter by schema.
async fn load_work_item_neighbours(
    ctx: &ToolCtx,
    memory_id: MemoryId,
    kind: EdgeKind,
    direction: EdgeDirection,
) -> Result<Vec<MemoryId>, ToolError> {
    let candidates = load_memory_edge_targets(ctx, memory_id, kind, direction).await?;
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let raw = candidates
        .iter()
        .map(|id| (*id).into_inner())
        .collect::<Vec<_>>();
    let engine = super::engine(ctx)?;
    let mut out = Vec::new();
    for schema_id in [
        <ExecutionRequestV1 as FactPayload>::schema_id(),
        <TestRequestV1 as FactPayload>::schema_id(),
    ] {
        out.extend(
            proxima::flavor::authorized_memory_ids(
                &engine,
                ctx.authz(),
                ctx.owner(),
                &raw,
                EntityKind::Fact,
                Some(schema_id),
                500,
            )
            .await?,
        );
    }
    // Request order, so the bundle reads the same way twice.
    Ok(candidates
        .into_iter()
        .filter(|id| out.contains(id))
        .collect())
}

/// Workers this item is assigned to: incoming assignment Perspectives,
/// return the worker each names.
async fn load_target_perspectives(
    ctx: &ToolCtx,
    memory_id: MemoryId,
) -> Result<Vec<MemoryId>, ToolError> {
    let candidates =
        load_memory_edge_targets(ctx, memory_id, EdgeKind::Reference, EdgeDirection::Incoming)
            .await?;
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let pool = code_store(ctx)?;
    let engine = super::engine(ctx)?;
    let assignments = proxima::flavor::authorized_memory_ids(
        &engine,
        ctx.authz(),
        ctx.owner(),
        &candidates
            .into_iter()
            .map(MemoryId::into_inner)
            .collect::<Vec<_>>(),
        EntityKind::Perspective,
        Some(<CodeWorkAssignmentV1 as PerspectivePayload>::schema_id()),
        500,
    )
    .await?;
    if assignments.is_empty() {
        return Ok(Vec::new());
    }
    let workers: Vec<Uuid> = sqlx::query_scalar(
        "SELECT target_perspective_memory_id
           FROM proxima_code.work_assignment_v1
          WHERE t = ANY($1::uuid[])
          ORDER BY t",
    )
    .bind(
        assignments
            .into_iter()
            .map(MemoryId::into_inner)
            .collect::<Vec<_>>(),
    )
    .fetch_all(pool.pool())
    .await
    .map_err(map_storage)?;
    proxima::flavor::authorized_memory_ids(
        &engine,
        ctx.authz(),
        ctx.owner(),
        &workers,
        EntityKind::Perspective,
        None,
        500,
    )
    .await
}

/// Plans that name this item. The plan's payload references the request
/// Fact behind each of its items, so the connection is an incoming
/// `reference` from an execution-plan Abstraction.
async fn load_plan_sources(ctx: &ToolCtx, memory_id: MemoryId) -> Result<Vec<MemoryId>, ToolError> {
    let candidates =
        load_memory_edge_targets(ctx, memory_id, EdgeKind::Reference, EdgeDirection::Incoming)
            .await?;
    let engine = super::engine(ctx)?;
    proxima::flavor::authorized_memory_ids(
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
        "SELECT t AS memory_id, status::text, summary, artifact_refs, log_excerpt
           FROM {table}
          WHERE {fk} = $1
          ORDER BY t ASC"
    );
    let pool = code_store(ctx)?;
    // SQL-POLICY: fixed-fragment — the only interpolation is `fk`, chosen
    // from a closed match above; `memory_id` is bound.
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
        "SELECT t AS memory_id, criterion_key, status::text, summary, artifact_refs, verifier_memory_id
           FROM proxima_code.acceptance_verification_v1
          WHERE work_item_memory_id = $1
          ORDER BY t ASC",
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
        "SELECT t
           FROM proxima_code.acceptance_summary_v1
          WHERE work_item_memory_id = $1
          ORDER BY t ASC",
    )
    .bind(memory_id.into_inner())
    .fetch_all(pool.pool())
    .await
    .map_err(map_storage)?;
    Ok(rows.into_iter().map(MemoryId::new).collect())
}

#[cfg(test)]
mod tests {
    #[test]
    fn criteria_and_goals_are_not_per_row_queries() {
        let src = include_str!("work_item_bundle.rs");
        let production = src.split("mod tests").next().expect("tests module");
        assert!(
            !production.contains("load_criterion_rows"),
            "criteria load in one query, not per parent"
        );
        assert!(
            !production.contains("query_active_goals"),
            "goal activation is one store call"
        );
        assert!(
            production.contains("acceptance_criteria_for_work_item"),
            "work criteria go through the store JOIN"
        );
        assert!(
            production.contains("test_requested_criteria"),
            "test criteria go through the store"
        );
        assert!(
            production.contains("active_goal_activations"),
            "goal activation is batched"
        );
    }
}

use std::collections::HashSet;

use proxima_core::mcp::{McpTool, McpToolCtx, McpToolError};
use proxima_core::personality::{PersonalityInstanceId, PersonalityStatus};
use proxima_core::relation::{CORE_AUTHORED_RELATION, CORE_DERIVED_FROM_RELATION};
use proxima_core::verbs::event_ingest::{CitationMappingHint, CitedObjectHint, EventDraft};
use proxima_core::{
    EdgeAuthorshipKind, EdgeId, EntityKind, FactPayload, MemoryId, SchemaId, SchemaVersion,
    SourceBatchId, SourceId,
};
use proxima_storage_pg::verbs::edge_append::{EdgeDraft, append_edge_in_tx};
use proxima_storage_pg::verbs::event_ingest::ingest_event_in_tx;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::payloads::{AcceptanceCriteriaV1, AcceptanceCriterionV1, ExecutionRequestV1};

use super::sql::{map_storage, owner_principal, resolve_repo_identifier};

const EXECUTION_REQUEST_SOURCE_ID: &str = "proxima-code/execution-request";
const EXECUTION_REQUEST_OBJECT_SCHEMA: &str = "proxima-code/execution-request-object-v1";
const EXECUTION_REQUEST_WHOLE_SCHEMA: &str = "proxima-code/execution-request-whole-v1";
pub const CODE_TARGETS_EXECUTION_REQUEST_RELATION: &str = "proxima-code/targets-execution-request";
pub const CODE_HAS_ACCEPTANCE_CRITERIA_RELATION: &str = "proxima-code/has-acceptance-criteria";
const ACCEPTANCE_CRITERIA_SOURCE_ID: &str = "proxima-code/acceptance-criteria";
const ACCEPTANCE_CRITERIA_OBJECT_SCHEMA: &str = "proxima-code/acceptance-criteria-object-v1";
const ACCEPTANCE_CRITERIA_WHOLE_SCHEMA: &str = "proxima-code/acceptance-criteria-whole-v1";

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CodeEmitExecutionRequestArgs {
    pub repo_handle: String,
    pub title: String,
    pub instructions: String,
    pub idempotency_key: String,
    pub goal_activated_memory: String,
    #[serde(default)]
    pub evidence: Vec<String>,
    #[serde(default)]
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

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CodeRetryExecutionRequestArgs {
    pub prior_execution_request: String,
    pub target_personality: String,
    pub idempotency_key: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub instructions_append: Option<String>,
    #[serde(default)]
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
        "Emit a repo-scoped proxima-code/execution-request-v1 Fact for an Active Goal.";
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
            let goal_activated_memory_id = resolve_memory_id(&ctx, &args.goal_activated_memory)?;
            let evidence = resolve_evidence(&ctx, &args.evidence)?;

            let mut tx = ctx.pool.begin().await.map_err(map_storage)?;
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
                    insert_sidecar(&mut tx, outcome.memory_id, &payload).await?;
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
                            execution_request_memory_id: outcome.memory_id.into_inner(),
                            criteria: acceptance_criteria,
                        };
                        let criteria_outcome =
                            ingest_acceptance_criteria(&mut tx, &ctx, &criteria_payload).await?;
                        if criteria_outcome.idempotent_replay {
                            (Some(criteria_outcome.memory_id), None)
                        } else {
                            insert_acceptance_criteria_sidecar(
                                &mut tx,
                                criteria_outcome.memory_id,
                                &criteria_payload,
                            )
                            .await?;
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
                handle: ctx.format_memory(outcome.memory_id),
                authored_edge_handle: authored_edge_id
                    .map(|edge_id| ctx.format_edge(EdgeId::new(edge_id))),
                derived_edge_handles: derived_edge_ids
                    .into_iter()
                    .map(|edge_id| ctx.format_edge(EdgeId::new(edge_id)))
                    .collect(),
                acceptance_criteria_handle: acceptance_memory_id.map(|id| ctx.format_memory(id)),
                acceptance_criteria_edge_handle: acceptance_edge_id
                    .map(|edge_id| ctx.format_edge(EdgeId::new(edge_id))),
                idempotent_replay: outcome.idempotent_replay,
            })
        })
    }
}

#[derive(Debug)]
pub struct CodeRetryExecutionRequestTool;

impl McpTool for CodeRetryExecutionRequestTool {
    const NAME: &'static str = "proxima-code/code_retry_execution_request";
    const DESCRIPTION: &'static str = "Shell-author override: retry a prior proxima-code/execution-request-v1 Fact for a target worker.";
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
            let prior_memory_id = resolve_memory_id(&ctx, &args.prior_execution_request)?;
            let target_personality_id = resolve_personality_id(&ctx, &args.target_personality)?;
            let request_key = normalize_text("idempotency_key", &args.idempotency_key, 1, 240)?;
            let explicit_evidence = resolve_evidence(&ctx, &args.evidence)?;

            let mut tx = ctx.pool.begin().await.map_err(map_storage)?;
            let prior = load_execution_request(&mut tx, &ctx, prior_memory_id).await?;
            if let Some(existing) =
                find_execution_request_by_key(&mut tx, &ctx, prior.repo_id, &request_key).await?
            {
                tx.commit().await.map_err(map_storage)?;
                return Ok(CodeRetryExecutionRequestOutput {
                    handle: ctx.format_memory(existing),
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
                insert_sidecar(&mut tx, outcome.memory_id, &payload).await?;
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
                handle: ctx.format_memory(outcome.memory_id),
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
        out.push(criterion);
    }
    Ok(out)
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

pub(super) fn resolve_memory_id(ctx: &McpToolCtx, raw: &str) -> Result<MemoryId, McpToolError> {
    ctx.resolve_memory(raw)
}

pub(super) fn resolve_personality_id(
    ctx: &McpToolCtx,
    raw: &str,
) -> Result<PersonalityInstanceId, McpToolError> {
    ctx.resolve_personality(raw)
}

fn resolve_evidence(ctx: &McpToolCtx, raw: &[String]) -> Result<Vec<MemoryId>, McpToolError> {
    raw.iter()
        .map(|value| resolve_memory_id(ctx, value))
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
         LEFT JOIN proxima_code.execution_request_v1 r USING (memory_id)
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
            "prior_execution_request must be a proxima-code/execution-request-v1 Fact".into(),
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
         FROM proxima_code.execution_request_v1 r
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
               AND execution_mode = 'workspace'
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
            "target_personality has no enabled workspace wake entry for proxima-code/execution-request-v1"
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
    .fetch_one(&ctx.pool)
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
         JOIN proxima_goal.goal_activated_v1 g USING (memory_id)
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
    if kind != EntityKind::Fact || schema_id != "proxima-goal/goal-activated-v1" {
        return Err(McpToolError::InvalidInput(
            "goal_activated_memory must be a proxima-goal/goal-activated-v1 Fact".into(),
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
            Some(EntityKind::Fact | EntityKind::Abstraction) => {}
            Some(_) => {
                return Err(McpToolError::LayeringViolation(format!(
                    "evidence {} must be Fact or Abstraction",
                    memory_id.into_inner()
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

pub(super) async fn ingest_execution_request(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    payload: &ExecutionRequestV1,
) -> Result<proxima_core::verbs::event_ingest::EventIngestOutcome, McpToolError> {
    let mut payload_bytes = Vec::new();
    ciborium::ser::into_writer(payload, &mut payload_bytes)
        .map_err(|err| McpToolError::InvalidInput(err.to_string()))?;
    let content_hash = blake3::hash(&payload_bytes);
    let observed_at = time::OffsetDateTime::now_utc();
    let draft = EventDraft {
        source_id: SourceId::new(EXECUTION_REQUEST_SOURCE_ID),
        source_batch_id: SourceBatchId::new(Uuid::now_v7()),
        owner: ctx.owner.clone(),
        schema_id: SchemaId::new(ExecutionRequestV1::SCHEMA_ID.into()),
        schema_version: SchemaVersion::new(ExecutionRequestV1::SCHEMA_VERSION),
        payload: payload_bytes,
        observed_at,
        occurred_at: observed_at,
        cited_object: CitedObjectHint {
            schema_id: SchemaId::new(EXECUTION_REQUEST_OBJECT_SCHEMA.into()),
            schema_version: SchemaVersion::new(1),
            content_hash: *content_hash.as_bytes(),
        },
        citation_mapping: CitationMappingHint {
            schema_id: SchemaId::new(EXECUTION_REQUEST_WHOLE_SCHEMA.into()),
            schema_version: SchemaVersion::new(1),
        },
    };
    ingest_event_in_tx(tx, &draft)
        .await
        .map_err(McpToolError::Storage)
}

pub(super) async fn insert_sidecar(
    tx: &mut Transaction<'_, Postgres>,
    memory_id: MemoryId,
    payload: &ExecutionRequestV1,
) -> Result<(), McpToolError> {
    sqlx::query(
        "INSERT INTO proxima_code.execution_request_v1
            (memory_id, repo_id, title, instructions, request_key)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(memory_id.into_inner())
    .bind(payload.repo_id)
    .bind(&payload.title)
    .bind(&payload.instructions)
    .bind(&payload.request_key)
    .execute(&mut **tx)
    .await
    .map_err(map_storage)?;
    Ok(())
}

pub(super) async fn ingest_acceptance_criteria(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    payload: &AcceptanceCriteriaV1,
) -> Result<proxima_core::verbs::event_ingest::EventIngestOutcome, McpToolError> {
    let mut payload_bytes = Vec::new();
    ciborium::ser::into_writer(payload, &mut payload_bytes)
        .map_err(|err| McpToolError::InvalidInput(err.to_string()))?;
    let content_hash = blake3::hash(&payload_bytes);
    let observed_at = time::OffsetDateTime::now_utc();
    let draft = EventDraft {
        source_id: SourceId::new(ACCEPTANCE_CRITERIA_SOURCE_ID),
        source_batch_id: SourceBatchId::new(Uuid::now_v7()),
        owner: ctx.owner.clone(),
        schema_id: SchemaId::new(AcceptanceCriteriaV1::SCHEMA_ID.into()),
        schema_version: SchemaVersion::new(AcceptanceCriteriaV1::SCHEMA_VERSION),
        payload: payload_bytes,
        observed_at,
        occurred_at: observed_at,
        cited_object: CitedObjectHint {
            schema_id: SchemaId::new(ACCEPTANCE_CRITERIA_OBJECT_SCHEMA.into()),
            schema_version: SchemaVersion::new(1),
            content_hash: *content_hash.as_bytes(),
        },
        citation_mapping: CitationMappingHint {
            schema_id: SchemaId::new(ACCEPTANCE_CRITERIA_WHOLE_SCHEMA.into()),
            schema_version: SchemaVersion::new(1),
        },
    };
    ingest_event_in_tx(tx, &draft)
        .await
        .map_err(McpToolError::Storage)
}

pub(super) async fn insert_acceptance_criteria_sidecar(
    tx: &mut Transaction<'_, Postgres>,
    memory_id: MemoryId,
    payload: &AcceptanceCriteriaV1,
) -> Result<(), McpToolError> {
    sqlx::query(
        "INSERT INTO proxima_code.acceptance_criteria_v1
            (memory_id, execution_request_memory_id, criteria_json)
         VALUES ($1, $2, $3)",
    )
    .bind(memory_id.into_inner())
    .bind(payload.execution_request_memory_id)
    .bind(
        serde_json::to_value(&payload.criteria)
            .map_err(|err| McpToolError::InvalidInput(format!("serialize criteria: {err}")))?,
    )
    .execute(&mut **tx)
    .await
    .map_err(map_storage)?;
    Ok(())
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
    append_edge_in_tx(
        tx,
        &EdgeDraft {
            edge_id,
            relation,
            source_kind: EntityKind::Fact,
            source_memory_id: Some(request_memory_id.into_inner()),
            source_goal_id: None,
            target_kind: EntityKind::Fact,
            target_memory_id: Some(criteria_memory_id.into_inner()),
            target_goal_id: None,
            authorship_kind: EdgeAuthorshipKind::ExternalAgent,
            authorship_owner_memory_id: ctx.caller_self_perspective.map(MemoryId::into_inner),
            owner: &ctx.owner,
        },
        None,
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
    append_edge_in_tx(
        tx,
        &EdgeDraft {
            edge_id,
            relation,
            source_kind: EntityKind::Perspective,
            source_memory_id: Some(planner_root.into_inner()),
            source_goal_id: None,
            target_kind: EntityKind::Fact,
            target_memory_id: Some(request_memory_id.into_inner()),
            target_goal_id: None,
            authorship_kind: EdgeAuthorshipKind::ExternalAgent,
            authorship_owner_memory_id: Some(planner_root.into_inner()),
            owner: &ctx.owner,
        },
        None,
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
    append_edge_in_tx(
        tx,
        &EdgeDraft {
            edge_id,
            relation,
            source_kind: EntityKind::Perspective,
            source_memory_id: Some(target_root.into_inner()),
            source_goal_id: None,
            target_kind: EntityKind::Fact,
            target_memory_id: Some(request_memory_id.into_inner()),
            target_goal_id: None,
            authorship_kind: EdgeAuthorshipKind::ExternalAgent,
            authorship_owner_memory_id: ctx.caller_self_perspective.map(MemoryId::into_inner),
            owner: &ctx.owner,
        },
        None,
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
    append_edge_in_tx(
        tx,
        &EdgeDraft {
            edge_id,
            relation,
            source_kind: EntityKind::Fact,
            source_memory_id: Some(request_memory_id.into_inner()),
            source_goal_id: None,
            target_kind: EntityKind::Fact,
            target_memory_id: Some(evidence_memory_id.into_inner()),
            target_goal_id: None,
            authorship_kind: EdgeAuthorshipKind::ExternalAgent,
            authorship_owner_memory_id: ctx.caller_self_perspective.map(MemoryId::into_inner),
            owner: &ctx.owner,
        },
        None,
    )
    .await
    .map_err(McpToolError::Storage)?;
    Ok(edge_id)
}

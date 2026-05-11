use std::collections::HashSet;

use proxima_core::mcp::{McpTool, McpToolCtx, McpToolError};
use proxima_core::relation::CORE_DERIVED_FROM_RELATION;
use proxima_core::verbs::event_ingest::{CitationMappingHint, CitedObjectHint, EventDraft};
use proxima_core::{
    EdgeId, FactPayload, MemoryId, SchemaId, SchemaVersion, SourceBatchId, SourceId,
};
use proxima_storage_pg::verbs::edge_append::{EdgeDraft, append_edge_in_tx};
use proxima_storage_pg::verbs::event_ingest::ingest_event_in_tx;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::payloads::{
    ExecutionRequestV1, WorkspaceDecision, WorkspaceDecisionV1, WorkspaceReviewFinding,
    WorkspaceReviewV1, WorkspaceReviewVerdict,
};

use super::emit_execution_request::{
    append_authored_edge, append_target_edge, find_execution_request_by_key,
    ingest_execution_request, insert_sidecar as insert_execution_request_sidecar,
    load_execution_request, load_prior_derived_targets, normalize_text, push_derived_edge,
    resolve_memory_id, resolve_personality_id, validate_target_execution_wake,
    validate_target_personality,
};
use super::sql::{map_storage, owner_principal};

const WORKSPACE_REVIEW_SOURCE_ID: &str = "proxima-code/workspace-review";
const WORKSPACE_REVIEW_OBJECT_SCHEMA: &str = "proxima-code/workspace-review-object-v1";
const WORKSPACE_REVIEW_WHOLE_SCHEMA: &str = "proxima-code/workspace-review-whole-v1";
pub const MAX_WORKSPACE_VETO_ROUNDS: i64 = 2;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CodeEmitWorkspaceReviewArgs {
    pub workspace_run_memory: String,
    pub verdict: WorkspaceReviewVerdict,
    pub summary: String,
    #[serde(default)]
    pub findings: Vec<WorkspaceReviewFinding>,
    #[serde(default)]
    pub correction_instructions: Option<String>,
    #[serde(default)]
    pub verification_summary: Option<String>,
    pub idempotency_key: String,
}

#[derive(Debug, Serialize)]
pub struct CodeEmitWorkspaceReviewOutput {
    pub handle: String,
    pub authored_edge_handle: Option<String>,
    pub derived_edge_handles: Vec<String>,
    pub verdict: WorkspaceReviewVerdict,
    pub round_index: u32,
    pub idempotent_replay: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CodeEmitCorrectionExecutionRequestArgs {
    #[serde(default)]
    pub workspace_review_memory: Option<String>,
    #[serde(default)]
    pub workspace_decision_memory: Option<String>,
    pub target_personality: String,
    pub request_key: String,
    pub idempotency_key: String,
}

#[derive(Debug, Serialize)]
pub struct CodeEmitCorrectionExecutionRequestOutput {
    pub handle: String,
    pub authored_edge_handle: Option<String>,
    pub target_edge_handle: Option<String>,
    pub derived_edge_handles: Vec<String>,
    pub idempotent_replay: bool,
}

enum CorrectionTrigger {
    RejectedReview(LoadedWorkspaceReview),
    RetryDecision {
        decision: LoadedWorkspaceDecision,
        execution_request_memory_id: MemoryId,
        latest_rejected_review: Option<LoadedWorkspaceReview>,
    },
}

impl CorrectionTrigger {
    fn execution_request_memory_id(&self) -> MemoryId {
        match self {
            Self::RejectedReview(review) => review.execution_request_memory_id,
            Self::RetryDecision {
                execution_request_memory_id,
                ..
            } => *execution_request_memory_id,
        }
    }

    fn rejected_review(&self) -> Option<&LoadedWorkspaceReview> {
        match self {
            Self::RejectedReview(review) => Some(review),
            Self::RetryDecision {
                latest_rejected_review,
                ..
            } => latest_rejected_review.as_ref(),
        }
    }

    fn retry_decision(&self) -> Option<&LoadedWorkspaceDecision> {
        match self {
            Self::RejectedReview(_) => None,
            Self::RetryDecision { decision, .. } => Some(decision),
        }
    }
}

#[derive(Debug)]
pub struct CodeEmitWorkspaceReviewTool;

impl McpTool for CodeEmitWorkspaceReviewTool {
    const NAME: &'static str = "proxima-code/code_emit_workspace_review";
    const DESCRIPTION: &'static str =
        "Emit a verifier-authored proxima-code/workspace-review-v1 Fact for a workspace run.";
    const PRODUCES_SCHEMA_IDS: &'static [&'static str] = &[WorkspaceReviewV1::SCHEMA_ID];

    type Args = CodeEmitWorkspaceReviewArgs;
    type Output = CodeEmitWorkspaceReviewOutput;

    fn call(
        ctx: McpToolCtx,
        args: CodeEmitWorkspaceReviewArgs,
    ) -> futures::future::BoxFuture<'static, Result<CodeEmitWorkspaceReviewOutput, McpToolError>>
    {
        Box::pin(async move {
            let verifier_root = ctx.caller_self_perspective.ok_or_else(|| {
                McpToolError::InvalidInput(
                    "caller_self_perspective is required to author a workspace review".into(),
                )
            })?;
            let workspace_run_memory_id = resolve_memory_id(&ctx, &args.workspace_run_memory)?;
            let _idempotency_key =
                normalize_text("idempotency_key", &args.idempotency_key, 1, 240)?;
            let summary = normalize_text("summary", &args.summary, 1, 4000)?;
            let correction_instructions = args
                .correction_instructions
                .as_deref()
                .map(|value| normalize_text("correction_instructions", value, 1, 12_000))
                .transpose()?;
            let verification_summary = args
                .verification_summary
                .as_deref()
                .map(|value| normalize_text("verification_summary", value, 1, 4000))
                .transpose()?;
            validate_findings(&args.findings)?;

            let mut tx = ctx.pool.begin().await.map_err(map_storage)?;
            load_workspace_run(&mut tx, &ctx, workspace_run_memory_id).await?;
            let execution_request_memory_id =
                find_execution_request_for_run(&mut tx, &ctx, workspace_run_memory_id).await?;
            load_execution_request(&mut tx, &ctx, execution_request_memory_id).await?;
            let veto_count =
                veto_count_for_request(&mut tx, &ctx, execution_request_memory_id).await?;
            let verdict = if args.verdict == WorkspaceReviewVerdict::Rejected
                && veto_count >= MAX_WORKSPACE_VETO_ROUNDS
            {
                WorkspaceReviewVerdict::NeedsUser
            } else {
                args.verdict
            };
            let round_index = u32::try_from(veto_count).unwrap_or(u32::MAX);
            let payload = WorkspaceReviewV1 {
                workspace_run_memory_id: workspace_run_memory_id.into_inner(),
                execution_request_memory_id: execution_request_memory_id.into_inner(),
                verdict,
                round_index,
                summary,
                findings: args.findings,
                correction_instructions,
                verification_summary,
                reviewed_at: time::OffsetDateTime::now_utc(),
            };
            let outcome = ingest_workspace_review(&mut tx, &ctx, &payload).await?;
            let (authored_edge_id, derived_edge_ids) = if outcome.idempotent_replay {
                (None, Vec::new())
            } else {
                insert_workspace_review_sidecar(&mut tx, outcome.memory_id, &payload).await?;
                let authored_edge_id =
                    append_authored_edge(&mut tx, &ctx, verifier_root, outcome.memory_id).await?;
                let derived_edge_ids = vec![
                    append_review_derived_edge(
                        &mut tx,
                        &ctx,
                        outcome.memory_id,
                        workspace_run_memory_id,
                    )
                    .await?,
                    append_review_derived_edge(
                        &mut tx,
                        &ctx,
                        outcome.memory_id,
                        execution_request_memory_id,
                    )
                    .await?,
                ];
                (Some(authored_edge_id), derived_edge_ids)
            };
            tx.commit().await.map_err(map_storage)?;

            Ok(CodeEmitWorkspaceReviewOutput {
                handle: ctx
                    .handles
                    .assign_memory(outcome.memory_id)
                    .as_str()
                    .to_string(),
                authored_edge_handle: authored_edge_id.map(|edge_id| {
                    ctx.handles
                        .assign_edge(EdgeId::new(edge_id))
                        .as_str()
                        .to_string()
                }),
                derived_edge_handles: derived_edge_ids
                    .into_iter()
                    .map(|edge_id| {
                        ctx.handles
                            .assign_edge(EdgeId::new(edge_id))
                            .as_str()
                            .to_string()
                    })
                    .collect(),
                verdict,
                round_index,
                idempotent_replay: outcome.idempotent_replay,
            })
        })
    }
}

#[derive(Debug)]
pub struct CodeEmitCorrectionExecutionRequestTool;

impl McpTool for CodeEmitCorrectionExecutionRequestTool {
    const NAME: &'static str = "proxima-code/code_emit_correction_execution_request";
    const DESCRIPTION: &'static str = "Emit a correction proxima-code/execution-request-v1 Fact from a rejected workspace review or retry-request workspace decision.";
    const PRODUCES_SCHEMA_IDS: &'static [&'static str] = &[ExecutionRequestV1::SCHEMA_ID];

    type Args = CodeEmitCorrectionExecutionRequestArgs;
    type Output = CodeEmitCorrectionExecutionRequestOutput;

    fn call(
        ctx: McpToolCtx,
        args: CodeEmitCorrectionExecutionRequestArgs,
    ) -> futures::future::BoxFuture<
        'static,
        Result<CodeEmitCorrectionExecutionRequestOutput, McpToolError>,
    > {
        Box::pin(async move {
            let correction_author_root = ctx.caller_self_perspective.ok_or_else(|| {
                McpToolError::InvalidInput(
                    "caller_self_perspective is required to author a correction request".into(),
                )
            })?;
            let workspace_review_memory_id = args
                .workspace_review_memory
                .as_deref()
                .map(|value| resolve_memory_id(&ctx, value))
                .transpose()?;
            let workspace_decision_memory_id = args
                .workspace_decision_memory
                .as_deref()
                .map(|value| resolve_memory_id(&ctx, value))
                .transpose()?;
            if workspace_review_memory_id.is_some() == workspace_decision_memory_id.is_some() {
                return Err(McpToolError::InvalidInput(
                    "provide exactly one of workspace_review_memory or workspace_decision_memory"
                        .into(),
                ));
            }
            let target_personality_id = resolve_personality_id(&ctx, &args.target_personality)?;
            let request_key = normalize_text("request_key", &args.request_key, 1, 240)?;
            let idempotency_key = normalize_text("idempotency_key", &args.idempotency_key, 1, 240)?;
            if request_key != idempotency_key {
                return Err(McpToolError::InvalidInput(
                    "request_key must match idempotency_key".into(),
                ));
            }

            let mut tx = ctx.pool.begin().await.map_err(map_storage)?;
            let trigger = if let Some(memory_id) = workspace_review_memory_id {
                let review = load_workspace_review(&mut tx, &ctx, memory_id).await?;
                if review.payload.verdict != WorkspaceReviewVerdict::Rejected {
                    return Err(McpToolError::InvalidInput(
                        "workspace_review_memory must point at a rejected workspace review".into(),
                    ));
                }
                CorrectionTrigger::RejectedReview(review)
            } else if let Some(memory_id) = workspace_decision_memory_id {
                let decision = load_workspace_decision(&mut tx, &ctx, memory_id).await?;
                if decision.payload.decision != WorkspaceDecision::RetryRequested {
                    return Err(McpToolError::InvalidInput(
                        "workspace_decision_memory must point at a retry-request workspace decision"
                            .into(),
                    ));
                }
                let workspace_run_memory_id =
                    MemoryId::new(decision.payload.workspace_run_memory_id);
                load_workspace_run(&mut tx, &ctx, workspace_run_memory_id).await?;
                let execution_request_memory_id =
                    find_execution_request_for_run(&mut tx, &ctx, workspace_run_memory_id).await?;
                let latest_rejected_review =
                    load_latest_rejected_review_for_run(&mut tx, &ctx, workspace_run_memory_id)
                        .await?;
                CorrectionTrigger::RetryDecision {
                    decision,
                    execution_request_memory_id,
                    latest_rejected_review,
                }
            } else {
                return Err(McpToolError::InvalidInput(
                    "provide exactly one of workspace_review_memory or workspace_decision_memory"
                        .into(),
                ));
            };
            let execution_request_memory_id = trigger.execution_request_memory_id();
            let prior = load_execution_request(&mut tx, &ctx, execution_request_memory_id).await?;
            if let Some(existing) =
                find_execution_request_by_key(&mut tx, &ctx, prior.repo_id, &request_key).await?
            {
                tx.commit().await.map_err(map_storage)?;
                return Ok(CodeEmitCorrectionExecutionRequestOutput {
                    handle: ctx.handles.assign_memory(existing).as_str().to_string(),
                    authored_edge_handle: None,
                    target_edge_handle: None,
                    derived_edge_handles: Vec::new(),
                    idempotent_replay: true,
                });
            }
            let veto_count =
                veto_count_for_request(&mut tx, &ctx, execution_request_memory_id).await?;
            if veto_count >= MAX_WORKSPACE_VETO_ROUNDS {
                return Err(McpToolError::InvalidInput(
                    "workspace review veto cap reached; user escalation required".into(),
                ));
            }
            let target_root =
                validate_target_personality(&mut tx, &ctx, target_personality_id).await?;
            validate_target_execution_wake(&mut tx, &ctx, target_personality_id).await?;

            let payload = ExecutionRequestV1 {
                repo_id: prior.repo_id,
                title: correction_title(&prior.title)?,
                instructions: correction_instructions(
                    &prior.instructions,
                    trigger.rejected_review(),
                    trigger.retry_decision(),
                    &request_key,
                )?,
                request_key,
            };
            let outcome = ingest_execution_request(&mut tx, &ctx, &payload).await?;
            let (authored_edge_id, target_edge_id, derived_edge_ids) = if outcome.idempotent_replay
            {
                (None, None, Vec::new())
            } else {
                insert_execution_request_sidecar(&mut tx, outcome.memory_id, &payload).await?;
                let authored_edge_id =
                    append_authored_edge(&mut tx, &ctx, correction_author_root, outcome.memory_id)
                        .await?;
                let target_edge_id =
                    append_target_edge(&mut tx, &ctx, target_root, outcome.memory_id).await?;
                let mut derived_edge_ids = Vec::new();
                let mut seen = HashSet::new();
                if let Some(review) = trigger.rejected_review() {
                    push_derived_edge(
                        &mut tx,
                        &ctx,
                        outcome.memory_id,
                        review.memory_id,
                        &mut seen,
                        &mut derived_edge_ids,
                    )
                    .await?;
                }
                if let Some(decision) = trigger.retry_decision() {
                    push_derived_edge(
                        &mut tx,
                        &ctx,
                        outcome.memory_id,
                        decision.memory_id,
                        &mut seen,
                        &mut derived_edge_ids,
                    )
                    .await?;
                }
                push_derived_edge(
                    &mut tx,
                    &ctx,
                    outcome.memory_id,
                    execution_request_memory_id,
                    &mut seen,
                    &mut derived_edge_ids,
                )
                .await?;
                for memory_id in
                    load_prior_derived_targets(&mut tx, &ctx, execution_request_memory_id).await?
                {
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

            Ok(CodeEmitCorrectionExecutionRequestOutput {
                handle: ctx
                    .handles
                    .assign_memory(outcome.memory_id)
                    .as_str()
                    .to_string(),
                authored_edge_handle: authored_edge_id.map(|edge_id| {
                    ctx.handles
                        .assign_edge(EdgeId::new(edge_id))
                        .as_str()
                        .to_string()
                }),
                target_edge_handle: target_edge_id.map(|edge_id| {
                    ctx.handles
                        .assign_edge(EdgeId::new(edge_id))
                        .as_str()
                        .to_string()
                }),
                derived_edge_handles: derived_edge_ids
                    .into_iter()
                    .map(|edge_id| {
                        ctx.handles
                            .assign_edge(EdgeId::new(edge_id))
                            .as_str()
                            .to_string()
                    })
                    .collect(),
                idempotent_replay: outcome.idempotent_replay,
            })
        })
    }
}

fn validate_findings(findings: &[WorkspaceReviewFinding]) -> Result<(), McpToolError> {
    for finding in findings {
        normalize_text("finding.severity", &finding.severity, 1, 80)?;
        normalize_text("finding.message", &finding.message, 1, 2000)?;
        if let Some(path) = &finding.file_path {
            normalize_text("finding.file_path", path, 1, 500)?;
        }
    }
    Ok(())
}

fn correction_title(title: &str) -> Result<String, McpToolError> {
    let prefixed = format!("Correct: {}", title.trim());
    let mut output = String::new();
    for ch in prefixed.chars().take(240) {
        output.push(ch);
    }
    normalize_text("title", &output, 1, 240)
}

fn correction_instructions(
    prior_instructions: &str,
    review: Option<&LoadedWorkspaceReview>,
    decision: Option<&LoadedWorkspaceDecision>,
    request_key: &str,
) -> Result<String, McpToolError> {
    let findings = if let Some(review) = review {
        if review.payload.findings.is_empty() {
            "none".to_string()
        } else {
            review
                .payload
                .findings
                .iter()
                .map(|finding| {
                    let location = match (&finding.file_path, finding.line) {
                        (Some(path), Some(line)) => format!("{path}:{line}"),
                        (Some(path), None) => path.clone(),
                        _ => "general".to_string(),
                    };
                    format!("- [{}] {}: {}", finding.severity, location, finding.message)
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
    } else {
        "none".to_string()
    };
    let review_memory = review
        .map(|review| review.memory_id.into_inner().to_string())
        .unwrap_or_else(|| "none".into());
    let workspace_run = review
        .map(|review| review.payload.workspace_run_memory_id.to_string())
        .or_else(|| decision.map(|decision| decision.payload.workspace_run_memory_id.to_string()))
        .unwrap_or_else(|| "unknown".into());
    let review_summary = review
        .map(|review| review.payload.summary.as_str())
        .unwrap_or("none");
    let correction_notes = review
        .and_then(|review| review.payload.correction_instructions.as_deref())
        .or_else(|| decision.and_then(|decision| decision.payload.reason_text.as_deref()))
        .unwrap_or("none");
    let retry_decision = decision
        .map(|decision| decision.memory_id.into_inner().to_string())
        .unwrap_or_else(|| "none".into());
    let retry_reason = decision
        .and_then(|decision| decision.payload.reason_text.as_deref())
        .unwrap_or("none");
    let instructions = format!(
        "{}\n\nCorrection context:\nworkspace_review: {}\nworkspace_decision: {}\nworkspace_run: {}\nretry_key: {}\nreview_summary: {}\nretry_reason: {}\ncorrection_instructions: {}\nfindings:\n{}",
        prior_instructions.trim(),
        review_memory,
        retry_decision,
        workspace_run,
        request_key,
        review_summary,
        retry_reason,
        correction_notes,
        findings,
    );
    normalize_text("instructions", &instructions, 1, 20_000)
}

async fn load_workspace_run(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    memory_id: MemoryId,
) -> Result<(), McpToolError> {
    let (owner_kind, owner_principal_id) = owner_principal(&ctx.owner);
    let row: Option<(String, String, Option<Uuid>)> = sqlx::query_as(
        "SELECT COALESCE(m.kind, 'Fact') AS kind, m.schema_id, r.memory_id
         FROM proxima_core.memories m
         LEFT JOIN proxima_code.workspace_run_v1 r USING (memory_id)
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
    let Some((kind, schema_id, sidecar_memory_id)) = row else {
        return Err(McpToolError::InvalidInput(format!(
            "workspace_run_memory is not visible: {}",
            memory_id.into_inner()
        )));
    };
    if kind != "Fact" || schema_id != "proxima-code/workspace-run-v1" {
        return Err(McpToolError::InvalidInput(
            "workspace_run_memory must be a proxima-code/workspace-run-v1 Fact".into(),
        ));
    }
    if sidecar_memory_id.is_none() {
        return Err(McpToolError::InvalidInput(
            "workspace_run_memory sidecar row is missing".into(),
        ));
    }
    Ok(())
}

#[derive(Debug)]
struct LoadedWorkspaceReview {
    memory_id: MemoryId,
    execution_request_memory_id: MemoryId,
    payload: WorkspaceReviewV1,
}

async fn load_workspace_review(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    memory_id: MemoryId,
) -> Result<LoadedWorkspaceReview, McpToolError> {
    let (owner_kind, owner_principal_id) = owner_principal(&ctx.owner);
    let row: Option<(
        String,
        String,
        Option<Uuid>,
        Option<Uuid>,
        Option<String>,
        Option<i32>,
        Option<String>,
        Option<serde_json::Value>,
        Option<String>,
        Option<String>,
        Option<time::OffsetDateTime>,
    )> = sqlx::query_as(
        "SELECT COALESCE(m.kind, 'Fact') AS kind,
                m.schema_id,
                r.workspace_run_memory_id,
                r.execution_request_memory_id,
                r.verdict,
                r.round_index,
                r.summary,
                r.findings_json,
                r.correction_instructions,
                r.verification_summary,
                r.reviewed_at
         FROM proxima_core.memories m
         LEFT JOIN proxima_code.workspace_review_v1 r USING (memory_id)
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
    let Some((
        kind,
        schema_id,
        workspace_run_memory_id,
        execution_request_memory_id,
        verdict,
        round_index,
        summary,
        findings_json,
        correction_instructions,
        verification_summary,
        reviewed_at,
    )) = row
    else {
        return Err(McpToolError::InvalidInput(format!(
            "workspace_review_memory is not visible: {}",
            memory_id.into_inner()
        )));
    };
    if kind != "Fact" || schema_id != WorkspaceReviewV1::SCHEMA_ID {
        return Err(McpToolError::InvalidInput(
            "workspace_review_memory must be a proxima-code/workspace-review-v1 Fact".into(),
        ));
    }
    let (
        Some(workspace_run_memory_id),
        Some(execution_request_memory_id),
        Some(verdict),
        Some(round_index),
        Some(summary),
        Some(findings_json),
        Some(reviewed_at),
    ) = (
        workspace_run_memory_id,
        execution_request_memory_id,
        verdict,
        round_index,
        summary,
        findings_json,
        reviewed_at,
    )
    else {
        return Err(McpToolError::InvalidInput(
            "workspace_review_memory sidecar row is missing".into(),
        ));
    };
    let verdict = match verdict.as_str() {
        "approved" => WorkspaceReviewVerdict::Approved,
        "rejected" => WorkspaceReviewVerdict::Rejected,
        "needs_user" => WorkspaceReviewVerdict::NeedsUser,
        other => {
            return Err(McpToolError::InvalidInput(format!(
                "workspace_review_memory has invalid verdict: {other}"
            )));
        }
    };
    let findings = serde_json::from_value(findings_json)
        .map_err(|err| McpToolError::InvalidInput(format!("findings_json: {err}")))?;
    let round_index = u32::try_from(round_index).map_err(|_| {
        McpToolError::InvalidInput("workspace_review_memory has invalid round_index".into())
    })?;
    Ok(LoadedWorkspaceReview {
        memory_id,
        execution_request_memory_id: MemoryId::new(execution_request_memory_id),
        payload: WorkspaceReviewV1 {
            workspace_run_memory_id,
            execution_request_memory_id,
            verdict,
            round_index,
            summary,
            findings,
            correction_instructions,
            verification_summary,
            reviewed_at,
        },
    })
}

#[derive(Debug)]
struct LoadedWorkspaceDecision {
    memory_id: MemoryId,
    payload: WorkspaceDecisionV1,
}

async fn load_workspace_decision(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    memory_id: MemoryId,
) -> Result<LoadedWorkspaceDecision, McpToolError> {
    let (owner_kind, owner_principal_id) = owner_principal(&ctx.owner);
    let row: Option<(
        String,
        String,
        Option<Uuid>,
        Option<String>,
        Option<time::OffsetDateTime>,
        Option<String>,
        Option<Uuid>,
    )> = sqlx::query_as(
        "SELECT COALESCE(m.kind, 'Fact') AS kind,
                m.schema_id,
                d.workspace_run_memory_id,
                d.decision,
                d.decided_at,
                d.reason_text,
                d.decided_by_owner_id
         FROM proxima_core.memories m
         LEFT JOIN proxima_code.workspace_decision_v1 d USING (memory_id)
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
    let Some((
        kind,
        schema_id,
        workspace_run_memory_id,
        decision,
        decided_at,
        reason_text,
        decided_by_owner_id,
    )) = row
    else {
        return Err(McpToolError::InvalidInput(format!(
            "workspace_decision_memory is not visible: {}",
            memory_id.into_inner()
        )));
    };
    if kind != "Fact" || schema_id != WorkspaceDecisionV1::SCHEMA_ID {
        return Err(McpToolError::InvalidInput(
            "workspace_decision_memory must be a proxima-code/workspace-decision-v1 Fact".into(),
        ));
    }
    let (
        Some(workspace_run_memory_id),
        Some(decision),
        Some(decided_at),
        Some(decided_by_owner_id),
    ) = (
        workspace_run_memory_id,
        decision,
        decided_at,
        decided_by_owner_id,
    )
    else {
        return Err(McpToolError::InvalidInput(
            "workspace_decision_memory sidecar row is missing".into(),
        ));
    };
    let decision = match decision.as_str() {
        "rejected" => WorkspaceDecision::Rejected,
        "retry_requested" => WorkspaceDecision::RetryRequested,
        "accepted" => WorkspaceDecision::Accepted,
        "merged" => WorkspaceDecision::Merged,
        other => {
            return Err(McpToolError::InvalidInput(format!(
                "workspace_decision_memory has invalid decision: {other}"
            )));
        }
    };
    Ok(LoadedWorkspaceDecision {
        memory_id,
        payload: WorkspaceDecisionV1 {
            workspace_run_memory_id,
            decision,
            decided_at,
            reason_text,
            decided_by_owner_id,
        },
    })
}

async fn load_latest_rejected_review_for_run(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    workspace_run_memory_id: MemoryId,
) -> Result<Option<LoadedWorkspaceReview>, McpToolError> {
    let (owner_kind, owner_principal_id) = owner_principal(&ctx.owner);
    let memory_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT r.memory_id
         FROM proxima_code.workspace_review_v1 r
         JOIN proxima_core.memories m USING (memory_id)
         WHERE m.owner_principal_kind = $1
           AND m.owner_principal_id = $2
           AND r.workspace_run_memory_id = $3
           AND r.verdict = 'rejected'
         ORDER BY r.created_at DESC, r.memory_id DESC
         LIMIT 1",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(workspace_run_memory_id.into_inner())
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_storage)?;
    match memory_id {
        Some(memory_id) => load_workspace_review(tx, ctx, MemoryId::new(memory_id))
            .await
            .map(Some),
        None => Ok(None),
    }
}

async fn find_execution_request_for_run(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    workspace_run_memory_id: MemoryId,
) -> Result<MemoryId, McpToolError> {
    let (owner_kind, owner_principal_id) = owner_principal(&ctx.owner);
    let request: Option<Uuid> = sqlx::query_scalar(
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
           AND e.target_kind = 'Fact'
           AND m.schema_id = $5
         ORDER BY e.created_at, e.edge_id
         LIMIT 1",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(CORE_DERIVED_FROM_RELATION)
    .bind(workspace_run_memory_id.into_inner())
    .bind(ExecutionRequestV1::SCHEMA_ID)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_storage)?;
    request.map(MemoryId::new).ok_or_else(|| {
        McpToolError::InvalidInput(
            "workspace_run_memory has no derived-from execution request".into(),
        )
    })
}

async fn veto_count_for_request(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    execution_request_memory_id: MemoryId,
) -> Result<i64, McpToolError> {
    let (owner_kind, owner_principal_id) = owner_principal(&ctx.owner);
    let count: i64 = sqlx::query_scalar(
        "WITH review_vetoes AS (
             SELECT r.memory_id
             FROM proxima_code.workspace_review_v1 r
             JOIN proxima_core.memories m USING (memory_id)
             WHERE m.owner_principal_kind = $1
               AND m.owner_principal_id = $2
               AND r.execution_request_memory_id = $3
               AND r.verdict = 'rejected'
         ),
         decision_vetoes AS (
             SELECT d.memory_id
             FROM proxima_code.workspace_decision_v1 d
             JOIN proxima_core.memories dm USING (memory_id)
             JOIN proxima_core.edges e
               ON e.source_kind = 'Fact'
              AND e.source_memory_id = d.workspace_run_memory_id
              AND e.target_kind = 'Fact'
              AND e.target_memory_id = $3
              AND e.relation = $4
              AND e.owner_principal_kind = dm.owner_principal_kind
              AND e.owner_principal_id = dm.owner_principal_id
             WHERE dm.owner_principal_kind = $1
               AND dm.owner_principal_id = $2
               AND d.decision = 'retry_requested'
         )
         SELECT (SELECT count(*) FROM review_vetoes)
              + (SELECT count(*) FROM decision_vetoes)",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(execution_request_memory_id.into_inner())
    .bind(CORE_DERIVED_FROM_RELATION)
    .fetch_one(&mut **tx)
    .await
    .map_err(map_storage)?;
    Ok(count)
}

async fn ingest_workspace_review(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    payload: &WorkspaceReviewV1,
) -> Result<proxima_core::verbs::event_ingest::EventIngestOutcome, McpToolError> {
    let mut payload_bytes = Vec::new();
    ciborium::ser::into_writer(payload, &mut payload_bytes)
        .map_err(|err| McpToolError::InvalidInput(err.to_string()))?;
    let content_hash = blake3::hash(&payload_bytes);
    let observed_at = time::OffsetDateTime::now_utc();
    let draft = EventDraft {
        source_id: SourceId::new(WORKSPACE_REVIEW_SOURCE_ID),
        source_batch_id: SourceBatchId::new(Uuid::now_v7()),
        owner: ctx.owner.clone(),
        schema_id: SchemaId::new(WorkspaceReviewV1::SCHEMA_ID.into()),
        schema_version: SchemaVersion::new(WorkspaceReviewV1::SCHEMA_VERSION),
        payload: payload_bytes,
        observed_at,
        occurred_at: payload.reviewed_at,
        cited_object: CitedObjectHint {
            schema_id: SchemaId::new(WORKSPACE_REVIEW_OBJECT_SCHEMA.into()),
            schema_version: SchemaVersion::new(1),
            content_hash: *content_hash.as_bytes(),
        },
        citation_mapping: CitationMappingHint {
            schema_id: SchemaId::new(WORKSPACE_REVIEW_WHOLE_SCHEMA.into()),
            schema_version: SchemaVersion::new(1),
        },
    };
    ingest_event_in_tx(tx, &draft)
        .await
        .map_err(McpToolError::Storage)
}

async fn insert_workspace_review_sidecar(
    tx: &mut Transaction<'_, Postgres>,
    memory_id: MemoryId,
    payload: &WorkspaceReviewV1,
) -> Result<(), McpToolError> {
    sqlx::query(
        "INSERT INTO proxima_code.workspace_review_v1
            (memory_id, workspace_run_memory_id, execution_request_memory_id,
             verdict, round_index, summary, findings_json,
             correction_instructions, verification_summary, reviewed_at)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
    )
    .bind(memory_id.into_inner())
    .bind(payload.workspace_run_memory_id)
    .bind(payload.execution_request_memory_id)
    .bind(payload.verdict.as_str())
    .bind(i32::try_from(payload.round_index).unwrap_or(i32::MAX))
    .bind(&payload.summary)
    .bind(
        serde_json::to_value(&payload.findings)
            .map_err(|err| McpToolError::InvalidInput(format!("serialize findings: {err}")))?,
    )
    .bind(payload.correction_instructions.as_deref())
    .bind(payload.verification_summary.as_deref())
    .bind(payload.reviewed_at)
    .execute(&mut **tx)
    .await
    .map_err(map_storage)?;
    Ok(())
}

async fn append_review_derived_edge(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    review_memory_id: MemoryId,
    target_memory_id: MemoryId,
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
            source_kind: "Fact",
            source_memory_id: Some(review_memory_id.into_inner()),
            source_goal_id: None,
            target_kind: "Fact",
            target_memory_id: Some(target_memory_id.into_inner()),
            target_goal_id: None,
            authorship_kind: "ExternalAgent",
            authorship_owner_memory_id: ctx.caller_self_perspective.map(MemoryId::into_inner),
            owner: &ctx.owner,
        },
        None,
    )
    .await
    .map_err(McpToolError::Storage)?;
    Ok(edge_id)
}

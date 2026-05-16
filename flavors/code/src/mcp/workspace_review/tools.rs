// Workspace review MCP tool implementations
use std::collections::HashSet;

use futures::future::BoxFuture;
use proxima_core::mcp::{McpTool, McpToolCtx, McpToolError};
use proxima_core::{EdgeId, MemoryId};

use super::MAX_WORKSPACE_VETO_ROUNDS;
use super::helpers::{correction_instructions, correction_title, validate_findings};
use super::ingest::{
    append_review_derived_edge, append_review_reviews_edge, ingest_workspace_review,
    insert_workspace_review_sidecar,
};
use super::loaders::{
    find_execution_request_for_run, load_latest_rejected_review_for_run, load_workspace_decision,
    load_workspace_review, load_workspace_run, veto_count_for_request,
};
use super::types::{
    CodeEmitCorrectionExecutionRequestArgs, CodeEmitCorrectionExecutionRequestOutput,
    CodeEmitWorkspaceReviewArgs, CodeEmitWorkspaceReviewOutput, CorrectionTrigger,
};

use crate::mcp::emit_execution_request::{
    append_authored_edge, append_target_edge, find_execution_request_by_key,
    ingest_execution_request, insert_sidecar as insert_execution_request_sidecar,
    load_execution_request, load_prior_derived_targets, push_derived_edge, resolve_memory_id,
    resolve_personality_id, validate_target_execution_wake, validate_target_personality,
};
use crate::mcp::sql::map_storage;
use crate::payloads::{
    ExecutionRequestV1, WorkspaceDecision, WorkspaceReviewV1, WorkspaceReviewVerdict,
};
use proxima_core::FactPayload;

/// MCP tool for emitting workspace reviews
#[derive(Debug)]
pub struct CodeEmitWorkspaceReviewTool;

impl McpTool for CodeEmitWorkspaceReviewTool {
    const NAME: &'static str = "proxima-code/code_emit_workspace_review";
    const DESCRIPTION: &'static str =
        "Emit a verifier-authored proxima-code/workspace-review-v1 Fact for a workspace run.";
    const PRODUCES_SCHEMA_IDS: &'static [&'static str] = &[WorkspaceReviewV1::SCHEMA_ID];

    type Args = CodeEmitWorkspaceReviewArgs;
    type Output = CodeEmitWorkspaceReviewOutput;

    #[allow(clippy::too_many_lines)]
    fn call(
        ctx: McpToolCtx,
        args: CodeEmitWorkspaceReviewArgs,
    ) -> BoxFuture<'static, Result<CodeEmitWorkspaceReviewOutput, McpToolError>> {
        Box::pin(async move {
            let verifier_root = ctx.caller_self_perspective.ok_or_else(|| {
                McpToolError::InvalidInput(
                    "caller_self_perspective is required to author a workspace review".into(),
                )
            })?;
            let workspace_run_memory_id = resolve_memory_id(&ctx, &args.workspace_run_memory)?;
            let _idempotency_key = crate::mcp::emit_execution_request::normalize_text(
                "idempotency_key",
                &args.idempotency_key,
                1,
                240,
            )?;
            let summary = crate::mcp::emit_execution_request::normalize_text(
                "summary",
                &args.summary,
                1,
                4000,
            )?;
            let correction_instructions = args
                .correction_instructions
                .as_deref()
                .map(|value| {
                    crate::mcp::emit_execution_request::normalize_text(
                        "correction_instructions",
                        value,
                        1,
                        12_000,
                    )
                })
                .transpose()?;
            let verification_summary = args
                .verification_summary
                .as_deref()
                .map(|value| {
                    crate::mcp::emit_execution_request::normalize_text(
                        "verification_summary",
                        value,
                        1,
                        4000,
                    )
                })
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
                    append_review_reviews_edge(
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
                handle: ctx.format_memory(outcome.memory_id),
                authored_edge_handle: authored_edge_id
                    .map(|edge_id| ctx.format_edge(EdgeId::new(edge_id))),
                derived_edge_handles: derived_edge_ids
                    .into_iter()
                    .map(|edge_id| ctx.format_edge(EdgeId::new(edge_id)))
                    .collect(),
                verdict,
                round_index,
                idempotent_replay: outcome.idempotent_replay,
            })
        })
    }
}

/// MCP tool for emitting correction execution requests
#[derive(Debug)]
pub struct CodeEmitCorrectionExecutionRequestTool;

impl McpTool for CodeEmitCorrectionExecutionRequestTool {
    const NAME: &'static str = "proxima-code/code_emit_correction_execution_request";
    const DESCRIPTION: &'static str = "Emit a correction proxima-code/execution-request-v1 Fact from a rejected workspace review or retry-request workspace decision.";
    const PRODUCES_SCHEMA_IDS: &'static [&'static str] = &[ExecutionRequestV1::SCHEMA_ID];

    type Args = CodeEmitCorrectionExecutionRequestArgs;
    type Output = CodeEmitCorrectionExecutionRequestOutput;

    #[allow(clippy::too_many_lines)]
    fn call(
        ctx: McpToolCtx,
        args: CodeEmitCorrectionExecutionRequestArgs,
    ) -> BoxFuture<'static, Result<CodeEmitCorrectionExecutionRequestOutput, McpToolError>> {
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
            let request_key = crate::mcp::emit_execution_request::normalize_text(
                "idempotency_key",
                &args.idempotency_key,
                1,
                240,
            )?;

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
                    handle: ctx.format_memory(existing),
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
                    trigger.workspace_run_memory_id(),
                    &mut seen,
                    &mut derived_edge_ids,
                )
                .await?;
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

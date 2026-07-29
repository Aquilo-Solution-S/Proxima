use proxima_core::{EdgeId, FactPayload, Tool, ToolCtx, ToolError};

use crate::payloads::{AcceptanceCriteriaV1, ExecutionRequestV1};

use super::super::sql::{map_storage, resolve_repo_identifier};
use super::super::{CodeToolCtxExt, code_store};
use super::context_validation::{
    validate_active_goal_context, validate_evidence_in_owner, validate_goal_activated_fact,
    validate_repo,
};
use super::edges::{append_acceptance_criteria_edge, append_authored_edge, append_derived_edge};
use super::ingest::{ingest_acceptance_criteria, ingest_execution_request};
use super::input_validation::{normalize_text, resolve_evidence, validate_acceptance_criteria};
use super::types::{CodeEmitExecutionRequestArgs, CodeEmitExecutionRequestOutput};

#[derive(Debug)]
pub struct CodeEmitExecutionRequestTool;

impl Tool for CodeEmitExecutionRequestTool {
    const NAME: &'static str = "proxima-code_emit_execution_request";
    const DESCRIPTION: &'static str =
        "Emit a repo-scoped proxima-code/work-requested-v1 Fact for an Active Goal.";
    const ANNOTATIONS: Option<proxima_core::mcp::McpToolAnnotations> =
        Some(crate::mcp::WRITE_NON_IDEMPOTENT);
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

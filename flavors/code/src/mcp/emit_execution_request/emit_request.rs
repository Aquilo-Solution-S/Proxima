use proxima_core::{EdgeEndpoint, EntityKind, FactPayload, Tool, ToolCtx, ToolError};

use crate::payloads::{AcceptanceCriteriaV1, ExecutionRequestV1};

use super::super::sql::resolve_repo_identifier;
use super::super::{CodeToolCtxExt, engine};
use super::context_validation::{
    validate_active_goal_context, validate_evidence_in_owner, validate_goal_activated_fact,
    validate_repo,
};
use super::ingest::{FactProvenance, ingest_acceptance_criteria, ingest_execution_request};
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

            let title = normalize_text("title", &args.title, 240)?;
            let instructions = normalize_text("instructions", &args.instructions, 20_000)?;
            let request_key = normalize_text("idempotency_key", &args.idempotency_key, 240)?;
            let acceptance_criteria = validate_acceptance_criteria(args.acceptance_criteria)?;

            let planner_root = ctx.caller_self_perspective().ok_or_else(|| {
                ToolError::InvalidInput(
                    "caller_self_perspective is required to author an execution request".into(),
                )
            })?;
            let goal_activated_memory_id = ctx.resolve_fact_memory(&args.goal_activated_memory)?;
            let evidence = resolve_evidence(&ctx, &args.evidence)?;

            let goal_id = validate_goal_activated_fact(&ctx, goal_activated_memory_id).await?;
            validate_active_goal_context(&ctx, goal_id, planner_root).await?;
            validate_evidence_in_owner(&ctx, &evidence).await?;

            // What the request was made from — the activation Fact and the
            // evidence — travels with the write instead of following it as
            // separate edge appends, so a replayed emit re-asserts the same
            // rows rather than minting new ones.
            let mut origins = Vec::with_capacity(1 + evidence.len());
            origins.push(EdgeEndpoint::memory(
                EntityKind::Fact,
                goal_activated_memory_id,
            ));
            origins.extend(
                evidence
                    .iter()
                    .map(|memory_id| EdgeEndpoint::memory(EntityKind::Fact, *memory_id)),
            );
            let provenance = FactProvenance {
                derived_from: &origins,
            };

            let payload = ExecutionRequestV1 {
                repo_id,
                title,
                instructions,
                request_key,
                depends_on_memory_ids: Vec::new(),
            };
            let engine = engine(&ctx)?;
            let mut uow = engine
                .unit_of_work(ctx.authz())
                .await
                .map_err(ToolError::Protocol)?;
            let outcome = ingest_execution_request(&mut uow, &payload, provenance).await?;
            let acceptance_memory_id =
                if outcome.idempotent_replay || acceptance_criteria.is_empty() {
                    None
                } else {
                    // The criteria Fact is the node that owns "these are the
                    // criteria for that request": `work_item_memory_id` is a
                    // schema-declared reference field, so the index row falls
                    // out of this ingest and nobody writes an edge.
                    let criteria_payload = AcceptanceCriteriaV1 {
                        work_item_memory_id: outcome.memory_id.into_inner(),
                        criteria: acceptance_criteria,
                    };
                    let criteria_outcome = ingest_acceptance_criteria(
                        &mut uow,
                        &criteria_payload,
                        FactProvenance { derived_from: &[] },
                    )
                    .await?;
                    Some(criteria_outcome.memory_id)
                };
            uow.commit().await.map_err(ToolError::Protocol)?;

            Ok(CodeEmitExecutionRequestOutput {
                handle: ctx.format_fact_memory(outcome.memory_id),
                origin_count: if outcome.idempotent_replay {
                    0
                } else {
                    origins.len()
                },
                acceptance_criteria_handle: acceptance_memory_id
                    .map(|id| ctx.format_fact_memory(id)),
                idempotent_replay: outcome.idempotent_replay,
            })
        })
    }
}

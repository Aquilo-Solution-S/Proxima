use std::collections::HashSet;

use proxima_core::{AccessKind, EdgeId, FactPayload, Tool, ToolCtx, ToolError};

use crate::payloads::ExecutionRequestV1;

use super::super::sql::map_storage;
use super::super::{CodeToolCtxExt, code_store};
use super::context_validation::validate_evidence_in_owner;
use super::edges::{append_authored_edge, append_target_edge};
use super::ingest::ingest_execution_request;
use super::input_validation::{normalize_text, resolve_evidence};
use super::retry_support::{
    find_execution_request_by_key, load_execution_request, load_prior_derived_targets,
    push_derived_edge, resolve_target_perspective_id, retry_instructions,
    validate_target_perspective,
};
use super::types::{CodeRetryExecutionRequestArgs, CodeRetryExecutionRequestOutput};

#[derive(Debug)]
pub struct CodeRetryExecutionRequestTool;

impl Tool for CodeRetryExecutionRequestTool {
    const NAME: &'static str = "proxima-code_retry_execution_request";
    const DESCRIPTION: &'static str = "Shell-author override: retry a prior proxima-code/work-requested-v1 Fact for a target worker.";
    const ANNOTATIONS: Option<proxima_core::mcp::McpToolAnnotations> =
        Some(crate::mcp::WRITE_NON_IDEMPOTENT);
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
            if !ctx.authz().may_write(&ctx.owner(), AccessKind::Fact) {
                return Err(ToolError::NotAuthorized(
                    "code_retry_execution_request requires Fact write authority on the bound owner"
                        .into(),
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
            let request_key = normalize_text("idempotency_key", &args.idempotency_key, 240)?;
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
                Some(value) => normalize_text("title", &value, 240)?,
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

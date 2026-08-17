use std::collections::HashSet;

use proxima_core::{
    AccessKind, AuthorDerivedRequestInput, EdgeEndpoint, EntityKind, FactPayload,
    MemoryOperatorKind, PerspectivePayload, SchemaId, SchemaVersion, SidecarPayload, Tool, ToolCtx,
    ToolError,
};

use crate::payloads::{CodeWorkAssignmentV1, ExecutionRequestV1};

use super::super::{CodeToolCtxExt, engine};
use super::context_validation::validate_evidence_in_owner;
use super::ingest::{FactProvenance, ingest_execution_request};
use super::input_validation::{normalize_text, resolve_evidence};
use super::retry_support::{
    find_execution_request_by_key, load_execution_request, load_prior_origins,
    resolve_target_perspective_id, retry_instructions, validate_target_perspective,
};
use super::types::{CodeRetryExecutionRequestArgs, CodeRetryExecutionRequestOutput};
use super::{
    work_assignment_input_contract_id, work_assignment_memory_id, work_assignment_operator_id,
};

#[derive(Debug)]
pub struct CodeRetryExecutionRequestTool;

impl Tool for CodeRetryExecutionRequestTool {
    const NAME: &'static str = "proxima-code_retry_execution_request";
    const DESCRIPTION: &'static str = "Shell-author override: retry a prior proxima-code/work-requested-v1 Fact for a target worker.";
    const ANNOTATIONS: Option<proxima_core::mcp::McpToolAnnotations> =
        Some(crate::mcp::WRITE_NON_IDEMPOTENT);
    const PRODUCES_SCHEMA_IDS: &'static [&'static str] = &[
        ExecutionRequestV1::SCHEMA_ID,
        <CodeWorkAssignmentV1 as PerspectivePayload>::SCHEMA_ID,
    ];

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
            let _shell_author_root = ctx.caller_self_perspective().ok_or_else(|| {
                ToolError::InvalidInput(
                    "caller_self_perspective is required for shell-author retry provenance".into(),
                )
            })?;
            let prior_memory_id = ctx.resolve_fact_memory(&args.prior_execution_request)?;
            let target_perspective_id =
                resolve_target_perspective_id(&ctx, &args.target_perspective)?;
            let request_key = normalize_text("idempotency_key", &args.idempotency_key, 240)?;
            let explicit_evidence = resolve_evidence(&ctx, &args.evidence)?;

            let prior = load_execution_request(&ctx, prior_memory_id).await?;
            if let Some(existing) =
                find_execution_request_by_key(&ctx, prior.repo_id, &request_key).await?
            {
                return Ok(CodeRetryExecutionRequestOutput {
                    handle: ctx.format_fact_memory(existing),
                    assignment_handle: None,
                    origin_count: 0,
                    idempotent_replay: true,
                });
            }
            validate_target_perspective(&ctx, target_perspective_id).await?;
            validate_evidence_in_owner(&ctx, &explicit_evidence).await?;

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

            // A retry is made from the request it retries, everything that
            // request was made from, and whatever extra evidence the caller
            // supplies. Declared once, on the write.
            let mut seen = HashSet::new();
            let mut origins = Vec::new();
            let mut push = |memory_id| {
                if seen.insert(memory_id) {
                    origins.push(EdgeEndpoint::memory(EntityKind::Fact, memory_id));
                }
            };
            push(prior_memory_id);
            for memory_id in load_prior_origins(&ctx, prior_memory_id).await? {
                push(memory_id);
            }
            for memory_id in explicit_evidence {
                push(memory_id);
            }

            let payload = ExecutionRequestV1 {
                repo_id: prior.repo_id,
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
            let outcome = ingest_execution_request(
                &mut uow,
                &payload,
                FactProvenance {
                    derived_from: &origins,
                },
            )
            .await?;
            uow.commit().await.map_err(ToolError::Protocol)?;

            // "This worker should pick up that request" is a claim about two
            // nodes that already exist, and neither of them owns it: a Fact
            // asserts no judgment, and the target Perspective's row is
            // append-only. So it becomes its own node, and its two
            // references are the connections (docs/16 §The Model).
            let assignment_handle = if outcome.idempotent_replay {
                None
            } else {
                Some(
                    author_assignment(
                        &ctx,
                        prior.repo_id,
                        target_perspective_id,
                        outcome.memory_id,
                    )
                    .await?,
                )
            };

            Ok(CodeRetryExecutionRequestOutput {
                handle: ctx.format_fact_memory(outcome.memory_id),
                assignment_handle,
                origin_count: if outcome.idempotent_replay {
                    0
                } else {
                    origins.len()
                },
                idempotent_replay: outcome.idempotent_replay,
            })
        })
    }
}

/// Author the assignment Perspective through the engine's derived-write
/// gate, which is what read-checks both subjects before the index rows land.
async fn author_assignment(
    ctx: &ToolCtx,
    repo_id: uuid::Uuid,
    target_perspective_memory_id: proxima_core::MemoryId,
    work_item_memory_id: proxima_core::MemoryId,
) -> Result<String, ToolError> {
    let engine = engine(ctx)?;
    let owner = ctx.owner();
    let caller = ctx
        .caller()
        .ok_or_else(|| ToolError::Other("code flavor tools require caller metadata".into()))?;
    let payload = CodeWorkAssignmentV1 {
        repo_id,
        target_perspective_memory_id: target_perspective_memory_id.into_inner(),
        work_item_memory_id: work_item_memory_id.into_inner(),
        reason: "shell-author retry assignment".to_string(),
    };
    let outcome = engine
        .author_derived_authorized(
            ctx.authz(),
            AuthorDerivedRequestInput {
                memory_id: work_assignment_memory_id(
                    &owner,
                    target_perspective_memory_id,
                    work_item_memory_id,
                ),
                owner,
                kind: EntityKind::Perspective,
                text: payload.reason.clone(),
                schema_id: SchemaId::new(
                    <CodeWorkAssignmentV1 as PerspectivePayload>::SCHEMA_ID.into(),
                ),
                schema_version: SchemaVersion::new(
                    <CodeWorkAssignmentV1 as PerspectivePayload>::SCHEMA_VERSION,
                ),
                operator_kind: MemoryOperatorKind::AtoP,
                operator_id: work_assignment_operator_id(),
                input_contract_id: work_assignment_input_contract_id(),
                source_batch_id: None,
                model_id: caller.model_id.as_str(),
                prompt_version: "proxima-code/work-assignment-v1",
                sidecar_payload: SidecarPayload::perspective(payload),
                authoring_perspective_id: ctx.caller_self_perspective(),
                // An assignment consumes nothing. It grounds through the
                // references its payload carries.
                derived_from: &[],
                supersedes: None,
                lexical_language: None,
            },
        )
        .await?;
    Ok(ctx.format_perspective_memory(outcome.memory_id))
}

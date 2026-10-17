use proxima_core::mcp::{Replay, ToolEffect};
use std::collections::HashSet;

use proxima_core::{
    AccessKind, DerivedMemory, EdgeEndpoint, EntityKind, FactPayload, MemoryId, MemoryTarget,
    PerspectivePayload, SeriesHandle, Tool, ToolCtx, ToolError,
};

use crate::payloads::{CodeWorkAssignmentV1, ExecutionRequestV1};

use super::super::engine;
use super::context_validation::validate_evidence_in_owner;
use super::ingest::{FactProvenance, ingest_execution_request};
use super::input_validation::{normalize_text, resolve_evidence};
use super::retry_support::{
    find_execution_request_by_key, load_execution_request, load_prior_origins, retry_instructions,
    validate_target_perspective,
};
use super::types::{CodeRetryExecutionRequestArgs, CodeRetryExecutionRequestOutput};
use super::work_assignment_memory_id;

#[derive(Debug)]
pub struct CodeRetryExecutionRequestTool;

impl Tool for CodeRetryExecutionRequestTool {
    const NAME: &'static str = "proxima-code_retry_execution_request";
    const DESCRIPTION: &'static str = "Re-issue a prior work request as a new one assigned to a worker Perspective, with optional new title, appended instructions and extra evidence. Needs a host-supplied caller Perspective.";
    const EFFECT: Option<ToolEffect> = Some(ToolEffect::Additive(Replay::NonIdempotent));
    const PRODUCES_SCHEMA_IDS: &'static [&'static str] = &[
        ExecutionRequestV1::SCHEMA_ID,
        <CodeWorkAssignmentV1 as PerspectivePayload>::SCHEMA_ID,
    ];

    type Args = CodeRetryExecutionRequestArgs;
    type Output = CodeRetryExecutionRequestOutput;

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
            let prior_memory_id = crate::mcp::parse_memory_uuid(&args.prior_execution_request)?;
            let target_perspective_id = crate::mcp::parse_memory_uuid(&args.target_perspective)?;
            let request_key = normalize_text("idempotency_key", &args.idempotency_key, 240)?;
            let explicit_evidence = resolve_evidence(&args.evidence)?;

            let prior = load_execution_request(&ctx, prior_memory_id).await?;
            if let Some(existing) =
                find_execution_request_by_key(&ctx, prior.repo_id, &request_key).await?
            {
                return Ok(CodeRetryExecutionRequestOutput {
                    handle: existing.into_inner().to_string(),
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

            let origins = retry_origins(&ctx, prior_memory_id, explicit_evidence).await?;

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
                ctx.owner(),
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
                handle: outcome.memory_id.into_inner().to_string(),
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

/// A retry retains its prior request and that request's origins, then adds new evidence.
async fn retry_origins(
    ctx: &ToolCtx,
    prior_memory_id: MemoryId,
    evidence: Vec<MemoryId>,
) -> Result<Vec<EdgeEndpoint>, ToolError> {
    let prior_origins = load_prior_origins(ctx, prior_memory_id).await?;
    let mut seen = HashSet::new();
    let mut origins = Vec::new();
    for id in std::iter::once(prior_memory_id)
        .chain(prior_origins)
        .chain(evidence)
    {
        if seen.insert(id) {
            origins.push(EdgeEndpoint::memory(EntityKind::Fact, id));
        }
    }
    Ok(origins)
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
    let payload = CodeWorkAssignmentV1 {
        repo_id,
        target_perspective_memory_id: target_perspective_memory_id.into_inner(),
        work_item_memory_id: work_item_memory_id.into_inner(),
        reason: "shell-author retry assignment".to_string(),
    };
    // `derive_memory` is this write inside a unit of work of
    // one, and either spelling is fenced: `CodeWorkAssignmentV1` declares
    // the `code-repo` scope, so the Engine takes it in whichever transaction
    // ends up writing the row.
    let mut uow = engine
        .unit_of_work(ctx.authz())
        .await
        .map_err(ToolError::Protocol)?;
    let outcome = uow
        .derive_memory(DerivedMemory::interpretation(
            MemoryTarget::Series(SeriesHandle::new(
                work_assignment_memory_id(
                    &owner,
                    target_perspective_memory_id,
                    work_item_memory_id,
                )
                .into_inner(),
            )),
            owner,
            payload.reason.clone(),
            payload,
        ))
        .await?;
    uow.commit().await.map_err(ToolError::Protocol)?;
    Ok(outcome.memory_id.into_inner().to_string())
}

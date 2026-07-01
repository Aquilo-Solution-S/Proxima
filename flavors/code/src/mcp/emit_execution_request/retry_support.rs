use std::collections::HashSet;

use proxima_core::relation::CORE_DERIVED_FROM_RELATION;
use proxima_core::verbs::query::{EdgeFilter, EdgeReadRequest, EdgeTargetProjection};
use proxima_core::{EntityKind, EntityRef, MemoryId, ToolCtx, ToolError};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::payloads::ExecutionRequestV1;

use super::super::sql::map_storage;
use super::super::{CodeToolCtxExt, code_store, engine};
use super::edges::append_derived_edge;
use super::input_validation::normalize_text;

pub(super) fn retry_instructions(
    prior_instructions: &str,
    prior_memory_id: MemoryId,
    request_key: &str,
    instructions_append: Option<&str>,
) -> Result<String, ToolError> {
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

pub(super) fn resolve_target_perspective_id(
    ctx: &ToolCtx,
    raw: &str,
) -> Result<MemoryId, ToolError> {
    ctx.resolve_perspective_memory(raw)
}

#[derive(Debug)]
pub(super) struct PriorExecutionRequest {
    pub repo_id: Uuid,
    pub title: String,
    pub instructions: String,
}

pub(super) async fn load_execution_request(
    _tx: &mut Transaction<'_, Postgres>,
    ctx: &ToolCtx,
    memory_id: MemoryId,
) -> Result<PriorExecutionRequest, ToolError> {
    let pool = code_store(ctx)?;
    let engine = engine(ctx)?;
    let Some((_, row)) = pool
        .authorized_fact_payloads::<ExecutionRequestV1>(
            &engine,
            ctx.authz(),
            ctx.owner(),
            &[memory_id.into_inner()],
            1,
        )
        .await?
        .into_iter()
        .next()
    else {
        return Err(ToolError::InvalidInput(
            "prior_execution_request must be a visible proxima-code/work-requested-v1 Fact".into(),
        ));
    };
    Ok(PriorExecutionRequest {
        repo_id: row.repo_id,
        title: row.title,
        instructions: row.instructions,
    })
}

pub(super) async fn find_execution_request_by_key(
    _tx: &mut Transaction<'_, Postgres>,
    ctx: &ToolCtx,
    repo_id: Uuid,
    request_key: &str,
) -> Result<Option<MemoryId>, ToolError> {
    let pool = code_store(ctx)?;
    let engine = engine(ctx)?;
    let candidates: Vec<Uuid> = sqlx::query_scalar(
        "SELECT memory_id
           FROM proxima_code.work_requested_v1
          WHERE repo_id = $1
            AND request_key = $2
          ORDER BY memory_id DESC
          LIMIT 20",
    )
    .bind(repo_id)
    .bind(request_key)
    .fetch_all(pool.pool())
    .await
    .map_err(map_storage)?;
    Ok(pool
        .authorized_fact_payloads::<ExecutionRequestV1>(
            &engine,
            ctx.authz(),
            ctx.owner(),
            &candidates,
            1,
        )
        .await?
        .into_iter()
        .next()
        .map(|(id, _)| id))
}

pub(super) async fn validate_target_perspective(
    _tx: &mut Transaction<'_, Postgres>,
    ctx: &ToolCtx,
    target_perspective: MemoryId,
) -> Result<(), ToolError> {
    let pool = code_store(ctx)?;
    let engine = engine(ctx)?;
    let visible = pool
        .authorized_memory_ids(
            &engine,
            ctx.authz(),
            ctx.owner(),
            &[target_perspective.into_inner()],
            EntityKind::Perspective,
            None,
            1,
        )
        .await?;
    if visible.is_empty() {
        return Err(ToolError::InvalidInput(format!(
            "target_perspective not found: {}",
            target_perspective.into_inner()
        )));
    }
    Ok(())
}

pub(super) async fn load_prior_derived_targets(
    _tx: &mut Transaction<'_, Postgres>,
    ctx: &ToolCtx,
    prior_memory_id: MemoryId,
) -> Result<Vec<MemoryId>, ToolError> {
    let engine = engine(ctx)?;
    let response = engine
        .read_edges(
            ctx.authz(),
            &EdgeReadRequest {
                owner: ctx.owner(),
                edge_ids: Vec::new(),
                filter: EdgeFilter {
                    relation: Some(CORE_DERIVED_FROM_RELATION.to_string()),
                    source: Some(EntityRef::Memory(prior_memory_id)),
                    target: None,
                },
                limit: 500,
            },
        )
        .await?;
    Ok(response
        .edges
        .into_iter()
        .filter_map(|edge| match edge.target {
            EdgeTargetProjection::Visible {
                target: EntityRef::Memory(id),
            } => Some(id),
            EdgeTargetProjection::Visible { .. }
            | EdgeTargetProjection::Redacted
            | EdgeTargetProjection::Unavailable => None,
        })
        .collect())
}

pub(super) async fn push_derived_edge(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &ToolCtx,
    request_memory_id: MemoryId,
    evidence_memory_id: MemoryId,
    seen: &mut HashSet<MemoryId>,
    edge_ids: &mut Vec<Uuid>,
) -> Result<(), ToolError> {
    if seen.insert(evidence_memory_id) {
        edge_ids.push(append_derived_edge(tx, ctx, request_memory_id, evidence_memory_id).await?);
    }
    Ok(())
}

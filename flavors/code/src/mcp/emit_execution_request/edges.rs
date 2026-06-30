use proxima_core::relation::{
    CORE_AUTHORED_RELATION, CORE_DEPENDS_ON_RELATION, CORE_DERIVED_FROM_RELATION,
};
use proxima_core::{EdgeAuthorshipKind, EdgeId, MemoryId, ToolCtx, ToolError};
use proxima_storage_pg::verbs::edge_write::{MemoryEndpoint, append_owner_checked_memory_edge};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use super::{CODE_HAS_ACCEPTANCE_CRITERIA_RELATION, CODE_TARGETS_EXECUTION_REQUEST_RELATION};

pub(super) async fn append_acceptance_criteria_edge(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &ToolCtx,
    request_memory_id: MemoryId,
    criteria_memory_id: MemoryId,
) -> Result<Uuid, ToolError> {
    let relation = ctx
        .registry()
        .resolve_relation(CODE_HAS_ACCEPTANCE_CRITERIA_RELATION)
        .ok_or_else(|| {
            ToolError::Other(format!(
                "{CODE_HAS_ACCEPTANCE_CRITERIA_RELATION} relation not registered"
            ))
        })?;
    let edge_id = Uuid::now_v7();
    append_owner_checked_memory_edge(
        tx.as_mut(),
        &ctx.owner(),
        EdgeId::new(edge_id),
        relation,
        MemoryEndpoint::fact(request_memory_id),
        MemoryEndpoint::fact(criteria_memory_id),
        EdgeAuthorshipKind::ExternalAgent,
        ctx.caller_self_perspective(),
    )
    .await
    .map_err(ToolError::Storage)?;
    Ok(edge_id)
}

pub(super) async fn append_authored_edge(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &ToolCtx,
    planner_root: MemoryId,
    request_memory_id: MemoryId,
) -> Result<Uuid, ToolError> {
    let relation = ctx
        .registry()
        .resolve_relation(CORE_AUTHORED_RELATION)
        .ok_or_else(|| ToolError::Other("core/authored relation not registered".into()))?;
    let edge_id = Uuid::now_v7();
    append_owner_checked_memory_edge(
        tx.as_mut(),
        &ctx.owner(),
        EdgeId::new(edge_id),
        relation,
        MemoryEndpoint::perspective(planner_root),
        MemoryEndpoint::fact(request_memory_id),
        EdgeAuthorshipKind::ExternalAgent,
        Some(planner_root),
    )
    .await
    .map_err(ToolError::Storage)?;
    Ok(edge_id)
}

pub(super) async fn append_target_edge(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &ToolCtx,
    target_root: MemoryId,
    request_memory_id: MemoryId,
) -> Result<Uuid, ToolError> {
    let relation = ctx
        .registry()
        .resolve_relation(CODE_TARGETS_EXECUTION_REQUEST_RELATION)
        .ok_or_else(|| {
            ToolError::Other(format!(
                "{CODE_TARGETS_EXECUTION_REQUEST_RELATION} relation not registered"
            ))
        })?;
    let edge_id = Uuid::now_v7();
    append_owner_checked_memory_edge(
        tx.as_mut(),
        &ctx.owner(),
        EdgeId::new(edge_id),
        relation,
        MemoryEndpoint::perspective(target_root),
        MemoryEndpoint::fact(request_memory_id),
        EdgeAuthorshipKind::ExternalAgent,
        ctx.caller_self_perspective(),
    )
    .await
    .map_err(ToolError::Storage)?;
    Ok(edge_id)
}

pub(super) async fn append_derived_edge(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &ToolCtx,
    request_memory_id: MemoryId,
    evidence_memory_id: MemoryId,
) -> Result<Uuid, ToolError> {
    let relation = ctx
        .registry()
        .resolve_relation(CORE_DERIVED_FROM_RELATION)
        .ok_or_else(|| ToolError::Other("core/derived-from relation not registered".into()))?;
    let edge_id = Uuid::now_v7();
    append_owner_checked_memory_edge(
        tx.as_mut(),
        &ctx.owner(),
        EdgeId::new(edge_id),
        relation,
        MemoryEndpoint::fact(request_memory_id),
        MemoryEndpoint::fact(evidence_memory_id),
        EdgeAuthorshipKind::ExternalAgent,
        ctx.caller_self_perspective(),
    )
    .await
    .map_err(ToolError::Storage)?;
    Ok(edge_id)
}

pub(super) async fn append_dependency_edge(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &ToolCtx,
    dependent_memory_id: MemoryId,
    dependency_memory_id: MemoryId,
) -> Result<Uuid, ToolError> {
    let relation = ctx
        .registry()
        .resolve_relation(CORE_DEPENDS_ON_RELATION)
        .ok_or_else(|| ToolError::Other("core/depends-on relation not registered".into()))?;
    let mut name = Vec::with_capacity(32);
    name.extend_from_slice(dependent_memory_id.into_inner().as_bytes());
    name.extend_from_slice(dependency_memory_id.into_inner().as_bytes());
    let edge_id = Uuid::new_v5(&Uuid::NAMESPACE_OID, &name);
    append_owner_checked_memory_edge(
        tx.as_mut(),
        &ctx.owner(),
        EdgeId::new(edge_id),
        relation,
        MemoryEndpoint::fact(dependent_memory_id),
        MemoryEndpoint::fact(dependency_memory_id),
        EdgeAuthorshipKind::ExternalAgent,
        ctx.caller_self_perspective(),
    )
    .await
    .map_err(ToolError::Storage)?;
    Ok(edge_id)
}

use super::{
    GoalId, MemoryId, PayloadKind, PgCitationMappingSidecar, PgCitedObjectSidecar, PgConnection,
    PgGoalSidecar, PgMemoryPayload, PgMemoryPayloadBatchFuture, PgMemoryPayloadFuture,
    PgMemorySidecar, PgSidecarFuture, PgSidecarReadCtx, Postgres, SidecarPayload, StorageError,
    Transaction,
};

pub(super) fn insert_memory_sidecar<'t, P>(
    tx: &'t mut Transaction<'_, Postgres>,
    memory_id: MemoryId,
    payload: &'t SidecarPayload,
) -> PgSidecarFuture<'t>
where
    P: PgMemorySidecar,
{
    Box::pin(async move {
        let typed = payload.downcast_ref::<P>().ok_or_else(|| {
            StorageError::ConstraintViolation(format!(
                "sidecar payload type mismatch for {} v{} {:?}",
                payload.schema_id.as_str(),
                payload.schema_version.into_inner(),
                payload.kind,
            ))
        })?;
        typed.insert_memory_sidecar(tx, memory_id).await
    })
}

pub(super) fn load_memory_payload<P>(
    ctx: PgSidecarReadCtx<'_>,
    memory_id: MemoryId,
) -> PgMemoryPayloadFuture<'_>
where
    P: PgMemoryPayload,
{
    P::load_memory_payload(ctx, memory_id)
}

pub(super) fn load_memory_payload_batch<'t, P>(
    ctx: PgSidecarReadCtx<'t>,
    kind: PayloadKind,
    memory_ids: &'t [MemoryId],
) -> PgMemoryPayloadBatchFuture<'t>
where
    P: PgMemoryPayload,
{
    P::load_batch(ctx, kind, memory_ids)
}

pub(super) fn insert_cited_object_sidecar<'t, P>(
    tx: &'t mut PgConnection,
    cited_object_id: uuid::Uuid,
    payload: &'t SidecarPayload,
) -> PgSidecarFuture<'t>
where
    P: PgCitedObjectSidecar,
{
    Box::pin(async move {
        let typed = payload.downcast_ref::<P>().ok_or_else(|| {
            StorageError::ConstraintViolation(format!(
                "sidecar payload type mismatch for {} v{} {:?}",
                payload.schema_id.as_str(),
                payload.schema_version.into_inner(),
                payload.kind,
            ))
        })?;
        typed.insert_cited_object_sidecar(tx, cited_object_id).await
    })
}

pub(super) fn insert_citation_mapping_sidecar<'t, P>(
    tx: &'t mut PgConnection,
    citation_mapping_id: uuid::Uuid,
    payload: &'t SidecarPayload,
) -> PgSidecarFuture<'t>
where
    P: PgCitationMappingSidecar,
{
    Box::pin(async move {
        let typed = payload.downcast_ref::<P>().ok_or_else(|| {
            StorageError::ConstraintViolation(format!(
                "sidecar payload type mismatch for {} v{} {:?}",
                payload.schema_id.as_str(),
                payload.schema_version.into_inner(),
                payload.kind,
            ))
        })?;
        typed
            .insert_citation_mapping_sidecar(tx, citation_mapping_id)
            .await
    })
}

pub(super) fn insert_goal_sidecar<'t, P>(
    tx: &'t mut Transaction<'_, Postgres>,
    goal_id: GoalId,
    payload: &'t SidecarPayload,
) -> PgSidecarFuture<'t>
where
    P: PgGoalSidecar,
{
    Box::pin(async move {
        let typed = payload.downcast_ref::<P>().ok_or_else(|| {
            StorageError::ConstraintViolation(format!(
                "sidecar payload type mismatch for {} v{} {:?}",
                payload.schema_id.as_str(),
                payload.schema_version.into_inner(),
                payload.kind,
            ))
        })?;
        typed.insert_goal_sidecar(tx, goal_id).await
    })
}

pub(super) fn copy_goal_sidecar<'t, P>(
    tx: &'t mut Transaction<'_, Postgres>,
    goal_id: GoalId,
    source_goal_id: GoalId,
) -> PgSidecarFuture<'t>
where
    P: PgGoalSidecar,
{
    P::copy_goal_sidecar(tx, goal_id, source_goal_id)
}

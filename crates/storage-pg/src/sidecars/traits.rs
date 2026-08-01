use super::{
    GoalId, MemoryId, PayloadKind, PgConnection, PgFactSidecar, PgMemoryPayloadBatchFuture,
    PgMemoryPayloadFuture, PgSidecarFuture, PgSidecarReadCtx, Postgres, StorageError, Transaction,
};

pub trait PgMemorySidecar: Send + Sync + 'static {
    fn insert_memory_sidecar<'t>(
        &'t self,
        tx: &'t mut Transaction<'_, Postgres>,
        memory_id: MemoryId,
    ) -> PgSidecarFuture<'t>;
}

/// Read-back of a memory's typed sidecar payload.
///
/// An implementor MUST override at least one of `load_batch` (the batched
/// primitive the read path dispatches through — the `pg_sidecar!` macro
/// generates it) or `load_memory_payload` (the single-row convenience).
/// `load_batch`'s default fans out over `load_memory_payload`; overriding
/// NEITHER is a programming error and yields a clear `Internal` error rather
/// than recursing (the two defaults must not call each other).
pub trait PgMemoryPayload: Send + Sync + 'static {
    #[must_use]
    fn load_batch<'t>(
        ctx: PgSidecarReadCtx<'t>,
        kind: PayloadKind,
        memory_ids: &'t [MemoryId],
    ) -> PgMemoryPayloadBatchFuture<'t> {
        Box::pin(async move {
            let _ = kind;
            let mut payloads = Vec::new();
            for memory_id in memory_ids {
                if let Some(payload) = Self::load_memory_payload(ctx, *memory_id).await? {
                    payloads.push((*memory_id, payload));
                }
            }
            Ok(payloads)
        })
    }

    #[must_use]
    fn load_memory_payload(
        ctx: PgSidecarReadCtx<'_>,
        memory_id: MemoryId,
    ) -> PgMemoryPayloadFuture<'_> {
        let _ = (ctx, memory_id);
        Box::pin(async move {
            Err(StorageError::Internal(
                "PgMemoryPayload requires overriding load_batch or load_memory_payload".to_string(),
            ))
        })
    }
}

pub trait PgCitedObjectSidecar: Send + Sync + 'static {
    fn insert_cited_object_sidecar<'t>(
        &'t self,
        tx: &'t mut PgConnection,
        cited_object_id: uuid::Uuid,
    ) -> PgSidecarFuture<'t>;
}

pub trait PgCitationMappingSidecar: Send + Sync + 'static {
    fn insert_citation_mapping_sidecar<'t>(
        &'t self,
        tx: &'t mut PgConnection,
        citation_mapping_id: uuid::Uuid,
    ) -> PgSidecarFuture<'t>;
}

pub trait PgGoalSidecar: Send + Sync + 'static {
    fn insert_goal_sidecar<'t>(
        &'t self,
        tx: &'t mut Transaction<'_, Postgres>,
        goal_id: GoalId,
    ) -> PgSidecarFuture<'t>;

    fn copy_goal_sidecar<'t>(
        tx: &'t mut Transaction<'_, Postgres>,
        goal_id: GoalId,
        source_goal_id: GoalId,
    ) -> PgSidecarFuture<'t>;
}

impl<T> PgMemorySidecar for T
where
    T: PgFactSidecar + Clone + Send + Sync,
{
    fn insert_memory_sidecar<'t>(
        &'t self,
        tx: &'t mut Transaction<'_, Postgres>,
        memory_id: MemoryId,
    ) -> PgSidecarFuture<'t> {
        self.clone().insert_sidecar(tx, memory_id)
    }
}

use super::{
    GoalId, MemoryId, PayloadKind, PgFactSidecar, PgMemoryPayloadBatchFuture,
    PgMemoryPayloadFuture, PgSidecarFuture, PgSidecarReadCtx, Postgres, SidecarInsertPermit,
    StorageError, Transaction,
};

/// A flavor's typed memory-sidecar insert.
///
/// Flavors IMPLEMENT this; only the frozen registry INVOKES it. The
/// [`SidecarInsertPermit`] argument is what separates the two — an
/// implementor names the type, a caller has to mint one, and the mint is
/// crate-private. See [`SidecarInsertPermit`] for the invariant that buys.
pub trait PgMemorySidecar: Send + Sync + 'static {
    fn insert_memory_sidecar<'t>(
        &'t self,
        tx: &'t mut Transaction<'_, Postgres>,
        memory_id: MemoryId,
        permit: SidecarInsertPermit,
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
    /// How this sidecar relates to its memory's owner.
    ///
    /// See [`super::SidecarOwnerMode`]. `None` reaches the owner through the
    /// memory. `Pinned` stamps `owner_id` and keeps it across transfer
    /// (audit / `RetainAtSource`). `Follows` stamps `owner_id` for RLS but
    /// transfer rehomes the row with the memory.
    ///
    /// `pg_sidecar!` emits it (`owner_pinned: true` → Pinned, `owner: follows`
    /// → Follows, omitted → None). It has NO DEFAULT, for the same reason
    /// `MEMORY_KEY_COLUMN` has none: every declaration on this trait is
    /// stated, and a hand-written impl inherits nothing it did not say.
    const OWNER: super::SidecarOwnerMode;

    /// The column this sidecar stores its memory `t` under.
    ///
    /// `pg_sidecar!(key: …)` emits it, so a macro-generated sidecar states
    /// it once and the generated statements read it. It has NO DEFAULT on
    /// purpose: `t` as a default is the naming convention the contract's
    /// `KeyShape::MemoryT { column }` exists to replace, and a hand-written
    /// impl that inherited it would be spelling a column no declaration of
    /// its own names.
    ///
    /// This is the SECOND declaration of that column — the contract's
    /// `Surface` is the first — and
    /// `PgSidecarRegistry::check_memory_key_against_contracts` is what keeps
    /// the two one fact. A disagreement puts the typed INSERT on one column
    /// and the projection INSERT on the other.
    const MEMORY_KEY_COLUMN: &'static str;

    #[must_use]
    fn load_batch<'t>(
        ctx: PgSidecarReadCtx<'t>,
        kind: PayloadKind,
        memory_ids: &'t [MemoryId],
    ) -> PgMemoryPayloadBatchFuture<'t> {
        Box::pin(async move {
            let _ = kind;
            let mut payloads = Vec::new();
            let mut ctx = ctx;
            for memory_id in memory_ids {
                if let Some(payload) = Self::load_memory_payload(ctx.reborrow(), *memory_id).await?
                {
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
        permit: SidecarInsertPermit,
    ) -> PgSidecarFuture<'t> {
        self.clone().insert_sidecar(tx, memory_id, permit)
    }
}

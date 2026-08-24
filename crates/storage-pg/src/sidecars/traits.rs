use super::{
    GoalId, MemoryId, PayloadKind, PgConnection, PgFactSidecar, PgMemoryPayloadBatchFuture,
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
    /// Does this sidecar carry its OWN `owner_id`, stamped at write time?
    ///
    /// The default is `false`: a sidecar is an extra column on a Memory and
    /// reaches its owner through that Memory, so it follows the Memory
    /// wherever it goes. Set it for a sidecar that describes the ACT rather
    /// than the Memory — an audit row naming the actor — which must stay
    /// with the owner that wrote it when the Memory is transferred away.
    /// Erase, export, and payload hydrate all key on it.
    const OWNER_PINNED: bool = false;

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
        permit: SidecarInsertPermit,
    ) -> PgSidecarFuture<'t> {
        self.clone().insert_sidecar(tx, memory_id, permit)
    }
}

use super::{
    EdgeId, GoalId, MemoryId, PayloadKind, PgConnection, PgMemoryPayloadBatchFuture,
    PgMemoryPayloadFuture, PgSidecarFuture, PgSidecarReadCtx, Postgres, SchemaId, SchemaVersion,
    SidecarPayload, Transaction,
};

type PgMemorySidecarInserter = for<'t> fn(
    &'t mut Transaction<'_, Postgres>,
    MemoryId,
    &'t SidecarPayload,
) -> PgSidecarFuture<'t>;
type PgMemoryPayloadLoader =
    for<'t> fn(PgSidecarReadCtx<'t>, MemoryId) -> PgMemoryPayloadFuture<'t>;
type PgMemoryPayloadBatchLoader =
    for<'t> fn(PgSidecarReadCtx<'t>, PayloadKind, &'t [MemoryId]) -> PgMemoryPayloadBatchFuture<'t>;

type PgEdgeSidecarInserter =
    for<'t> fn(&'t mut PgConnection, EdgeId, &'t SidecarPayload) -> PgSidecarFuture<'t>;
type PgCitedObjectSidecarInserter =
    for<'t> fn(&'t mut PgConnection, uuid::Uuid, &'t SidecarPayload) -> PgSidecarFuture<'t>;
type PgCitationMappingSidecarInserter =
    for<'t> fn(&'t mut PgConnection, uuid::Uuid, &'t SidecarPayload) -> PgSidecarFuture<'t>;
type PgGoalSidecarInserter = for<'t> fn(
    &'t mut Transaction<'_, Postgres>,
    GoalId,
    &'t SidecarPayload,
) -> PgSidecarFuture<'t>;
type PgGoalSidecarCopier =
    for<'t> fn(&'t mut Transaction<'_, Postgres>, GoalId, GoalId) -> PgSidecarFuture<'t>;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PgSidecarKey {
    pub kind: PayloadKind,
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
}

impl PgSidecarKey {
    #[must_use]
    pub fn new(kind: PayloadKind, schema_id: SchemaId, schema_version: SchemaVersion) -> Self {
        Self {
            kind,
            schema_id,
            schema_version,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PgSidecarEntry {
    pub key: PgSidecarKey,
    pub sidecar_table: String,
    pub(super) memory_insert: Option<PgMemorySidecarInserter>,
    pub(super) memory_load: Option<PgMemoryPayloadLoader>,
    pub(super) memory_load_batch: Option<PgMemoryPayloadBatchLoader>,
    pub(super) edge_insert: Option<PgEdgeSidecarInserter>,
    pub(super) cited_object_insert: Option<PgCitedObjectSidecarInserter>,
    pub(super) citation_mapping_insert: Option<PgCitationMappingSidecarInserter>,
    pub(super) goal_insert: Option<PgGoalSidecarInserter>,
    pub(super) goal_copy: Option<PgGoalSidecarCopier>,
}

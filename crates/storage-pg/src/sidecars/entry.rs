use proxima_core::flavor::LanguagePolicy;

use super::{
    GoalId, MemoryId, PayloadKind, PgConnection, PgMemoryPayloadBatchFuture, PgMemoryPayloadFuture,
    PgSidecarFuture, PgSidecarReadCtx, Postgres, SchemaId, SchemaVersion, SidecarInsertPermit,
    SidecarPayload, Transaction,
};

type PgMemorySidecarInserter = for<'t> fn(
    &'t mut Transaction<'_, Postgres>,
    MemoryId,
    &'t SidecarPayload,
    SidecarInsertPermit,
) -> PgSidecarFuture<'t>;
type PgMemoryPayloadLoader =
    for<'t> fn(PgSidecarReadCtx<'t>, MemoryId) -> PgMemoryPayloadFuture<'t>;
type PgMemoryPayloadBatchLoader =
    for<'t> fn(PgSidecarReadCtx<'t>, PayloadKind, &'t [MemoryId]) -> PgMemoryPayloadBatchFuture<'t>;

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
    /// See [`super::PgMemoryPayload::OWNER_PINNED`]. Carried on the entry so
    /// erase and export can find owner-pinned tables without knowing which
    /// payload types they belong to.
    pub owner_pinned: bool,
    pub(super) memory_insert: Option<PgMemorySidecarInserter>,
    pub(super) memory_load: Option<PgMemoryPayloadLoader>,
    pub(super) memory_load_batch: Option<PgMemoryPayloadBatchLoader>,
    pub(super) cited_object_insert: Option<PgCitedObjectSidecarInserter>,
    pub(super) citation_mapping_insert: Option<PgCitationMappingSidecarInserter>,
    pub(super) goal_insert: Option<PgGoalSidecarInserter>,
    pub(super) goal_copy: Option<PgGoalSidecarCopier>,
    /// The generated `INSERT INTO <flavor>.projection … FROM <sidecar>`
    /// statement for this schema, or `None` when the schema is not a search
    /// surface. Built in `freeze_against` from the flavor contract, so a
    /// projected schema cannot be written without its projection row: the
    /// two statements are one method call apart in one transaction.
    pub(super) projection_insert: Option<String>,
    /// The projection table this entry maintains. Carried so transfer can
    /// ask the registry which tables follow an owner without knowing which
    /// flavors are linked.
    pub(super) projection_table: Option<String>,
    /// The projected schema's declared language policy, `None` when the
    /// schema writes no projection row.
    ///
    /// The write path reads it instead of taking a language on faith: a
    /// pinned policy carries its configuration INSIDE `projection_insert`
    /// as a literal and its statement has no language bind to fill, so
    /// there is nothing for a caller to pass; `PerRow` says the row's
    /// language is the writing draft's, so the draft must carry one.
    pub(super) projection_language: Option<LanguagePolicy>,
}

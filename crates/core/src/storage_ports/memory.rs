pub use super::proof::{OperatorWriteProof, OwnerWritePermit};

/// Newest-first inbound pin page. Kind selects which array; `None` is both.
#[derive(Debug, Clone, Copy)]
pub struct InboundPinQuery<'a> {
    pub targets: &'a [MemoryId],
    pub kind: Option<EdgeKind>,
    pub heads_only: bool,
    pub after: Option<MemoryId>,
    pub limit: u32,
    /// Whether `targets` name Goals rather than Memories. Reference pins now
    /// live in two columns, so the backend searches exactly the one this
    /// target kind can appear in instead of scanning a mixed array.
    pub goal_targets: bool,
}

use crate::edge::{EdgeKind, PinNode};
use crate::read_models::{MemoryCounts, MemorySchemaSpec, MemorySnapshot};
use crate::storage::{
    AuthorDerivedOutcome, AuthorDerivedRequest, MemoryGraphIdentity, MemoryGraphPayloadRow,
    MemoryKindRow, StorageError,
};
use crate::{GoalId, MemoryId, OwnerId, OwnerScope};

/// Node writes that also assert index rows.
///
/// There is deliberately no edge-append method. An edge is never a
/// free-standing act (docs/concepts.md): every index row
/// this port writes is a consequence of the node write carrying it, in
/// that write's own transaction, and the row's kind follows the
/// declaration it came from — `origins` produce
/// [`crate::EdgeKind::Origin`] rows, `references`
/// [`crate::EdgeKind::Reference`] rows. Storage never reads a kind off
/// the request because none is transmitted.
#[async_trait::async_trait]
pub trait MemoryAuthoringPort: Send + Sync {
    /// Append one already-authorized derived memory together with the
    /// source-owned origins/refs in one transaction. Revisions append to
    /// the prior row's series; neither row stores a supersession pointer.
    /// Public callers cannot forge `OperatorWriteProof`; use
    /// `Engine::derive_memory`.
    async fn author_derived(
        &self,
        req: &AuthorDerivedRequest<'_>,
        permit: &OwnerWritePermit,
        proof: OperatorWriteProof,
    ) -> Result<AuthorDerivedOutcome, StorageError>;

    async fn load_memory_kinds(
        &self,
        owner_scope: Option<&OwnerScope>,
        owner: &OwnerId,
        memory_ids: &[MemoryId],
    ) -> Result<Vec<MemoryKindRow>, StorageError>;

    /// Forget one memory `t`: mark its row `forgotten_at`, so owner reads stop
    /// seeing it, and delete what search and embedding rebuild. The payload
    /// stays in place for a restore; no object store is involved.
    async fn forget_memory(
        &self,
        permit: &OwnerWritePermit,
        memory_id: MemoryId,
    ) -> Result<(), StorageError>;

    /// Bring one forgotten memory `t` back: clear its `forgotten_at`,
    /// rebuild what forget deleted (search projection in the memory's own
    /// lexical language, embedding jobs for `embedding_spaces`) and move the
    /// series head back. Restoring a live memory is a no-op success; an
    /// absent, foreign or unwritable-kind `t` is `NotFound`.
    ///
    /// `embedding_spaces` are the spaces the permit Owner's route names now,
    /// not the ones the memory had vectors in before the forget.
    ///
    /// Defaults to refusing, so a backend written before restore keeps
    /// compiling and says so instead of pretending.
    async fn restore_memory(
        &self,
        _permit: &OwnerWritePermit,
        _memory_id: MemoryId,
        _embedding_spaces: &[crate::EmbeddingSpace],
    ) -> Result<(), StorageError> {
        Err(StorageError::Internal(
            "storage backend does not implement memory restore".into(),
        ))
    }
}

#[async_trait::async_trait]
pub trait MemoryReadPort: Send + Sync {
    /// The persisted number this backend uses when minting memory UUIDs.
    fn store_number(&self) -> u16;

    async fn load_fact_text(
        &self,
        owner_scope: Option<&OwnerScope>,
        owner: &OwnerId,
        memory_id: crate::MemoryId,
    ) -> Result<Option<String>, StorageError>;

    async fn load_memory_graph_payloads(
        &self,
        owner_scope: Option<&OwnerScope>,
        identities: &[MemoryGraphIdentity],
        schemas: &[MemorySchemaSpec],
        include_body: bool,
    ) -> Result<Vec<MemoryGraphPayloadRow>, StorageError>;

    /// Owner-scoped PK load of pin carriers (`t`, kind, `origins`, `refs`).
    /// Missing and unreadable ids are absent.
    /// Owner-scoped persisted one-liners. Missing/unreadable ids are absent.
    async fn load_sketches(
        &self,
        owner_scope: Option<&OwnerScope>,
        read_owners: &[OwnerId],
        memory_ids: &[MemoryId],
    ) -> Result<Vec<crate::read_models::MemorySketch>, StorageError>;

    async fn load_pin_nodes(
        &self,
        owner_scope: Option<&OwnerScope>,
        read_owners: &[OwnerId],
        memory_ids: &[MemoryId],
    ) -> Result<Vec<PinNode>, StorageError>;

    /// Resolve the subset of candidate reference ids that are readable Goal
    /// rows for this owner set. Non-returned ids remain unresolved UUIDs, so
    /// callers can redact them without probing one target at a time.
    async fn load_visible_goal_ids(
        &self,
        owner_scope: Option<&OwnerScope>,
        read_owners: &[OwnerId],
        goal_ids: &[GoalId],
    ) -> Result<Vec<GoalId>, StorageError>;

    /// Owner-scoped GIN page of rows that list any of `query.targets` in
    /// `origins` and/or `refs`. Newest `t` first; `after` is exclusive.
    /// `limit == 0` is a constraint violation.
    async fn load_inbound_pin_nodes(
        &self,
        owner_scope: Option<&OwnerScope>,
        read_owners: &[OwnerId],
        query: InboundPinQuery<'_>,
    ) -> Result<Vec<PinNode>, StorageError>;

    async fn query_memories(
        &self,
        owner_scope: Option<&OwnerScope>,
        read_owners: &[OwnerId],
        req: &crate::verbs::query::QueryRequest,
        schemas: &[MemorySchemaSpec],
    ) -> Result<crate::verbs::query::QueryResponse, StorageError>;

    /// Count the current memories `read_owners` holds, by kind: one per
    /// series head (`memory_head`) for the three memory kinds and one per
    /// Goal (`goal_head`) in any state. A stateful series counts once
    /// however many versions it has; a forgotten memory has left the heads
    /// and is not counted. `read_owners` and the backend's per-kind read
    /// scope narrow the count exactly as they narrow `query_memories`.
    ///
    /// A stored kind the backend cannot parse is an error, never a skipped
    /// or defaulted row.
    async fn count_memories(
        &self,
        owner_scope: Option<&OwnerScope>,
        read_owners: &[OwnerId],
    ) -> Result<MemoryCounts, StorageError>;

    async fn search_memories(
        &self,
        owner_scope: Option<&OwnerScope>,
        req: &crate::verbs::query::MemorySearchRequest,
        projections: &[crate::verbs::schema::MemorySearchProjection],
    ) -> Result<crate::verbs::query::MemorySearchPage, StorageError>;

    async fn walk_memory_lineage(
        &self,
        owner_scope: Option<&OwnerScope>,
        read_owners: &[OwnerId],
        req: &crate::verbs::query::MemoryLineageRequest,
    ) -> Result<crate::verbs::query::MemoryLineageResponse, StorageError>;

    /// The other-store edges recorded for `memory_ids` that `read_owners`
    /// hold: `(memory, other-store memory, kind)`, kind `Origin` or
    /// `Reference` (#480). The same rows a lineage page reports, for the
    /// ancestor walk `core_think` and lineage share.
    async fn external_edges(
        &self,
        owner_scope: Option<&OwnerScope>,
        read_owners: &[OwnerId],
        memory_ids: &[crate::MemoryId],
    ) -> Result<Vec<(crate::MemoryId, uuid::Uuid, crate::EdgeKind)>, StorageError>;

    /// The lineage nodes (kind, schema, snippet) of the `memory_ids`
    /// `read_owners` hold, as a lineage page renders them; `distance` is 0
    /// and set by the caller. An id the caller cannot read is absent.
    async fn lineage_nodes(
        &self,
        owner_scope: Option<&OwnerScope>,
        read_owners: &[OwnerId],
        memory_ids: &[crate::MemoryId],
    ) -> Result<Vec<crate::verbs::query::MemoryLineageNode>, StorageError>;

    /// The forgotten rows among `memory_ids` that `read_owners` could read
    /// were they not forgotten, as lineage nodes (no snippet, distance 0).
    /// Bound by the same owner and kind permits as every read: an id the
    /// caller could not read in either state is absent, so a pin to it stays
    /// redacted.
    async fn forgotten_nodes(
        &self,
        owner_scope: Option<&OwnerScope>,
        read_owners: &[OwnerId],
        memory_ids: &[crate::MemoryId],
    ) -> Result<Vec<crate::verbs::query::MemoryLineageNode>, StorageError>;

    /// Current owned series handle whose sidecar matches `columns`.
    /// Owner-only. A miss after an owner-to-owner transfer is expected.
    async fn owned_series_handle(
        &self,
        owner_scope: Option<&OwnerScope>,
        owner: OwnerId,
        schema_id: &crate::SchemaId,
        sidecar_table: &str,
        columns: &[(&str, crate::verbs::query::SidecarAtom)],
    ) -> Result<Option<uuid::Uuid>, StorageError>;
}

#[async_trait::async_trait]
pub trait MemoryInspectPort: Send + Sync {
    async fn load_memory_by_id(
        &self,
        owner_scope: Option<&OwnerScope>,
        memory_id: crate::MemoryId,
        schemas: &[MemorySchemaSpec],
    ) -> Result<Option<MemorySnapshot>, StorageError>;

    /// Batch counterpart of [`Self::load_memory_by_id`], visibility-scoped:
    /// returns snapshots for the subset of `memory_ids` readable by
    /// `read_owners`. Unknown, invisible, and tombstoned ids are simply
    /// absent from the result; order is unspecified.
    async fn load_memories_by_ids(
        &self,
        owner_scope: Option<&OwnerScope>,
        read_owners: &[OwnerId],
        memory_ids: &[crate::MemoryId],
        schemas: &[MemorySchemaSpec],
    ) -> Result<Vec<MemorySnapshot>, StorageError>;
}

#[async_trait::async_trait]
pub trait CitationPort: Send + Sync {
    /// One page of citing Facts, newest first
    /// (`created_at DESC, memory_id DESC`), starting strictly after
    /// `after` when given. The page computes its own `has_more` and
    /// `next_cursor` by over-fetching one row past `limit`.
    async fn facts_citing_object(
        &self,
        owner_scope: Option<&OwnerScope>,
        read_owners: &[OwnerId],
        cited_object_id: uuid::Uuid,
        schemas: &[MemorySchemaSpec],
        after: Option<crate::verbs::query::FactCitationCursor>,
        limit: u32,
    ) -> Result<crate::verbs::query::FactCitationPage, StorageError>;

    async fn citation_of_fact(
        &self,
        owner_scope: Option<&OwnerScope>,
        read_owners: &[OwnerId],
        fact_memory_id: crate::MemoryId,
    ) -> Result<Option<crate::verbs::query::FactCitationReadback>, StorageError>;
}

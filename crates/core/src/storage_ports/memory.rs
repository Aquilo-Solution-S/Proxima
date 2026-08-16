pub use super::proof::{OperatorWriteProof, OwnerWritePermit};

use crate::edge::PinNode;
use crate::read_models::{MemorySnapshot, SidecarSpec};
use crate::storage::{
    AuthorDerivedOutcome, AuthorDerivedRequest, FactSourceBatchRow, MemoryGraphPayloadRow,
    MemoryKindRow, StorageError,
};
use crate::{MemoryId, Owner, OwnerRef};

/// Node writes that also assert index rows.
///
/// There is deliberately no edge-append method. An edge is never a
/// free-standing act (docs/16 §Kernel Invariants, E4): every index row
/// this port writes is a consequence of the node write carrying it, in
/// that write's own transaction, and the row's kind follows the
/// declaration it came from — `origins` produce
/// [`crate::EdgeKind::Origin`] rows, `references`
/// [`crate::EdgeKind::Reference`] rows. Storage never reads a kind off
/// the request because none is transmitted.
#[async_trait::async_trait]
pub trait MemoryAuthoringPort: Send + Sync {
    /// Append one already-authorized derived memory together with the
    /// index rows its declarations imply and, when it revises a prior
    /// head, the supersession lineage pointer — one transaction.
    ///
    /// Index writes are idempotent by construction: the primary key is
    /// `(source, target, kind)`, so a replayed write re-asserts the same
    /// rows. Public callers cannot forge `OperatorWriteProof`; route
    /// through `Engine::author_derived_authorized` instead.
    async fn author_derived(
        &self,
        req: &AuthorDerivedRequest<'_>,
        permit: &OwnerWritePermit,
        proof: OperatorWriteProof,
    ) -> Result<AuthorDerivedOutcome, StorageError>;

    async fn load_memory_kinds(
        &self,
        owner: &Owner,
        memory_ids: &[MemoryId],
    ) -> Result<Vec<MemoryKindRow>, StorageError>;

    async fn load_fact_source_batches(
        &self,
        owner: &Owner,
        memory_ids: &[MemoryId],
    ) -> Result<Vec<FactSourceBatchRow>, StorageError>;

    /// Cool one memory `t`: PUT cold object, stub + delete hot, `announce.forget`.
    async fn forget_memory(
        &self,
        permit: &OwnerWritePermit,
        memory_id: MemoryId,
    ) -> Result<(), StorageError>;
}

#[async_trait::async_trait]
pub trait MemoryReadPort: Send + Sync {
    async fn load_fact_text(
        &self,
        owner: &Owner,
        memory_id: crate::MemoryId,
    ) -> Result<Option<String>, StorageError>;

    async fn load_memory_graph_payloads(
        &self,
        owner: &Owner,
        memory_ids: &[MemoryId],
        include_body: bool,
    ) -> Result<Vec<MemoryGraphPayloadRow>, StorageError>;

    /// Owner-scoped PK load of pin carriers (`t`, kind, `origins`, `refs`).
    /// Missing and unreadable ids are absent.
    async fn load_pin_nodes(
        &self,
        read_owners: &[OwnerRef],
        memory_ids: &[MemoryId],
    ) -> Result<Vec<PinNode>, StorageError>;

    /// Owner-scoped GIN load of rows that list any of `memory_ids` in
    /// `origins` or `refs`.
    async fn load_inbound_pin_nodes(
        &self,
        read_owners: &[OwnerRef],
        memory_ids: &[MemoryId],
    ) -> Result<Vec<PinNode>, StorageError>;

    async fn query_memories(
        &self,
        req: &crate::verbs::query::QueryRequest,
        schemas: &[crate::verbs::schema::SchemaInfo],
    ) -> Result<crate::verbs::query::QueryResponse, StorageError>;

    async fn search_memories(
        &self,
        req: &crate::verbs::query::MemorySearchRequest,
        projections: &[crate::verbs::schema::MemorySearchProjection],
    ) -> Result<crate::verbs::query::MemorySearchPage, StorageError>;

    async fn walk_memory_lineage(
        &self,
        read_owners: &[OwnerRef],
        req: &crate::verbs::query::MemoryLineageRequest,
    ) -> Result<crate::verbs::query::MemoryLineageResponse, StorageError>;
}

#[async_trait::async_trait]
pub trait MemoryInspectPort: Send + Sync {
    async fn load_memory_by_id(
        &self,
        memory_id: crate::MemoryId,
        sidecars: &[SidecarSpec],
    ) -> Result<Option<MemorySnapshot>, StorageError>;

    /// Batch counterpart of [`Self::load_memory_by_id`], visibility-scoped:
    /// returns snapshots for the subset of `memory_ids` readable by
    /// `read_owners`. Unknown, invisible, and tombstoned ids are simply
    /// absent from the result; order is unspecified.
    async fn load_memories_by_ids(
        &self,
        read_owners: &[OwnerRef],
        memory_ids: &[crate::MemoryId],
        sidecars: &[SidecarSpec],
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
        read_owners: &[OwnerRef],
        cited_object_id: uuid::Uuid,
        sidecars: &[SidecarSpec],
        after: Option<crate::verbs::query::FactCitationCursor>,
        limit: u32,
    ) -> Result<crate::verbs::query::FactCitationPage, StorageError>;

    async fn citation_of_fact(
        &self,
        read_owners: &[OwnerRef],
        fact_memory_id: crate::MemoryId,
    ) -> Result<Option<crate::verbs::query::FactCitationReadback>, StorageError>;
}

pub use super::proof::{EdgeWriteProof, OperatorWriteProof, OwnerWritePermit};

use crate::dependency::MemoryDependency;
use crate::read_models::{MemorySnapshot, SidecarSpec};
use crate::storage::{
    AuthorDerivedOutcome, AuthorDerivedRequest, EdgeEndpointKindRow, FactSourceBatchRow,
    MemoryGraphPayloadRow, MemoryKindRow, NeighborEdgeRow, StorageError,
};
use crate::{
    DerivedEdgeSpec, EdgeId, FactEntityId, MemoryId, Owner, OwnerRef, SchemaId, SchemaVersion,
};

#[async_trait::async_trait]
pub trait MemoryAuthoringPort: Send + Sync {
    /// Append one already-authorized derived memory. Public callers cannot
    /// forge `OperatorWriteProof`; route through
    /// `Engine::author_derived_authorized` instead.
    async fn author_derived(
        &self,
        req: &AuthorDerivedRequest<'_>,
        permit: &OwnerWritePermit,
        proof: OperatorWriteProof,
    ) -> Result<AuthorDerivedOutcome, StorageError>;

    /// Append one already-authorized memory edge. Public callers cannot forge
    /// `EdgeWriteProof`; route through engine/checked edge-write APIs instead.
    async fn append_memory_edge(
        &self,
        edge: &DerivedEdgeSpec<'_>,
        permit: &OwnerWritePermit,
        proof: EdgeWriteProof,
    ) -> Result<EdgeId, StorageError>;

    async fn load_memory_kinds(
        &self,
        _owner: &Owner,
        _memory_ids: &[MemoryId],
    ) -> Result<Vec<MemoryKindRow>, StorageError> {
        Ok(Vec::new())
    }

    async fn load_fact_source_batches(
        &self,
        _owner: &Owner,
        _memory_ids: &[MemoryId],
    ) -> Result<Vec<FactSourceBatchRow>, StorageError> {
        Ok(Vec::new())
    }

    async fn load_memory_edge_ids(
        &self,
        _owner: &Owner,
        _relation: &str,
        _source_memory_id: MemoryId,
        _target_memory_ids: &[MemoryId],
    ) -> Result<Vec<EdgeId>, StorageError> {
        Ok(Vec::new())
    }
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
        _owner: &Owner,
        _memory_ids: &[MemoryId],
        _include_body: bool,
    ) -> Result<Vec<MemoryGraphPayloadRow>, StorageError> {
        Ok(Vec::new())
    }

    async fn load_neighbor_memory_edges(
        &self,
        _read_owners: &[OwnerRef],
        _memory_ids: &[MemoryId],
        _limit: usize,
    ) -> Result<Vec<NeighborEdgeRow>, StorageError> {
        Ok(Vec::new())
    }

    async fn load_edge_endpoint_kinds(
        &self,
        _edge_ids: &[EdgeId],
    ) -> Result<Vec<EdgeEndpointKindRow>, StorageError> {
        Ok(Vec::new())
    }

    async fn query_memories(
        &self,
        req: &crate::verbs::query::QueryRequest,
        schemas: &[crate::verbs::schema::SchemaInfo],
    ) -> Result<crate::verbs::query::QueryResponse, StorageError>;

    async fn search_memories(
        &self,
        req: &crate::verbs::query::MemorySearchRequest,
        projections: &[crate::verbs::schema::MemorySearchProjection],
    ) -> Result<Vec<crate::verbs::query::MemorySearchResult>, StorageError>;

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

    async fn list_memory_dependencies(
        &self,
        _owner: &Owner,
        _source_memory_id: crate::MemoryId,
    ) -> Result<Vec<MemoryDependency>, StorageError> {
        Ok(Vec::new())
    }
}

#[async_trait::async_trait]
pub trait EdgeReadPort: Send + Sync {
    async fn read_edges(
        &self,
        read_owners: &[OwnerRef],
        req: &crate::verbs::query::EdgeReadRequest,
    ) -> Result<crate::verbs::query::EdgeReadResponse, StorageError>;

    async fn edge_exists(
        &self,
        read_owners: &[OwnerRef],
        req: &crate::verbs::query::EdgeExistsRequest,
    ) -> Result<crate::verbs::query::EdgeExistsResponse, StorageError>;
}

#[async_trait::async_trait]
pub trait CitationPort: Send + Sync {
    async fn fact_entity_id_for(
        &self,
        owner: &Owner,
        schema_id: &SchemaId,
        schema_version: SchemaVersion,
        natural_key: &[String],
    ) -> Result<Option<FactEntityId>, StorageError>;

    async fn facts_citing_object(
        &self,
        read_owners: &[OwnerRef],
        cited_object_id: uuid::Uuid,
        sidecars: &[SidecarSpec],
    ) -> Result<Vec<MemorySnapshot>, StorageError>;

    async fn citation_of_fact(
        &self,
        fact_memory_id: crate::MemoryId,
    ) -> Result<Option<crate::verbs::query::FactCitationReadback>, StorageError>;

    async fn citation_of_entity_head(
        &self,
        read_owners: &[OwnerRef],
        fact_entity_id: FactEntityId,
    ) -> Result<Option<crate::verbs::query::FactCitationReadback>, StorageError>;
}

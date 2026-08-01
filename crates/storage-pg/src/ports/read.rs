use proxima_core::read_models::{ChangeEventForWake, SidecarSpec};
use proxima_core::storage_ports::{ChangeEventPort, CitationPort, EdgeReadPort};
use proxima_core::verbs::change_history::{ChangeHistoryRequest, ChangeHistoryResponse};
use proxima_core::verbs::query::{
    EdgeExistsRequest, EdgeExistsResponse, EdgeReadRequest, EdgeReadResponse, FactCitationReadback,
};
use proxima_core::{
    FactEntityId, MemoryId, Owner, OwnerRef, SchemaId, SchemaVersion, StorageError,
};

use crate::{PgStorage, verbs};

#[async_trait::async_trait]
impl ChangeEventPort for PgStorage {
    async fn change_history(
        &self,
        read_owners: &[OwnerRef],
        req: &ChangeHistoryRequest,
    ) -> Result<ChangeHistoryResponse, StorageError> {
        verbs::change_history::change_history(&self.pool, read_owners, req).await
    }

    async fn list_change_events_after(
        &self,
        read_owners: &[OwnerRef],
        after: uuid::Uuid,
        limit: usize,
    ) -> Result<Vec<ChangeEventForWake>, StorageError> {
        verbs::consolidate::list_change_events_after(&self.pool, read_owners, after, limit).await
    }

    async fn list_change_events_for_replay(
        &self,
        owner: &Owner,
        after: uuid::Uuid,
        until: Option<uuid::Uuid>,
        limit: usize,
    ) -> Result<Vec<ChangeEventForWake>, StorageError> {
        verbs::consolidate::list_change_events_for_replay(&self.pool, owner, after, until, limit)
            .await
    }
}

#[async_trait::async_trait]
impl EdgeReadPort for PgStorage {
    async fn read_edges(
        &self,
        read_owners: &[OwnerRef],
        req: &EdgeReadRequest,
    ) -> Result<EdgeReadResponse, StorageError> {
        verbs::query::read_edges(&self.pool, read_owners, req).await
    }

    async fn edge_exists(
        &self,
        read_owners: &[OwnerRef],
        req: &EdgeExistsRequest,
    ) -> Result<EdgeExistsResponse, StorageError> {
        verbs::query::edge_exists(&self.pool, read_owners, req).await
    }
}

#[async_trait::async_trait]
impl CitationPort for PgStorage {
    async fn fact_entity_id_for(
        &self,
        owner: &Owner,
        schema_id: &SchemaId,
        schema_version: SchemaVersion,
        natural_key: &[String],
    ) -> Result<Option<FactEntityId>, StorageError> {
        verbs::query::fact_entity_id_for_pool(
            &self.pool,
            owner,
            schema_id,
            schema_version,
            natural_key,
        )
        .await
    }

    async fn facts_citing_object(
        &self,
        read_owners: &[OwnerRef],
        cited_object_id: uuid::Uuid,
        sidecars: &[SidecarSpec],
        after: Option<proxima_core::verbs::query::FactCitationCursor>,
        limit: u32,
    ) -> Result<proxima_core::verbs::query::FactCitationPage, StorageError> {
        verbs::query::facts_citing_object(
            &self.pool,
            &self.sidecars,
            read_owners,
            cited_object_id,
            sidecars,
            after,
            limit,
        )
        .await
    }

    async fn citation_of_fact(
        &self,
        fact_memory_id: MemoryId,
    ) -> Result<Option<FactCitationReadback>, StorageError> {
        verbs::query::citation_of_fact(&self.pool, fact_memory_id).await
    }

    async fn citation_of_entity_head(
        &self,
        read_owners: &[OwnerRef],
        fact_entity_id: FactEntityId,
    ) -> Result<Option<FactCitationReadback>, StorageError> {
        verbs::query::citation_of_entity_head(&self.pool, read_owners, fact_entity_id).await
    }
}

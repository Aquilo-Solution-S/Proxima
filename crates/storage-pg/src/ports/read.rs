use proxima_core::read_models::{ChangeEventForWake, SidecarSpec};
use proxima_core::storage_ports::{ChangeEventPort, CitationPort};
use proxima_core::verbs::change_history::{ChangeHistoryRequest, ChangeHistoryResponse};
use proxima_core::verbs::query::FactCitationReadback;
use proxima_core::{MemoryId, Owner, OwnerRef, StorageError};

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
impl CitationPort for PgStorage {
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
}

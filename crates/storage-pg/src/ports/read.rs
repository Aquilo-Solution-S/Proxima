use proxima_core::read_models::{ChangeEventForWake, MemorySchemaSpec};
use proxima_core::storage_ports::{ChangeEventPort, CitationPort};
use proxima_core::verbs::change_history::{ChangeHistoryRequest, ChangeHistoryResponse};
use proxima_core::verbs::query::FactCitationReadback;
use proxima_core::{MemoryId, Owner, OwnerRef, StorageError};

use crate::{PgStorage, verbs};

#[async_trait::async_trait]
impl ChangeEventPort for PgStorage {
    async fn change_history(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
        read_owners: &[OwnerRef],
        req: &ChangeHistoryRequest,
    ) -> Result<ChangeHistoryResponse, StorageError> {
        let mut tx =
            crate::owner_scope::begin_compatible_owner_transaction(&self.pool, owner_scope).await?;
        let result = async {
            verbs::change_history::change_history_on_connection(&mut tx, read_owners, req).await
        }
        .await;
        crate::owner_scope::finish_transaction(tx, result).await
    }

    async fn list_change_events_after(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
        read_owners: &[OwnerRef],
        after: uuid::Uuid,
        limit: usize,
    ) -> Result<Vec<ChangeEventForWake>, StorageError> {
        let mut tx =
            crate::owner_scope::begin_compatible_owner_transaction(&self.pool, owner_scope).await?;
        let result = async {
            verbs::consolidate::list_change_events_after_on_connection(
                &mut tx,
                read_owners,
                after,
                limit,
            )
            .await
        }
        .await;
        crate::owner_scope::finish_transaction(tx, result).await
    }

    async fn list_change_events_for_replay(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
        owner: &Owner,
        after: uuid::Uuid,
        until: Option<uuid::Uuid>,
        limit: usize,
    ) -> Result<Vec<ChangeEventForWake>, StorageError> {
        let mut tx =
            crate::owner_scope::begin_compatible_owner_transaction(&self.pool, owner_scope).await?;
        let result = async {
            verbs::consolidate::list_change_events_for_replay_on_connection(
                &mut tx, owner, after, until, limit,
            )
            .await
        }
        .await;
        crate::owner_scope::finish_transaction(tx, result).await
    }
}

#[async_trait::async_trait]
impl CitationPort for PgStorage {
    async fn facts_citing_object(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
        read_owners: &[OwnerRef],
        cited_object_id: uuid::Uuid,
        schemas: &[MemorySchemaSpec],
        after: Option<proxima_core::verbs::query::FactCitationCursor>,
        limit: u32,
    ) -> Result<proxima_core::verbs::query::FactCitationPage, StorageError> {
        let mut tx =
            crate::owner_scope::begin_compatible_owner_transaction(&self.pool, owner_scope).await?;
        let result = async {
            verbs::query::facts_citing_object_on_connection(
                &mut tx,
                &self.sidecars,
                read_owners,
                cited_object_id,
                schemas,
                after,
                limit,
            )
            .await
        }
        .await;
        crate::owner_scope::finish_transaction(tx, result).await
    }

    async fn citation_of_fact(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
        read_owners: &[OwnerRef],
        fact_memory_id: MemoryId,
    ) -> Result<Option<FactCitationReadback>, StorageError> {
        let mut tx =
            crate::owner_scope::begin_compatible_owner_transaction(&self.pool, owner_scope).await?;
        let result = async {
            verbs::query::citation_of_fact_on_connection(&mut tx, read_owners, fact_memory_id).await
        }
        .await;
        crate::owner_scope::finish_transaction(tx, result).await
    }
}

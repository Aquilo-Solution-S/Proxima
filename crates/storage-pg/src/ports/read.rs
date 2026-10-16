use proxima_core::read_models::MemorySchemaSpec;
use proxima_core::storage_ports::CitationPort;
use proxima_core::verbs::query::FactCitationReadback;
use proxima_core::{MemoryId, OwnerId, StorageError};

use crate::{PgStorage, verbs};

#[async_trait::async_trait]
impl CitationPort for PgStorage {
    async fn facts_citing_object(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
        read_owners: &[OwnerId],
        cited_object_id: uuid::Uuid,
        schemas: &[MemorySchemaSpec],
        after: Option<proxima_core::verbs::query::FactCitationCursor>,
        limit: u32,
    ) -> Result<proxima_core::verbs::query::FactCitationPage, StorageError> {
        let mut tx = crate::owner_scope::begin_owner_transaction(
            &self.pool,
            crate::owner_scope::require_owner_scope(owner_scope)?,
        )
        .await?;
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
        read_owners: &[OwnerId],
        fact_memory_id: MemoryId,
    ) -> Result<Option<FactCitationReadback>, StorageError> {
        let mut tx = crate::owner_scope::begin_owner_transaction(
            &self.pool,
            crate::owner_scope::require_owner_scope(owner_scope)?,
        )
        .await?;
        let result = async {
            verbs::query::citation_of_fact_on_connection(&mut tx, read_owners, fact_memory_id).await
        }
        .await;
        crate::owner_scope::finish_transaction(tx, result).await
    }
}

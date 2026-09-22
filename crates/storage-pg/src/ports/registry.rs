use proxima_core::read_models::{AbstractionRow, FactRow, MemorySchemaSpec};
use proxima_core::storage_ports::RegistryProjectionPort;
use proxima_core::{Owner, StorageError};

use crate::{PgStorage, verbs};

#[async_trait::async_trait]
impl RegistryProjectionPort for PgStorage {
    async fn load_memory_batch_facts(
        &self,
        _owner_scope: Option<&proxima_core::OwnerScope>,
        owner: &Owner,
        memory_id: proxima_core::MemoryId,
        schemas: &[MemorySchemaSpec],
    ) -> Result<Vec<FactRow>, StorageError> {
        verbs::consolidate::load_memory_batch_facts(
            &self.pool,
            &self.sidecars,
            owner,
            memory_id,
            schemas,
        )
        .await
    }

    async fn load_abstraction_heads(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
        owner: &Owner,
        schemas: &[MemorySchemaSpec],
        limit: usize,
    ) -> Result<Vec<AbstractionRow>, StorageError> {
        let mut tx = crate::begin_compatible_owner_transaction(&self.pool, owner_scope).await?;
        let result = verbs::consolidate::load_abstraction_heads_on_connection(
            tx.as_mut(),
            &self.sidecars,
            owner,
            schemas,
            limit,
        )
        .await;
        crate::owner_scope::finish_transaction(tx, result).await
    }
}

use proxima_core::read_models::{AbstractionRow, FactRow, SidecarSpec};
use proxima_core::storage_ports::RegistryProjectionPort;
use proxima_core::{Owner, StorageError};

use crate::{PgStorage, verbs};

#[async_trait::async_trait]
impl RegistryProjectionPort for PgStorage {
    async fn load_memory_batch_facts(
        &self,
        owner: &Owner,
        memory_id: proxima_core::MemoryId,
        sidecars: &[SidecarSpec],
    ) -> Result<Vec<FactRow>, StorageError> {
        verbs::consolidate::load_memory_batch_facts(
            &self.pool,
            &self.sidecars,
            owner,
            memory_id,
            sidecars,
        )
        .await
    }

    async fn load_abstraction_heads(
        &self,
        owner: &Owner,
        sidecars: &[SidecarSpec],
        limit: usize,
    ) -> Result<Vec<AbstractionRow>, StorageError> {
        verbs::consolidate::load_abstraction_heads(
            &self.pool,
            &self.sidecars,
            owner,
            sidecars,
            limit,
        )
        .await
    }
}

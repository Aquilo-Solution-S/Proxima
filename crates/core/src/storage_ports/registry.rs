use crate::Owner;
use crate::read_models::{AbstractionRow, FactRow, SidecarSpec};
use crate::storage::StorageError;

#[async_trait::async_trait]
pub trait RegistryProjectionPort: Send + Sync {
    async fn load_memory_batch_facts(
        &self,
        owner: &Owner,
        memory_id: crate::MemoryId,
        sidecars: &[SidecarSpec],
    ) -> Result<Vec<FactRow>, StorageError>;

    async fn load_abstraction_heads(
        &self,
        owner: &Owner,
        sidecars: &[SidecarSpec],
        limit: usize,
    ) -> Result<Vec<AbstractionRow>, StorageError>;
}

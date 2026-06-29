//! Core dependency-gated dispatch contracts.

use crate::{MemoryId, MemoryInspectPort, Owner, SchemaId, StorageError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryDependency {
    pub dependency_memory_id: MemoryId,
    pub dependency_schema_id: SchemaId,
}

#[async_trait::async_trait]
pub trait DependencySatisfactionRule: std::fmt::Debug + Send + Sync {
    fn target_schema_id(&self) -> &'static str;

    async fn is_satisfied(
        &self,
        storage: &dyn MemoryInspectPort,
        owner: &Owner,
        dependency_memory_id: MemoryId,
    ) -> Result<bool, StorageError>;
}

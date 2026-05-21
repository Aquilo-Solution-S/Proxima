//! Core dependency-gated dispatch contracts.

use crate::{MemoryId, Owner, SchemaId, Storage, StorageError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryDependency {
    pub dependency_memory_id: MemoryId,
    pub dependency_schema_id: SchemaId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockedWakeCandidate {
    pub owner: Owner,
    pub personality_instance_id: crate::PersonalityInstanceId,
    pub wake_entry_id: uuid::Uuid,
    pub change_event_seq: uuid::Uuid,
    pub triggering_memory_id: MemoryId,
    pub dependency_memory_id: MemoryId,
    pub dependency_schema_id: SchemaId,
    pub reason: String,
}

#[async_trait::async_trait]
pub trait DependencySatisfactionRule: std::fmt::Debug + Send + Sync {
    fn target_schema_id(&self) -> &'static str;

    async fn is_satisfied(
        &self,
        storage: &dyn Storage,
        owner: &Owner,
        dependency_memory_id: MemoryId,
    ) -> Result<bool, StorageError>;
}

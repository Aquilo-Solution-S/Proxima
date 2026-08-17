//! Cold object store for forget/hydrate (UML §5c).

use crate::StorageError;

/// One object per Memory `t` under `cold/<owner_hash>/<handle>/<t>`.
#[async_trait::async_trait]
pub trait ColdObjectStore: Send + Sync {
    async fn put(&self, key: &str, bytes: &[u8]) -> Result<(), StorageError>;
    async fn get(&self, key: &str) -> Result<Vec<u8>, StorageError>;
    async fn delete(&self, key: &str) -> Result<(), StorageError>;
}

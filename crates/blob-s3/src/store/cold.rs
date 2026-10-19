//! S3 adapter for cold Memory objects.

use proxima_core::{ColdObjectStore, ObjectStoreId, PurgeDeleted, StorageError};

use super::CitedBlobStore;

/// PUT/GET/DELETE under the store's blob bucket.
#[derive(Debug, Clone)]
pub struct S3ColdStore {
    store: CitedBlobStore,
}

impl CitedBlobStore {
    #[must_use]
    pub fn cold_store(&self) -> S3ColdStore {
        S3ColdStore {
            store: self.clone(),
        }
    }
}

#[async_trait::async_trait]
impl ColdObjectStore for S3ColdStore {
    /// Exactly what `blob_uploads.object_store` records and what the durable
    /// purge queue matches a debt against.
    fn store_id(&self) -> &ObjectStoreId {
        self.store.store_id()
    }

    async fn delete(&self, key: &str) -> Result<PurgeDeleted, StorageError> {
        let client = self
            .store
            .client()
            .await
            .map_err(|err| StorageError::Unavailable(err.to_string()))?;
        let versions = super::erase::purge_exact_key(client, self.store.bucket(), key).await?;
        Ok(PurgeDeleted::from_versions(versions))
    }
}

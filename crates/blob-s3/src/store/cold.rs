//! S3 adapter for UML §5c cold objects.

use aws_sdk_s3::primitives::ByteStream;
use proxima_core::{ColdObjectStore, StorageError};

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
    async fn put(&self, key: &str, bytes: &[u8]) -> Result<(), StorageError> {
        let client = self
            .store
            .client()
            .await
            .map_err(|err| StorageError::Unavailable(err.to_string()))?;
        client
            .put_object()
            .bucket(self.store.bucket())
            .key(key)
            .body(ByteStream::from(bytes.to_vec()))
            .send()
            .await
            .map_err(|err| StorageError::Unavailable(format!("cold put {key}: {err}")))?;
        Ok(())
    }

    async fn get(&self, key: &str) -> Result<Vec<u8>, StorageError> {
        let client = self
            .store
            .client()
            .await
            .map_err(|err| StorageError::Unavailable(err.to_string()))?;
        let object = client
            .get_object()
            .bucket(self.store.bucket())
            .key(key)
            .send()
            .await
            .map_err(|err| StorageError::Unavailable(format!("cold get {key}: {err}")))?;
        object
            .body
            .collect()
            .await
            .map(|data| data.into_bytes().to_vec())
            .map_err(|err| StorageError::Unavailable(format!("cold read {key}: {err}")))
    }

    async fn delete(&self, key: &str) -> Result<(), StorageError> {
        let client = self
            .store
            .client()
            .await
            .map_err(|err| StorageError::Unavailable(err.to_string()))?;
        client
            .delete_object()
            .bucket(self.store.bucket())
            .key(key)
            .send()
            .await
            .map_err(|err| StorageError::Unavailable(format!("cold delete {key}: {err}")))?;
        Ok(())
    }
}

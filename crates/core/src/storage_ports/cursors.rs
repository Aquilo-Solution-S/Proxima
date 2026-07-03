use crate::storage::StorageError;
use crate::storage_ports::OwnerWritePermit;
use crate::{Cursor, Owner};
use std::time::Duration;

#[async_trait::async_trait]
pub trait SourceCursorPort: Send + Sync {
    async fn load_source_cursor(
        &self,
        owner: &Owner,
        source: &str,
    ) -> Result<Option<Cursor>, StorageError>;

    async fn store_source_cursor(
        &self,
        permit: &OwnerWritePermit,
        source: &str,
        cursor: &Cursor,
    ) -> Result<(), StorageError>;

    async fn source_cursor_age(
        &self,
        owner: &Owner,
        source: &str,
    ) -> Result<Option<Duration>, StorageError>;
}

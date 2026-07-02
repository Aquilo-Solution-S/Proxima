use crate::storage::StorageError;
use crate::{Cursor, Owner};

#[async_trait::async_trait]
pub trait SourceCursorPort: Send + Sync {
    async fn load_source_cursor(
        &self,
        owner: &Owner,
        source: &str,
    ) -> Result<Option<Cursor>, StorageError>;

    async fn store_source_cursor(
        &self,
        owner: &Owner,
        source: &str,
        cursor: &Cursor,
    ) -> Result<(), StorageError>;
}

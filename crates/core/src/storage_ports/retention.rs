use crate::storage::StorageError;
use crate::storage_ports::OwnerWritePermit;
use crate::{KeepDays, OwnerId, Retention, SchemaId};

/// The `retention` rows: one per owner and schema (docs/data-and-erasure.md).
#[async_trait::async_trait]
pub trait RetentionPort: Send + Sync {
    /// Set the permit owner's row for `schema` to `keep_days`, or remove it
    /// with `None`.
    async fn set_retention(
        &self,
        permit: &OwnerWritePermit,
        schema: &SchemaId,
        keep_days: Option<KeepDays>,
    ) -> Result<(), StorageError>;

    /// The rows of `owners`, read under the caller's owner scope, ordered by
    /// owner and schema.
    async fn owner_retention(
        &self,
        owner_scope: Option<&crate::OwnerScope>,
        owners: &[OwnerId],
    ) -> Result<Vec<Retention>, StorageError>;

    /// Every row of every owner, ordered by owner and schema. The engine
    /// admits it only under the host's system authority.
    async fn all_retention(&self) -> Result<Vec<Retention>, StorageError>;
}

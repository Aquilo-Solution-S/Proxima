//! Backend-owned write session: one transaction, several Engine writes.

use crate::storage::{AuthorDerivedOutcome, AuthorDerivedRequest, StorageError};
use crate::storage_ports::OwnerWritePermit;
use crate::verbs::fact_ingest::{AuthorizedFactWrite, FactIngestOutcome};
use crate::{MemoryId, SidecarPayload};

/// Opens a backend-owned write session (one transaction).
#[async_trait::async_trait]
pub trait WriteSessionFactory: Send + Sync {
    /// Begin a transaction. Drop without [`WriteSession::commit`] rolls back.
    async fn begin(&self) -> Result<Box<dyn WriteSession>, StorageError>;
}

/// One transaction the Engine can attach several authorized writes to.
#[async_trait::async_trait]
pub trait WriteSession: Send {
    async fn advisory_xact_lock(&mut self, key: i64) -> Result<(), StorageError>;

    async fn ingest_fact_with_typed_sidecar(
        &mut self,
        authorized: &AuthorizedFactWrite,
        sidecar_payloads: &[SidecarPayload],
        embedding_model_id: Option<&str>,
    ) -> Result<FactIngestOutcome, StorageError>;

    async fn author_derived(
        &mut self,
        req: &AuthorDerivedRequest<'_>,
        permit: &OwnerWritePermit,
    ) -> Result<AuthorDerivedOutcome, StorageError>;

    async fn forget_memory(
        &mut self,
        permit: &OwnerWritePermit,
        memory_id: MemoryId,
    ) -> Result<(), StorageError>;

    async fn commit(self: Box<Self>) -> Result<(), StorageError>;
}

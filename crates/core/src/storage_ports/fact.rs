use crate::SidecarPayload;
use crate::SourceBatchId;
use crate::storage::StorageError;
use crate::storage_ports::OwnerWritePermit;
use crate::verbs::close_batch::CloseBatchOutcome;
use crate::verbs::fact_ingest::{
    AuthorizedFactWithCitation, AuthorizedFactWithCitationRef, AuthorizedFactWrite,
    FactIngestOutcome, FactWriteCommand,
};

#[async_trait::async_trait]
pub trait FactIngestPort: Send + Sync {
    async fn ingest_fact_atomic(
        &self,
        permit: &OwnerWritePermit,
        draft: &FactWriteCommand,
        embedding_model_id: Option<&str>,
    ) -> Result<FactIngestOutcome, StorageError>;

    async fn ingest_fact_with_typed_sidecar(
        &self,
        authorized: &AuthorizedFactWrite,
        sidecar_payload: &SidecarPayload,
        embedding_model_id: Option<&str>,
    ) -> Result<FactIngestOutcome, StorageError>;

    async fn ingest_fact_with_citation_and_typed_sidecar(
        &self,
        authorized: &AuthorizedFactWithCitation,
        sidecar_payload: &SidecarPayload,
        embedding_model_id: Option<&str>,
    ) -> Result<FactIngestOutcome, StorageError>;

    /// The by-ref twin of
    /// [`Self::ingest_fact_with_citation_and_typed_sidecar`]: the cited
    /// object already exists; storage must verify it (existence, owner,
    /// schema against `expected_object_schema`) in the same transaction
    /// that writes the citation mapping.
    async fn ingest_fact_with_citation_ref_and_typed_sidecar(
        &self,
        authorized: &AuthorizedFactWithCitationRef,
        sidecar_payload: &SidecarPayload,
        embedding_model_id: Option<&str>,
    ) -> Result<FactIngestOutcome, StorageError>;
}

#[async_trait::async_trait]
pub trait SourceBatchPort: Send + Sync {
    async fn close_batch(
        &self,
        permit: &OwnerWritePermit,
        source_batch_id: SourceBatchId,
    ) -> Result<CloseBatchOutcome, StorageError>;
}

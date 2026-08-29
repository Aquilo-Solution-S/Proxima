use crate::SidecarPayload;

use crate::storage::StorageError;
#[cfg(any(test, feature = "test-fixtures"))]
use crate::storage_ports::OwnerWritePermit;
#[cfg(any(test, feature = "test-fixtures"))]
use crate::verbs::fact_ingest::FactWriteCommand;
use crate::verbs::fact_ingest::{
    AuthorizedFactWithCitation, AuthorizedFactWithCitationRef, AuthorizedFactWrite,
    FactIngestOutcome,
};

#[async_trait::async_trait]
pub trait FactIngestPort: Send + Sync {
    async fn ingest_authorized_fact_atomic(
        &self,
        authorized: &AuthorizedFactWrite,
        embedding_model_id: Option<&str>,
    ) -> Result<FactIngestOutcome, StorageError>;

    /// Compatibility adapter for storage fixtures. Production callers must
    /// use the Engine-minted authorized carrier above.
    #[cfg(any(test, feature = "test-fixtures"))]
    async fn ingest_fact_atomic(
        &self,
        permit: &OwnerWritePermit,
        draft: &FactWriteCommand,
        embedding_model_id: Option<&str>,
    ) -> Result<FactIngestOutcome, StorageError> {
        let permit = OwnerWritePermit::new_for_tests(*permit.owner(), permit.access_kind());
        let authorized =
            AuthorizedFactWrite::new_for_tests(permit, draft.clone(), None, Vec::new());
        self.ingest_authorized_fact_atomic(&authorized, embedding_model_id)
            .await
    }

    /// Persist an authorized Fact together with every typed sidecar row it
    /// carries, in ONE transaction.
    ///
    /// A slice rather than a single payload because a Fact may be extended:
    /// the substrate owns the Fact and its own sidecar, and a flavor may add
    /// further rows of its own against the same `memory_id` — extra columns
    /// on an event the substrate defines, without the flavor having to own
    /// the event.
    ///
    /// **Destination is resolved per payload, never positionally.** Storage
    /// routes each payload by its own `(kind, schema_id, schema_version)`
    /// through the sidecar registry, and an unregistered schema is a
    /// `ConstraintViolation`. Two payloads therefore cannot be transposed
    /// into each other's tables by an ordering slip, and a flavor cannot
    /// name a destination it has not registered. The slice's ORDER is the
    /// insert order and nothing else.
    ///
    /// **All or nothing.** Every payload lands in the same transaction as
    /// the Fact row; one failure rolls back the Fact too. This is the whole
    /// reason extension is expressed as data handed to storage rather than
    /// as a callback invoked mid-transaction: storage keeps sole authority
    /// over the transaction, so an extension can only ADD rows it has
    /// registered — it cannot reach the Fact row, and there is no handle to
    /// misuse.
    ///
    /// Payloads are DATA, not closures, so the bounded retry around
    /// begin→body→commit can rebuild the work on every attempt.
    ///
    /// Note what is NOT widened: `AuthorizedFactWrite::fact_sidecar_table`
    /// and its natural-key columns still come from the Fact's OWN registered
    /// schema, because a stateful Fact's identity belongs to the event, not
    /// to whatever a flavor chose to staple onto it.
    ///
    /// # Errors
    ///
    /// `ConstraintViolation` when any payload's schema is not registered as
    /// a memory sidecar; otherwise storage faults.
    async fn ingest_fact_with_typed_sidecar(
        &self,
        authorized: &AuthorizedFactWrite,
        sidecar_payloads: &[SidecarPayload],
        embedding_model_id: Option<&str>,
    ) -> Result<FactIngestOutcome, StorageError>;

    async fn ingest_fact_with_citation_and_typed_sidecar(
        &self,
        authorized: &AuthorizedFactWithCitation,
        sidecar_payloads: &[SidecarPayload],
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
        sidecar_payloads: &[SidecarPayload],
        embedding_model_id: Option<&str>,
    ) -> Result<FactIngestOutcome, StorageError>;
}

pub trait SourceBatchPort: Send + Sync {}

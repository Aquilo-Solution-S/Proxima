//! Narrow storage ports consumed by engine components.
//!
//! The port split keeps storage DTOs in [`crate::storage`] while making
//! each engine path depend on only the capability it needs.

mod access;
mod bundle;
mod cited_blob;
mod cited_blob_read;
mod cited_blob_reconcile;
mod cursors;
mod delegated_authority;
mod embeddings;
mod fact;
mod goals;
mod handles;
mod host_state;
mod host_state_lifecycle;
mod memory;
pub mod other_stores;
mod owner_inverse;
mod proof;
pub mod publication;
mod rejecting;
mod vector_index;
mod write_session;

pub use access::{OwnerAccessReadPort, OwnerMembershipAdminPort, OwnerTransferPort};
pub(crate) use bundle::{
    EngineStoragePorts, GoalCommandStoragePorts, QueryStoragePorts, ReadVerbStoragePorts,
};
pub use bundle::{StoragePorts, StoragePortsBuildError, StoragePortsBuilder};
pub use cited_blob::{
    CitedBlobHeld, CitedBlobPort, CitedBlobReadUrl, CitedBlobService, CitedBlobStaged,
    CitedBlobUploadAborted, CitedBlobUploadCompleted, CitedBlobUploadHeader,
    CitedBlobUploadPrepared, MAX_HELD_BLOB_DIGESTS,
};
pub use cited_blob_read::{
    CitedBlobIntegrityMismatch, CitedBlobReadError, CitedBlobReadPort, CitedBlobReadService,
    VerifiedCitedBlob,
};
pub use cited_blob_reconcile::{
    CitedBlobMissingObject, CitedBlobOwnerMissingObject, CitedBlobOwnerReconcileOutcome,
    CitedBlobOwnerReconcilePort, CitedBlobOwnerReconcileService, CitedBlobReconcileOutcome,
    CitedBlobReconcilePort, MAX_RECONCILE_SAMPLE,
};
pub use cursors::SourceCursorPort;
pub use delegated_authority::{
    DelegatedAuthorityError, DelegatedAuthorityService, DelegatedCommand, DelegationGrant,
    DelegationGrantStorage, DelegationId, DelegationIssued, DelegationMutationPermit,
    DelegationRevocation, DelegationStorePort,
};
pub use embeddings::{
    EMBEDDING_RECONCILE_DEFAULT_LIMIT, EmbeddingAnnObservability, EmbeddingJobBacklog,
    EmbeddingJobDiagnostic, EmbeddingJobPort, EmbeddingJobState, EmbeddingJobStatusCounts,
    EmbeddingMaintenancePort, EmbeddingOrphanCounts, EmbeddingOrphanSweepOutcome,
    EmbeddingPurgeOutcome, EmbeddingRecallCanary, EmbeddingReconcileOptions,
    EmbeddingReconcileOutcome, EmbeddingReconcileScope, EmbeddingSpaceCounts,
    EmbeddingSpaceCoverage, EmbeddingSpaceRole, EmbeddingTextPort, EmbeddingWriteOutcome,
    EmbeddingWritePort, EmbeddingWriteProof, MAX_EMBEDDING_DIAGNOSTIC_UNITS,
    check_diagnostic_unit_count,
};
pub use fact::FactIngestPort;
pub use goals::{GoalReadPort, GoalWritePort};
pub use handles::{
    CitationHandle, EmbeddingJobHandle, EmbeddingMaintenanceHandle, EmbeddingTextHandle,
    EmbeddingWriteHandle, FactIngestHandle, GoalReadHandle, GoalWriteHandle, MemoryAuthoringHandle,
    MemoryInspectHandle, MemoryReadHandle, OwnerAccessReadHandle, OwnerDropProofHandle,
    OwnerEraseAuthorityHandle, OwnerInverseHandle, OwnerMembershipAdminHandle, OwnerTransferHandle,
    SourceCursorHandle, WriteSessionFactoryHandle,
};
pub use host_state::{
    HostStateCommand, HostStateOutcome, HostStateParticipantDescriptor, HostStateParticipantId,
    HostStatePayloadOwners, HostStateReply, HostStateReplyKind, HostStateRequest, StateSurfaceName,
};
pub use host_state_lifecycle::{
    HostStateEraseReceipt, HostStateEraseRequest, HostStateEraseScope, HostStateEraseSelection,
    HostStateEraseTableCount, HostStateExportReceipt, HostStateExportRequest, HostStateExportTable,
    HostStateFactCopyLocator,
};
pub use memory::{
    CitationPort, InboundPinQuery, MemoryAuthoringPort, MemoryInspectPort, MemoryReadPort,
    OperatorWriteProof, PinTargetKind,
};
pub use owner_inverse::{OwnerDropProofPort, OwnerEraseAuthorityPort, OwnerInversePort};
pub use proof::{
    HostStateWriteOrigin, HostStateWritePermit, MembershipEndpointAuthority,
    MembershipMemberAuthority, MembershipWritePermit, OperatorMaintenanceProof, OwnerWritePermit,
};
pub use publication::{
    AckOutcome, ClaimToken, ClaimedPublication, PublicationOutboxPort, PublisherId,
    PublisherIdError, ReleaseOutcome,
};
pub use vector_index::{InMemoryVectorIndex, VectorCandidate, VectorEntry, VectorIndex, VectorKey};
pub use write_session::{
    SIDECAR_SESSION_READ_MAX_ROWS, SidecarSessionRead, WriteSession, WriteSessionFactory,
};

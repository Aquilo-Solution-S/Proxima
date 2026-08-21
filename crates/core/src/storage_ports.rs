//! Narrow storage ports consumed by engine components.
//!
//! The port split keeps storage DTOs in [`crate::storage`] while making
//! each engine path depend on only the capability it needs.

mod access;
mod bundle;
mod change;
mod cited_blob;
mod cited_blob_read;
mod cited_blob_reconcile;
mod cited_object_erase;
mod compliance;
mod cursors;
mod delegated_authority;
mod embeddings;
mod fact;
mod goals;
mod handles;
mod mcp;
mod memory;
mod proof;
mod registry;
mod rejecting;
mod write_session;

pub use access::{OwnerAccessReadPort, OwnerMembershipAdminPort, OwnerTransferPort};
pub(crate) use bundle::{
    EngineStoragePorts, GoalCommandStoragePorts, QueryStoragePorts, ReadVerbStoragePorts,
};
pub use bundle::{StoragePorts, StoragePortsBuildError, StoragePortsBuilder};
pub use change::ChangeEventPort;
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
pub use cited_object_erase::CitedObjectErasePort;
pub use compliance::{ComplianceAdminPort, ComplianceErasePort, OwnerDropProofPort};
pub use cursors::SourceCursorPort;
pub use delegated_authority::{
    DelegatedAuthorityError, DelegatedAuthorityService, DelegatedCommand, DelegationGrant,
    DelegationGrantStorage, DelegationId, DelegationIssued, DelegationMutationPermit,
    DelegationRevocation, DelegationStorePort,
};
pub use embeddings::{
    EMBEDDING_RECONCILE_DEFAULT_LIMIT, EmbeddingAnnObservability, EmbeddingJobBacklog,
    EmbeddingJobPort, EmbeddingJobStatusCounts, EmbeddingMaintenancePort, EmbeddingOrphanCounts,
    EmbeddingOrphanSweepOutcome, EmbeddingRecallCanary, EmbeddingReconcileOptions,
    EmbeddingReconcileOutcome, EmbeddingReconcileScope, EmbeddingTextPort, EmbeddingWriteOutcome,
    EmbeddingWritePort, EmbeddingWriteProof,
};
pub use fact::{FactIngestPort, SourceBatchPort};
pub use goals::{GoalReadPort, GoalWakeCandidatePort, GoalWritePort};
pub use handles::{
    ChangeEventHandle, CitationHandle, ComplianceAdminHandle, ComplianceEraseHandle,
    EmbeddingJobHandle, EmbeddingMaintenanceHandle, EmbeddingTextHandle, EmbeddingWriteHandle,
    FactIngestHandle, GoalReadHandle, GoalWakeCandidateHandle, GoalWriteHandle, McpCallReadHandle,
    McpCallWriteHandle, MemoryAuthoringHandle, MemoryInspectHandle, MemoryReadHandle,
    OwnerAccessReadHandle, OwnerDropProofHandle, OwnerMembershipAdminHandle, OwnerTransferHandle,
    RegistryProjectionHandle, SourceBatchHandle, SourceCursorHandle, WriteSessionFactoryHandle,
};
pub use mcp::{McpCallReadPort, McpCallWritePort};
pub use memory::{
    CitationPort, InboundPinQuery, MemoryAuthoringPort, MemoryInspectPort, MemoryReadPort,
    OperatorWriteProof,
};
pub use proof::{OperatorMaintenanceProof, OwnerWritePermit};
pub use registry::RegistryProjectionPort;
pub use write_session::{WriteSession, WriteSessionFactory};

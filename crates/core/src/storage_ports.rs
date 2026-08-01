//! Narrow storage ports consumed by engine components.
//!
//! The port split keeps storage DTOs in [`crate::storage`] while making
//! each engine path depend on only the capability it needs.

mod access;
mod bundle;
mod change;
mod cited_blob;
mod cited_object_erase;
mod compliance;
mod cursors;
mod embeddings;
mod fact;
mod goals;
mod handles;
mod mcp;
mod memory;
mod proof;
mod registry;
mod rejecting;

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
pub use cited_object_erase::CitedObjectErasePort;
pub use compliance::{
    ComplianceAdminPort, ComplianceErasePort, FactRetentionPort, OwnerDropProofPort,
};
pub use cursors::SourceCursorPort;
pub use embeddings::{
    EmbeddingAnnObservability, EmbeddingJobBacklog, EmbeddingJobPort, EmbeddingJobStatusCounts,
    EmbeddingMaintenancePort, EmbeddingOrphanCounts, EmbeddingOrphanSweepOutcome,
    EmbeddingRecallCanary, EmbeddingReconcileOptions, EmbeddingReconcileOutcome,
    EmbeddingReconcileScope, EmbeddingTextPort, EmbeddingWriteOutcome, EmbeddingWritePort,
    EmbeddingWriteProof, PERMANENT_EMBED_FAILURE_MARKER,
};
pub use fact::{FactIngestPort, SourceBatchPort};
pub use goals::{GoalReadPort, GoalWakeCandidatePort, GoalWritePort};
pub use handles::{
    ChangeEventHandle, CitationHandle, ComplianceAdminHandle, ComplianceEraseHandle,
    EdgeReadHandle, EmbeddingJobHandle, EmbeddingMaintenanceHandle, EmbeddingTextHandle,
    EmbeddingWriteHandle, FactIngestHandle, FactRetentionHandle, GoalReadHandle,
    GoalWakeCandidateHandle, GoalWriteHandle, McpCallReadHandle, McpCallWriteHandle,
    MemoryAuthoringHandle, MemoryInspectHandle, MemoryReadHandle, OwnerAccessReadHandle,
    OwnerDropProofHandle, OwnerMembershipAdminHandle, OwnerTransferHandle,
    RegistryProjectionHandle, SourceBatchHandle, SourceCursorHandle,
};
pub use mcp::{McpCallReadPort, McpCallWritePort};
pub use memory::{
    CitationPort, EdgeReadPort, MemoryAuthoringPort, MemoryInspectPort, MemoryReadPort,
    OperatorWriteProof,
};
pub use proof::{OperatorMaintenanceProof, OwnerWritePermit};
pub use registry::RegistryProjectionPort;

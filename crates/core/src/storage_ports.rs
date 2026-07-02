//! Narrow storage ports consumed by engine components.
//!
//! The port split keeps storage DTOs in [`crate::storage`] while making
//! each engine path depend on only the capability it needs.

mod access;
mod bundle;
mod change;
mod compliance;
mod embeddings;
mod fact;
mod goals;
mod handles;
mod mcp;
mod memory;
mod proof;
mod registry;
mod rejecting;

pub use access::{OwnerAccessReadPort, OwnerMembershipAdminPort};
pub(crate) use bundle::{
    EngineStoragePorts, GoalCommandStoragePorts, QueryStoragePorts, ReadVerbStoragePorts,
};
pub use bundle::{StoragePorts, StoragePortsBuilder};
pub use change::ChangeEventPort;
pub use compliance::{
    ComplianceAdminPort, ComplianceErasePort, FactRetentionPort, OwnerDropProofPort,
};
pub use embeddings::{
    EmbeddingJobPort, EmbeddingTextPort, EmbeddingWriteOutcome, EmbeddingWritePort,
    EmbeddingWriteProof,
};
pub use fact::{FactIngestPort, SourceBatchPort};
pub use goals::{GoalReadPort, GoalWakeCandidatePort, GoalWritePort};
pub use handles::{
    ChangeEventHandle, CitationHandle, ComplianceAdminHandle, ComplianceEraseHandle,
    EdgeReadHandle, EmbeddingJobHandle, EmbeddingTextHandle, EmbeddingWriteHandle,
    FactIngestHandle, FactRetentionHandle, GoalReadHandle, GoalWriteHandle, McpCallReadHandle,
    McpCallWriteHandle, MemoryAuthoringHandle, MemoryInspectHandle, MemoryReadHandle,
    OwnerAccessReadHandle, OwnerDropProofHandle, OwnerMembershipAdminHandle,
    RegistryProjectionHandle, SourceBatchHandle,
};
pub use mcp::{McpCallReadPort, McpCallWritePort};
pub use memory::{
    CitationPort, EdgeReadPort, EdgeWriteProof, MemoryAuthoringPort, MemoryInspectPort,
    MemoryReadPort, OperatorWriteProof,
};
pub use registry::RegistryProjectionPort;

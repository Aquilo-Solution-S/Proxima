use std::sync::Arc;

use super::access::{OwnerAccessReadPort, OwnerMembershipAdminPort, OwnerTransferPort};
use super::change::ChangeEventPort;
use super::compliance::{
    ComplianceAdminPort, ComplianceErasePort, FactRetentionPort, OwnerDropProofPort,
};
use super::cursors::SourceCursorPort;
use super::embeddings::{EmbeddingJobPort, EmbeddingTextPort, EmbeddingWritePort};
use super::fact::{FactIngestPort, SourceBatchPort};
use super::goals::{GoalReadPort, GoalWritePort};
use super::mcp::{McpCallReadPort, McpCallWritePort};
use super::memory::{
    CitationPort, EdgeReadPort, MemoryAuthoringPort, MemoryInspectPort, MemoryReadPort,
};
use super::registry::RegistryProjectionPort;

pub type FactIngestHandle = Arc<dyn FactIngestPort>;
pub type McpCallWriteHandle = Arc<dyn McpCallWritePort>;
pub type McpCallReadHandle = Arc<dyn McpCallReadPort>;
pub type MemoryAuthoringHandle = Arc<dyn MemoryAuthoringPort>;
pub type MemoryReadHandle = Arc<dyn MemoryReadPort>;
pub type MemoryInspectHandle = Arc<dyn MemoryInspectPort>;
pub type EmbeddingTextHandle = Arc<dyn EmbeddingTextPort>;
pub type EmbeddingWriteHandle = Arc<dyn EmbeddingWritePort>;
pub type EmbeddingJobHandle = Arc<dyn EmbeddingJobPort>;
pub type GoalWriteHandle = Arc<dyn GoalWritePort>;
pub type GoalReadHandle = Arc<dyn GoalReadPort>;
pub type ChangeEventHandle = Arc<dyn ChangeEventPort>;
pub type EdgeReadHandle = Arc<dyn EdgeReadPort>;
pub type CitationHandle = Arc<dyn CitationPort>;
pub type OwnerAccessReadHandle = Arc<dyn OwnerAccessReadPort>;
pub type OwnerMembershipAdminHandle = Arc<dyn OwnerMembershipAdminPort>;
pub type OwnerTransferHandle = Arc<dyn OwnerTransferPort>;
pub type SourceBatchHandle = Arc<dyn SourceBatchPort>;
pub type SourceCursorHandle = Arc<dyn SourceCursorPort>;
pub type FactRetentionHandle = Arc<dyn FactRetentionPort>;
pub type ComplianceEraseHandle = Arc<dyn ComplianceErasePort>;
pub type RegistryProjectionHandle = Arc<dyn RegistryProjectionPort>;
pub type ComplianceAdminHandle = Arc<dyn ComplianceAdminPort>;
pub type OwnerDropProofHandle = Arc<dyn OwnerDropProofPort>;

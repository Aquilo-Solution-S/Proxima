use std::fmt;
use std::sync::Arc;

use super::handles::{
    ChangeEventHandle, CitationHandle, EmbeddingJobHandle, EmbeddingMaintenanceHandle,
    EmbeddingTextHandle, EmbeddingWriteHandle, FactIngestHandle, GoalReadHandle,
    GoalWakeCandidateHandle, GoalWriteHandle, McpCallReadHandle, MemoryAuthoringHandle,
    MemoryInspectHandle, MemoryReadHandle, OwnerAccessReadHandle, OwnerDropProofHandle,
    OwnerEraseAuthorityHandle, OwnerInverseHandle, OwnerMembershipAdminHandle, OwnerTransferHandle,
    RegistryProjectionHandle, SourceBatchHandle, SourceCursorHandle, WriteSessionFactoryHandle,
};
use super::rejecting::RejectingStorage;

#[allow(dead_code)]
#[derive(Clone)]
pub struct StoragePorts {
    fact_ingest: FactIngestHandle,
    mcp_call_read: McpCallReadHandle,
    memory_authoring: MemoryAuthoringHandle,
    memory_read: MemoryReadHandle,
    memory_inspect: MemoryInspectHandle,
    embedding_text: EmbeddingTextHandle,
    embedding_write: EmbeddingWriteHandle,
    embedding_job: EmbeddingJobHandle,
    embedding_maintenance: EmbeddingMaintenanceHandle,
    goal_write: GoalWriteHandle,
    goal_read: GoalReadHandle,
    goal_wake_candidate: GoalWakeCandidateHandle,
    change_event: ChangeEventHandle,
    citation: CitationHandle,
    owner_access_read: OwnerAccessReadHandle,
    owner_membership_admin: OwnerMembershipAdminHandle,
    owner_transfer: OwnerTransferHandle,
    #[allow(dead_code)]
    source_batch: SourceBatchHandle,
    source_cursor: SourceCursorHandle,
    owner_erase: OwnerInverseHandle,
    erase_authority: Option<OwnerEraseAuthorityHandle>,
    owner_drop_proof: Option<OwnerDropProofHandle>,
    registry_projection: RegistryProjectionHandle,
    write_session: WriteSessionFactoryHandle,
}

#[derive(Clone)]
#[allow(clippy::struct_field_names)] // all three ports are owner-* by domain, not incidental naming
pub(crate) struct AccessAdminStoragePorts {
    pub owner_membership_admin: OwnerMembershipAdminHandle,
    pub owner_access_read: OwnerAccessReadHandle,
    pub owner_transfer: OwnerTransferHandle,
}

#[derive(Clone)]
pub(crate) struct SourceCursorStoragePorts {
    pub source_cursor: SourceCursorHandle,
}

#[derive(Clone)]
pub(crate) struct GoalCommandStoragePorts {
    pub goal_write: GoalWriteHandle,
    pub owner_access_read: OwnerAccessReadHandle,
}

#[derive(Clone)]
pub(crate) struct IngestStoragePorts {
    pub fact_ingest: FactIngestHandle,
    pub embedding_text: EmbeddingTextHandle,
    pub embedding_write: EmbeddingWriteHandle,
    pub embedding_job: EmbeddingJobHandle,
    #[allow(dead_code)]
    pub source_batch: SourceBatchHandle,
}

#[derive(Clone)]
pub(crate) struct MemoryAuthoringStoragePorts {
    pub memory_authoring: MemoryAuthoringHandle,
    pub owner_access_read: OwnerAccessReadHandle,
}

#[derive(Clone)]
pub(crate) struct PipelineStoragePorts {
    pub owner_access_read: OwnerAccessReadHandle,
}

#[derive(Clone)]
pub(crate) struct QueryStoragePorts {
    pub change_event: ChangeEventHandle,
    pub mcp_call_read: McpCallReadHandle,
    pub memory_read: MemoryReadHandle,
}

#[derive(Clone)]
pub(crate) struct ReadVerbStoragePorts {
    pub embedding_job: EmbeddingJobHandle,
    pub memory_read: MemoryReadHandle,
    pub memory_inspect: MemoryInspectHandle,
    pub change_event: ChangeEventHandle,
    pub citation: CitationHandle,
    pub goal_wake_candidate: GoalWakeCandidateHandle,
    pub goal_read: GoalReadHandle,
}

#[derive(Clone)]
#[allow(dead_code)]
pub(crate) struct OwnerInverseStoragePorts {
    pub owner_erase: OwnerInverseHandle,
    pub erase_authority: Option<OwnerEraseAuthorityHandle>,
    pub owner_drop_proof: Option<OwnerDropProofHandle>,
    pub embedding_maintenance: EmbeddingMaintenanceHandle,
}

#[derive(Clone)]
pub(crate) struct EngineStoragePorts {
    pub access_admin: AccessAdminStoragePorts,
    pub owner_inverse: OwnerInverseStoragePorts,
    pub source_cursor: SourceCursorStoragePorts,
    pub goal_command: GoalCommandStoragePorts,
    pub ingest: IngestStoragePorts,
    pub memory_authoring: MemoryAuthoringStoragePorts,
    pub pipeline: PipelineStoragePorts,
    pub query: QueryStoragePorts,
    pub read_verb: ReadVerbStoragePorts,
    pub write_session: WriteSessionFactoryHandle,
}

#[derive(Default)]
pub struct StoragePortsBuilder {
    fact_ingest: Option<FactIngestHandle>,
    mcp_call_read: Option<McpCallReadHandle>,
    memory_authoring: Option<MemoryAuthoringHandle>,
    memory_read: Option<MemoryReadHandle>,
    memory_inspect: Option<MemoryInspectHandle>,
    embedding_text: Option<EmbeddingTextHandle>,
    embedding_write: Option<EmbeddingWriteHandle>,
    embedding_job: Option<EmbeddingJobHandle>,
    embedding_maintenance: Option<EmbeddingMaintenanceHandle>,
    goal_write: Option<GoalWriteHandle>,
    goal_read: Option<GoalReadHandle>,
    goal_wake_candidate: Option<GoalWakeCandidateHandle>,
    change_event: Option<ChangeEventHandle>,
    citation: Option<CitationHandle>,
    owner_access_read: Option<OwnerAccessReadHandle>,
    owner_membership_admin: Option<OwnerMembershipAdminHandle>,
    owner_transfer: Option<OwnerTransferHandle>,
    source_batch: Option<SourceBatchHandle>,
    source_cursor: Option<SourceCursorHandle>,
    owner_erase: Option<OwnerInverseHandle>,
    erase_authority: Option<OwnerEraseAuthorityHandle>,
    owner_drop_proof: Option<OwnerDropProofHandle>,
    registry_projection: Option<RegistryProjectionHandle>,
    write_session: Option<WriteSessionFactoryHandle>,
}

impl fmt::Debug for StoragePorts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoragePorts").finish_non_exhaustive()
    }
}

impl fmt::Debug for StoragePortsBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoragePortsBuilder")
            .finish_non_exhaustive()
    }
}

impl StoragePorts {
    #[must_use]
    pub fn builder() -> StoragePortsBuilder {
        StoragePortsBuilder::default()
    }

    #[must_use]
    pub(crate) fn rejecting() -> Self {
        let rejecting = Arc::new(RejectingStorage);
        Self {
            fact_ingest: rejecting.clone(),
            mcp_call_read: rejecting.clone(),
            memory_authoring: rejecting.clone(),
            memory_read: rejecting.clone(),
            memory_inspect: rejecting.clone(),
            embedding_text: rejecting.clone(),
            embedding_write: rejecting.clone(),
            embedding_job: rejecting.clone(),
            embedding_maintenance: rejecting.clone(),
            goal_write: rejecting.clone(),
            goal_read: rejecting.clone(),
            goal_wake_candidate: rejecting.clone(),
            change_event: rejecting.clone(),
            citation: rejecting.clone(),
            owner_access_read: rejecting.clone(),
            owner_membership_admin: rejecting.clone(),
            owner_transfer: rejecting.clone(),
            source_batch: rejecting.clone(),
            source_cursor: rejecting.clone(),
            owner_erase: rejecting.clone(),
            erase_authority: None,
            owner_drop_proof: None,
            registry_projection: rejecting.clone(),
            write_session: rejecting,
        }
    }

    /// All-rejecting ports except a caller-supplied `owner_erase` (and an
    /// optional `owner_drop_proof`), so the engine's owner-inverse verbs can be
    /// exercised against a fake erase backend without wiring 20+ ports.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn rejecting_with_owner_inverse(
        owner_erase: OwnerInverseHandle,
        owner_drop_proof: Option<OwnerDropProofHandle>,
    ) -> Self {
        let mut ports = Self::rejecting();
        ports.owner_erase = owner_erase;
        ports.owner_drop_proof = owner_drop_proof;
        ports
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn rejecting_with_erase_authority(
        erase_authority: OwnerEraseAuthorityHandle,
    ) -> Self {
        let mut ports = Self::rejecting();
        ports.erase_authority = Some(erase_authority);
        ports
    }

    /// Rejecting everywhere except the node-write port. Lets a test
    /// observe exactly what a write hands storage — above all which
    /// index rows it asserts — without standing up a database.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn rejecting_with_memory_authoring(memory_authoring: MemoryAuthoringHandle) -> Self {
        let mut ports = Self::rejecting();
        ports.memory_authoring = memory_authoring;
        ports
    }
}

impl From<StoragePorts> for EngineStoragePorts {
    fn from(ports: StoragePorts) -> Self {
        Self {
            access_admin: AccessAdminStoragePorts {
                owner_membership_admin: ports.owner_membership_admin.clone(),
                owner_access_read: ports.owner_access_read.clone(),
                owner_transfer: ports.owner_transfer.clone(),
            },
            owner_inverse: OwnerInverseStoragePorts {
                owner_erase: ports.owner_erase.clone(),
                erase_authority: ports.erase_authority.clone(),
                owner_drop_proof: ports.owner_drop_proof.clone(),
                embedding_maintenance: ports.embedding_maintenance.clone(),
            },
            source_cursor: SourceCursorStoragePorts {
                source_cursor: ports.source_cursor.clone(),
            },
            goal_command: GoalCommandStoragePorts {
                goal_write: ports.goal_write.clone(),
                owner_access_read: ports.owner_access_read.clone(),
            },
            ingest: IngestStoragePorts {
                fact_ingest: ports.fact_ingest.clone(),
                embedding_text: ports.embedding_text.clone(),
                embedding_write: ports.embedding_write.clone(),
                embedding_job: ports.embedding_job.clone(),
                source_batch: ports.source_batch.clone(),
            },
            memory_authoring: MemoryAuthoringStoragePorts {
                memory_authoring: ports.memory_authoring.clone(),
                owner_access_read: ports.owner_access_read.clone(),
            },
            pipeline: PipelineStoragePorts {
                owner_access_read: ports.owner_access_read.clone(),
            },
            query: QueryStoragePorts {
                change_event: ports.change_event.clone(),
                mcp_call_read: ports.mcp_call_read.clone(),
                memory_read: ports.memory_read.clone(),
            },
            read_verb: ReadVerbStoragePorts {
                embedding_job: ports.embedding_job.clone(),
                memory_read: ports.memory_read.clone(),
                memory_inspect: ports.memory_inspect.clone(),
                change_event: ports.change_event.clone(),
                citation: ports.citation.clone(),
                goal_wake_candidate: ports.goal_wake_candidate.clone(),
                goal_read: ports.goal_read.clone(),
            },
            write_session: ports.write_session,
        }
    }
}

impl StoragePortsBuilder {
    #[must_use]
    pub fn fact_ingest(mut self, handle: FactIngestHandle) -> Self {
        self.fact_ingest = Some(handle);
        self
    }

    #[must_use]
    pub fn mcp_call_read(mut self, handle: McpCallReadHandle) -> Self {
        self.mcp_call_read = Some(handle);
        self
    }

    #[must_use]
    pub fn memory_authoring(mut self, handle: MemoryAuthoringHandle) -> Self {
        self.memory_authoring = Some(handle);
        self
    }

    #[must_use]
    pub fn memory_read(mut self, handle: MemoryReadHandle) -> Self {
        self.memory_read = Some(handle);
        self
    }

    #[must_use]
    pub fn memory_inspect(mut self, handle: MemoryInspectHandle) -> Self {
        self.memory_inspect = Some(handle);
        self
    }

    #[must_use]
    pub fn embedding_text(mut self, handle: EmbeddingTextHandle) -> Self {
        self.embedding_text = Some(handle);
        self
    }

    #[must_use]
    pub fn embedding_write(mut self, handle: EmbeddingWriteHandle) -> Self {
        self.embedding_write = Some(handle);
        self
    }

    #[must_use]
    pub fn embedding_job(mut self, handle: EmbeddingJobHandle) -> Self {
        self.embedding_job = Some(handle);
        self
    }

    #[must_use]
    pub fn embedding_maintenance(mut self, handle: EmbeddingMaintenanceHandle) -> Self {
        self.embedding_maintenance = Some(handle);
        self
    }

    #[must_use]
    pub fn goal_write(mut self, handle: GoalWriteHandle) -> Self {
        self.goal_write = Some(handle);
        self
    }

    #[must_use]
    pub fn goal_read(mut self, handle: GoalReadHandle) -> Self {
        self.goal_read = Some(handle);
        self
    }

    #[must_use]
    pub fn goal_wake_candidate(mut self, handle: GoalWakeCandidateHandle) -> Self {
        self.goal_wake_candidate = Some(handle);
        self
    }

    #[must_use]
    pub fn change_event(mut self, handle: ChangeEventHandle) -> Self {
        self.change_event = Some(handle);
        self
    }

    #[must_use]
    pub fn citation(mut self, handle: CitationHandle) -> Self {
        self.citation = Some(handle);
        self
    }

    #[must_use]
    pub fn owner_access_read(mut self, handle: OwnerAccessReadHandle) -> Self {
        self.owner_access_read = Some(handle);
        self
    }

    #[must_use]
    pub fn owner_membership_admin(mut self, handle: OwnerMembershipAdminHandle) -> Self {
        self.owner_membership_admin = Some(handle);
        self
    }

    #[must_use]
    pub fn owner_transfer(mut self, handle: OwnerTransferHandle) -> Self {
        self.owner_transfer = Some(handle);
        self
    }

    #[must_use]
    pub fn source_batch(mut self, handle: SourceBatchHandle) -> Self {
        self.source_batch = Some(handle);
        self
    }

    #[must_use]
    pub fn source_cursor(mut self, handle: SourceCursorHandle) -> Self {
        self.source_cursor = Some(handle);
        self
    }

    #[must_use]
    pub fn owner_erase(mut self, handle: OwnerInverseHandle) -> Self {
        self.owner_erase = Some(handle);
        self
    }

    #[must_use]
    pub fn erase_authority(mut self, handle: OwnerEraseAuthorityHandle) -> Self {
        self.erase_authority = Some(handle);
        self
    }

    #[must_use]
    pub fn owner_drop_proof(mut self, handle: OwnerDropProofHandle) -> Self {
        self.owner_drop_proof = Some(handle);
        self
    }

    #[must_use]
    pub fn registry_projection(mut self, handle: RegistryProjectionHandle) -> Self {
        self.registry_projection = Some(handle);
        self
    }

    #[must_use]
    pub fn write_session(mut self, handle: WriteSessionFactoryHandle) -> Self {
        self.write_session = Some(handle);
        self
    }

    /// Builds a complete storage port bundle, collecting every unconfigured
    /// required port into a typed error instead of panicking on the first one.
    ///
    /// # Errors
    ///
    /// Returns [`StoragePortsBuildError`] naming every required port handle that
    /// was not configured. `erase_authority` and `owner_drop_proof` are
    /// optional and never reported.
    pub fn try_build(self) -> Result<StoragePorts, StoragePortsBuildError> {
        // One list of required ports; the macro expands the missing-name
        // collection, the all-at-once refutable binding (never panics, unlike
        // `build`), and the struct literal from it.
        macro_rules! require_ports {
            ($($port:ident),+ $(,)?) => {{
                let mut missing: Vec<&'static str> = Vec::new();
                $(
                    if self.$port.is_none() {
                        missing.push(stringify!($port));
                    }
                )+
                let ($(Some($port),)+) = ($(self.$port,)+) else {
                    return Err(StoragePortsBuildError { missing });
                };
                StoragePorts {
                    $($port,)+
                    erase_authority: self.erase_authority,
                    owner_drop_proof: self.owner_drop_proof,
                }
            }};
        }

        Ok(require_ports!(
            fact_ingest,
            mcp_call_read,
            memory_authoring,
            memory_read,
            memory_inspect,
            embedding_text,
            embedding_write,
            embedding_job,
            embedding_maintenance,
            goal_write,
            goal_read,
            goal_wake_candidate,
            change_event,
            citation,
            owner_access_read,
            owner_membership_admin,
            owner_transfer,
            source_batch,
            source_cursor,
            owner_erase,
            registry_projection,
            write_session,
        ))
    }

    /// Builds a complete storage port bundle.
    ///
    /// # Panics
    ///
    /// Panics when any required port handle was not configured; the panic names
    /// every missing port. Use [`Self::try_build`] to handle the error instead.
    #[must_use]
    pub fn build(self) -> StoragePorts {
        self.try_build()
            .expect("all required storage ports must be configured")
    }
}

/// Every required storage port that a [`StoragePortsBuilder`] left unconfigured.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("storage ports not fully configured; missing: {}", .missing.join(", "))]
pub struct StoragePortsBuildError {
    missing: Vec<&'static str>,
}

impl StoragePortsBuildError {
    /// Names of the required ports that were not configured.
    #[must_use]
    pub fn missing(&self) -> &[&'static str] {
        &self.missing
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn try_build_reports_every_missing_required_port_by_name() {
        let err = StoragePorts::builder()
            .try_build()
            .expect_err("an empty builder must not build");

        // Names are surfaced both structurally and in the Display message.
        assert!(err.missing().contains(&"fact_ingest"));
        assert!(err.missing().contains(&"registry_projection"));
        assert!(err.to_string().contains("fact_ingest"));

        // Optional ports are never reported as missing.
        assert!(!err.missing().contains(&"erase_authority"));
        assert!(!err.missing().contains(&"owner_drop_proof"));
    }
}

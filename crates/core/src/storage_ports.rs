//! Narrow storage ports consumed by engine components.
//!
//! The port split keeps storage DTOs in [`crate::storage`] while making
//! each engine path depend on only the capability it needs.

use std::fmt;
use std::sync::Arc;

use crate::SourceBatchId;
use crate::dependency::MemoryDependency;
use crate::personality::{
    AbstractionRow, ActiveGoalSummary, ChangeEventForWake, InstantiatePersonalityRequest,
    InstantiatePersonalityResponse, MemorySnapshot, PersonalityInstanceRow, PersonalityRef,
    PersonalityWriteOutcome, PersonalityWriteRequest, SetWakeEntriesRequest,
    SetWakeEntriesResponse, SidecarSpec, TombstonePersonalityRequest, TombstonePersonalityResponse,
};
use crate::storage::{
    AuthorDerivedOutcome, AuthorDerivedRequest, EdgeEndpointKindRow, EmbeddingJobClaim,
    MemoryGraphPayloadRow, MemoryKindRow, NeighborEdgeRow, StorageError, WakeEntriesMutator,
};
use crate::verbs::change_history::{ChangeHistoryRequest, ChangeHistoryResponse};
use crate::verbs::close_batch::CloseBatchOutcome;
use crate::verbs::fact_cleanup::{CleanupDueFactsOutcome, TombstoneFactOutcome};
use crate::verbs::fact_ingest::{
    AuthorizedFactWithCitation, AuthorizedFactWrite, FactIngestOutcome, FactWriteCommand,
};
use crate::verbs::goal_write::{
    AchieveGoalAtomicRequest, CreateGoalAtomicRequest, DecomposeGoalAtomicRequest,
    DecomposeGoalOutcome, GoalWriteOutcome, ModifyGoalAtomicRequest, TransitionGoalAtomicRequest,
};
use crate::verbs::mcp_call_history::{McpCallHistoryRequest, McpCallHistoryResponse};
use crate::verbs::persist_mcp_call::{McpCallLogInput, McpCallLogOutcome};
use crate::{
    DerivedEdgeSpec, EdgeId, EntityId, EntityKind, FactEntityId, GroupId, MasterTokenPersonality,
    MembershipRow, MemoryId, Owner, OwnerRef, PersonalityInstanceId, Relation, SchemaId,
    SchemaVersion, SidecarPayload, UserId,
};

/// Unforgeable witness that engine admission already enforced the relation
/// descriptor's source-owner, owner-policy, and target-access gates before a
/// storage backend performs the atomic edge append.
#[derive(Debug, Clone, Copy)]
pub struct EdgeWriteProof {
    _private: (),
}

impl EdgeWriteProof {
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self { _private: () }
    }
}

#[async_trait::async_trait]
pub trait FactIngestPort: Send + Sync {
    async fn ingest_fact_atomic(
        &self,
        owner: &Owner,
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
}

#[async_trait::async_trait]
pub trait OperatorInvocationWritePort: Send + Sync {
    async fn persist_mcp_call_atomic(
        &self,
        input: &McpCallLogInput,
    ) -> Result<McpCallLogOutcome, StorageError>;
}

#[async_trait::async_trait]
pub trait OperatorInvocationReadPort: Send + Sync {
    async fn read_mcp_call_history(
        &self,
        req: &McpCallHistoryRequest,
    ) -> Result<McpCallHistoryResponse, StorageError>;
}

#[async_trait::async_trait]
pub trait MemoryAuthoringPort: Send + Sync {
    async fn author_derived(
        &self,
        req: &AuthorDerivedRequest<'_>,
    ) -> Result<AuthorDerivedOutcome, StorageError>;

    /// Append one already-authorized memory edge. Public callers cannot forge
    /// `EdgeWriteProof`; route through engine/checked edge-write APIs instead.
    async fn append_memory_edge(
        &self,
        edge: &DerivedEdgeSpec<'_>,
        proof: EdgeWriteProof,
    ) -> Result<EdgeId, StorageError>;

    async fn load_memory_kinds(
        &self,
        _owner: &Owner,
        _memory_ids: &[MemoryId],
    ) -> Result<Vec<MemoryKindRow>, StorageError> {
        Ok(Vec::new())
    }

    async fn load_memory_edge_ids(
        &self,
        _owner: &Owner,
        _relation: &str,
        _source_memory_id: MemoryId,
        _target_memory_ids: &[MemoryId],
    ) -> Result<Vec<EdgeId>, StorageError> {
        Ok(Vec::new())
    }
}

#[async_trait::async_trait]
pub trait MemoryReadPort: Send + Sync {
    async fn load_fact_text(
        &self,
        owner: &Owner,
        memory_id: crate::MemoryId,
    ) -> Result<Option<String>, StorageError>;

    async fn load_memory_graph_payloads(
        &self,
        _owner: &Owner,
        _memory_ids: &[MemoryId],
        _include_body: bool,
    ) -> Result<Vec<MemoryGraphPayloadRow>, StorageError> {
        Ok(Vec::new())
    }

    async fn load_neighbor_memory_edges(
        &self,
        _read_owners: &[OwnerRef],
        _memory_ids: &[MemoryId],
        _limit: usize,
    ) -> Result<Vec<NeighborEdgeRow>, StorageError> {
        Ok(Vec::new())
    }

    async fn load_edge_endpoint_kinds(
        &self,
        _edge_ids: &[EdgeId],
    ) -> Result<Vec<EdgeEndpointKindRow>, StorageError> {
        Ok(Vec::new())
    }

    async fn query_memories(
        &self,
        req: &crate::verbs::query::QueryRequest,
        schemas: &[crate::verbs::schema::SchemaInfo],
    ) -> Result<crate::verbs::query::QueryResponse, StorageError>;

    async fn search_memories(
        &self,
        req: &crate::verbs::query::MemorySearchRequest,
        projections: &[crate::verbs::schema::MemorySearchProjection],
    ) -> Result<Vec<crate::verbs::query::MemorySearchResult>, StorageError>;

    async fn walk_memory_lineage(
        &self,
        read_owners: &[OwnerRef],
        req: &crate::verbs::query::MemoryLineageRequest,
    ) -> Result<crate::verbs::query::MemoryLineageResponse, StorageError>;
}

#[async_trait::async_trait]
pub trait MemoryInspectPort: Send + Sync {
    async fn load_memory_by_id(
        &self,
        memory_id: crate::MemoryId,
        reader_personality_instance_id: Option<PersonalityInstanceId>,
        sidecars: &[SidecarSpec],
    ) -> Result<Option<MemorySnapshot>, StorageError>;

    async fn list_memory_dependencies(
        &self,
        _owner: &Owner,
        _source_memory_id: crate::MemoryId,
    ) -> Result<Vec<MemoryDependency>, StorageError> {
        Ok(Vec::new())
    }
}

#[async_trait::async_trait]
pub trait EmbeddingTextPort: Send + Sync {
    async fn load_embedding_text(
        &self,
        owner: &Owner,
        entity_kind: EntityKind,
        memory_id: crate::MemoryId,
    ) -> Result<Option<String>, StorageError>;

    async fn list_facts_missing_embedding(
        &self,
        owner: &Owner,
        model_id: &str,
        limit: usize,
    ) -> Result<Vec<crate::MemoryId>, StorageError>;
}

#[async_trait::async_trait]
pub trait EmbeddingWritePort: Send + Sync {
    async fn upsert_fact_embedding(
        &self,
        owner: &Owner,
        memory_id: crate::MemoryId,
        model_id: &str,
        dim: usize,
        vec: &[f32],
    ) -> Result<(), StorageError>;

    async fn upsert_memory_embedding(
        &self,
        owner: &Owner,
        entity_kind: EntityKind,
        memory_id: crate::MemoryId,
        model_id: &str,
        dim: usize,
        vec: &[f32],
    ) -> Result<(), StorageError>;
}

#[async_trait::async_trait]
pub trait EmbeddingJobPort: Send + Sync {
    async fn claim_pending_embedding_jobs(
        &self,
        model_id: &str,
        limit: i64,
    ) -> Result<Vec<EmbeddingJobClaim>, StorageError>;

    async fn complete_embedding_job(&self, claim: &EmbeddingJobClaim) -> Result<(), StorageError>;

    async fn fail_embedding_job(
        &self,
        claim: &EmbeddingJobClaim,
        error: &str,
    ) -> Result<(), StorageError>;

    async fn enqueue_missing_embedding_jobs(
        &self,
        owner: &Owner,
        model_id: &str,
        limit: i64,
    ) -> Result<u64, StorageError>;

    async fn count_pending_embedding_jobs(&self, owner: &Owner) -> Result<u64, StorageError>;
}

#[async_trait::async_trait]
pub trait GoalWritePort: Send + Sync {
    async fn create_goal_atomic(
        &self,
        req: &CreateGoalAtomicRequest<'_>,
    ) -> Result<GoalWriteOutcome, StorageError>;

    async fn transition_goal_atomic(
        &self,
        req: &TransitionGoalAtomicRequest<'_>,
    ) -> Result<GoalWriteOutcome, StorageError>;

    async fn achieve_goal_atomic(
        &self,
        req: &AchieveGoalAtomicRequest<'_>,
    ) -> Result<GoalWriteOutcome, StorageError>;

    async fn modify_goal_atomic(
        &self,
        req: &ModifyGoalAtomicRequest<'_>,
    ) -> Result<GoalWriteOutcome, StorageError>;

    async fn decompose_goal_atomic(
        &self,
        req: &DecomposeGoalAtomicRequest<'_>,
    ) -> Result<DecomposeGoalOutcome, StorageError>;
}

#[async_trait::async_trait]
pub trait GoalSupportReadPort: Send + Sync {
    async fn active_personality_root(
        &self,
        _owner: &Owner,
        _instance_id: PersonalityInstanceId,
    ) -> Result<Option<MemoryId>, StorageError> {
        Ok(None)
    }
}

#[async_trait::async_trait]
pub trait GoalReadPort: Send + Sync {
    async fn list_active_goals(
        &self,
        read_owners: &[OwnerRef],
        self_perspective_memory_id: crate::MemoryId,
        limit: usize,
    ) -> Result<Vec<ActiveGoalSummary>, StorageError>;
}

#[async_trait::async_trait]
pub trait ChangeEventPort: Send + Sync {
    async fn change_history(
        &self,
        read_owners: &[OwnerRef],
        req: &ChangeHistoryRequest,
    ) -> Result<ChangeHistoryResponse, StorageError>;

    async fn list_change_events_after(
        &self,
        read_owners: &[OwnerRef],
        after: uuid::Uuid,
        limit: usize,
    ) -> Result<Vec<ChangeEventForWake>, StorageError>;

    async fn list_change_events_for_replay(
        &self,
        owner: &Owner,
        after: uuid::Uuid,
        until: Option<uuid::Uuid>,
        limit: usize,
    ) -> Result<Vec<ChangeEventForWake>, StorageError> {
        let rows = self
            .list_change_events_after(std::slice::from_ref(owner), after, limit)
            .await?;
        Ok(rows
            .into_iter()
            .filter(|row| until.is_none_or(|until| row.event.seq <= until))
            .collect())
    }
}

#[async_trait::async_trait]
pub trait EdgeReadPort: Send + Sync {
    async fn read_edges(
        &self,
        read_owners: &[OwnerRef],
        req: &crate::verbs::query::EdgeReadRequest,
    ) -> Result<crate::verbs::query::EdgeReadResponse, StorageError>;

    async fn edge_exists(
        &self,
        read_owners: &[OwnerRef],
        req: &crate::verbs::query::EdgeExistsRequest,
    ) -> Result<crate::verbs::query::EdgeExistsResponse, StorageError>;
}

#[async_trait::async_trait]
pub trait CitationPort: Send + Sync {
    async fn fact_entity_id_for(
        &self,
        owner: &Owner,
        schema_id: &SchemaId,
        schema_version: SchemaVersion,
        natural_key: &[String],
    ) -> Result<Option<FactEntityId>, StorageError>;

    async fn facts_citing_object(
        &self,
        read_owners: &[OwnerRef],
        cited_object_id: uuid::Uuid,
        sidecars: &[SidecarSpec],
    ) -> Result<Vec<crate::personality::MemorySnapshot>, StorageError>;

    async fn citation_of_fact(
        &self,
        fact_memory_id: crate::MemoryId,
    ) -> Result<Option<crate::verbs::query::FactCitationReadback>, StorageError>;

    async fn citation_of_entity_head(
        &self,
        read_owners: &[OwnerRef],
        fact_entity_id: FactEntityId,
    ) -> Result<Option<crate::verbs::query::FactCitationReadback>, StorageError>;
}

#[async_trait::async_trait]
pub trait OwnerAccessReadPort: Send + Sync {
    async fn resolve_membership(
        &self,
        member: &OwnerRef,
    ) -> Result<Vec<MembershipRow>, StorageError>;

    async fn visible_to_any(
        &self,
        entity: EntityId,
        read_owners: &[OwnerRef],
    ) -> Result<bool, StorageError>;

    async fn home_owner(&self, entity: EntityId) -> Result<Option<OwnerRef>, StorageError>;
}

#[async_trait::async_trait]
pub trait OwnerMembershipAdminPort: Send + Sync {
    async fn add_group_member(
        &self,
        group_id: GroupId,
        member_user_id: UserId,
        relation: Relation,
        granted_by: uuid::Uuid,
    ) -> Result<(), StorageError>;

    async fn remove_group_member(
        &self,
        group_id: GroupId,
        member_user_id: UserId,
    ) -> Result<(), StorageError>;

    async fn list_group_members(
        &self,
        group_id: GroupId,
    ) -> Result<Vec<(UserId, Relation)>, StorageError>;
}

#[async_trait::async_trait]
pub trait SourceBatchPort: Send + Sync {
    async fn close_batch(
        &self,
        principal: &OwnerRef,
        source_batch_id: SourceBatchId,
    ) -> Result<CloseBatchOutcome, StorageError>;
}

/// Current runtime personality storage boundary; PR6 owns public ontology/naming cleanup.
#[async_trait::async_trait]
pub trait PersonalityReadPort: Send + Sync {
    async fn list_personality_instances(
        &self,
        owner: &Owner,
        include_tombstoned: bool,
    ) -> Result<Vec<PersonalityInstanceRow>, StorageError>;
}

/// Current runtime personality storage boundary; PR6 owns public ontology/naming cleanup.
#[async_trait::async_trait]
pub trait PersonalityWritePort: Send + Sync {
    async fn tombstone_personality(
        &self,
        req: &TombstonePersonalityRequest,
    ) -> Result<TombstonePersonalityResponse, StorageError>;

    async fn instantiate_personality(
        &self,
        req: &InstantiatePersonalityRequest,
    ) -> Result<InstantiatePersonalityResponse, StorageError>;

    async fn ensure_master_token_personality(
        &self,
        owner: &Owner,
        master_token_id: uuid::Uuid,
    ) -> Result<MasterTokenPersonality, StorageError>;

    async fn ensure_subject_personality(
        &self,
        owner: &Owner,
        subject: &OwnerRef,
    ) -> Result<MasterTokenPersonality, StorageError>;

    async fn append_personality_memories(
        &self,
        req: &PersonalityWriteRequest<'_>,
    ) -> Result<PersonalityWriteOutcome, StorageError>;
}

#[async_trait::async_trait]
pub trait WakeConfigPort: Send + Sync {
    async fn set_wake_entries(
        &self,
        req: &SetWakeEntriesRequest,
    ) -> Result<SetWakeEntriesResponse, StorageError>;

    async fn set_wake_entries_within(
        &self,
        owner: &Owner,
        personality_instance_id: PersonalityInstanceId,
        mutate: WakeEntriesMutator,
    ) -> Result<SetWakeEntriesResponse, StorageError>;
}

#[async_trait::async_trait]
pub trait FactRetentionPort: Send + Sync {
    async fn upsert_fact_retention(&self, owner: &Owner, seconds: i64) -> Result<(), StorageError>;

    async fn get_fact_retention(&self, owner: &Owner) -> Result<Option<i64>, StorageError>;

    async fn clear_fact_retention(&self, owner: &Owner) -> Result<bool, StorageError>;
}

#[async_trait::async_trait]
pub trait ComplianceErasePort: Send + Sync {
    async fn cleanup_due_facts(
        &self,
        owner: &Owner,
        fact_sidecar_tables: &[String],
        edge_sidecar_tables: &[String],
        citation_mapping_sidecar_tables: &[String],
        cited_object_sidecar_tables: &[String],
    ) -> Result<CleanupDueFactsOutcome, StorageError>;

    async fn tombstone_fact(
        &self,
        owner: &Owner,
        fact_id: uuid::Uuid,
        fact_sidecar_tables: &[String],
        edge_sidecar_tables: &[String],
        citation_mapping_sidecar_tables: &[String],
        cited_object_sidecar_tables: &[String],
    ) -> Result<TombstoneFactOutcome, StorageError>;
}

#[async_trait::async_trait]
pub trait RegistryProjectionPort: Send + Sync {
    async fn load_memory_batch_facts(
        &self,
        owner: &Owner,
        memory_id: crate::MemoryId,
        sidecars: &[SidecarSpec],
    ) -> Result<Vec<crate::FactRow>, StorageError>;

    async fn load_abstraction_heads(
        &self,
        owner: &Owner,
        sidecars: &[SidecarSpec],
        limit: usize,
    ) -> Result<Vec<AbstractionRow>, StorageError>;

    async fn load_perspective_heads(
        &self,
        owner: &Owner,
        instance: PersonalityInstanceId,
        root_perspective_memory_id: crate::MemoryId,
        sidecars: &[SidecarSpec],
        limit: usize,
    ) -> Result<Vec<MemorySnapshot>, StorageError>;

    async fn lookup_prior_personality_head(
        &self,
        owner: &Owner,
        instance: &PersonalityRef,
        schema_id: &crate::SchemaId,
    ) -> Result<Option<crate::MemoryId>, StorageError>;
}

pub type FactIngestHandle = Arc<dyn FactIngestPort>;
pub type OperatorInvocationWriteHandle = Arc<dyn OperatorInvocationWritePort>;
pub type OperatorInvocationReadHandle = Arc<dyn OperatorInvocationReadPort>;
pub type MemoryAuthoringHandle = Arc<dyn MemoryAuthoringPort>;
pub type MemoryReadHandle = Arc<dyn MemoryReadPort>;
pub type MemoryInspectHandle = Arc<dyn MemoryInspectPort>;
pub type EmbeddingTextHandle = Arc<dyn EmbeddingTextPort>;
pub type EmbeddingWriteHandle = Arc<dyn EmbeddingWritePort>;
pub type EmbeddingJobHandle = Arc<dyn EmbeddingJobPort>;
pub type GoalWriteHandle = Arc<dyn GoalWritePort>;
pub type GoalSupportReadHandle = Arc<dyn GoalSupportReadPort>;
pub type GoalReadHandle = Arc<dyn GoalReadPort>;
pub type ChangeEventHandle = Arc<dyn ChangeEventPort>;
pub type EdgeReadHandle = Arc<dyn EdgeReadPort>;
pub type CitationHandle = Arc<dyn CitationPort>;
pub type OwnerAccessReadHandle = Arc<dyn OwnerAccessReadPort>;
pub type OwnerMembershipAdminHandle = Arc<dyn OwnerMembershipAdminPort>;
pub type SourceBatchHandle = Arc<dyn SourceBatchPort>;
pub type PersonalityReadHandle = Arc<dyn PersonalityReadPort>;
pub type PersonalityWriteHandle = Arc<dyn PersonalityWritePort>;
pub type WakeConfigHandle = Arc<dyn WakeConfigPort>;
pub type FactRetentionHandle = Arc<dyn FactRetentionPort>;
pub type ComplianceEraseHandle = Arc<dyn ComplianceErasePort>;
pub type RegistryProjectionHandle = Arc<dyn RegistryProjectionPort>;

#[allow(dead_code)]
#[derive(Clone)]
pub struct StoragePorts {
    fact_ingest: FactIngestHandle,
    operator_invocation_write: OperatorInvocationWriteHandle,
    operator_invocation_read: OperatorInvocationReadHandle,
    memory_authoring: MemoryAuthoringHandle,
    memory_read: MemoryReadHandle,
    memory_inspect: MemoryInspectHandle,
    embedding_text: EmbeddingTextHandle,
    embedding_write: EmbeddingWriteHandle,
    embedding_job: EmbeddingJobHandle,
    goal_write: GoalWriteHandle,
    goal_support_read: GoalSupportReadHandle,
    goal_read: GoalReadHandle,
    change_event: ChangeEventHandle,
    edge_read: EdgeReadHandle,
    citation: CitationHandle,
    owner_access_read: OwnerAccessReadHandle,
    owner_membership_admin: OwnerMembershipAdminHandle,
    source_batch: SourceBatchHandle,
    personality_read: PersonalityReadHandle,
    personality_write: PersonalityWriteHandle,
    wake_config: WakeConfigHandle,
    fact_retention: FactRetentionHandle,
    compliance_erase: ComplianceEraseHandle,
    registry_projection: RegistryProjectionHandle,
}

#[derive(Clone)]
pub(crate) struct AccessReadStoragePorts {
    pub owner_access_read: OwnerAccessReadHandle,
}

#[derive(Clone)]
pub(crate) struct AccessAdminStoragePorts {
    pub owner_membership_admin: OwnerMembershipAdminHandle,
}

#[derive(Clone)]
pub(crate) struct FactRetentionStoragePorts {
    pub fact_retention: FactRetentionHandle,
    pub compliance_erase: ComplianceEraseHandle,
}

#[derive(Clone)]
pub(crate) struct GoalCommandStoragePorts {
    pub goal_write: GoalWriteHandle,
    pub goal_support_read: GoalSupportReadHandle,
    pub owner_access_read: OwnerAccessReadHandle,
}

#[derive(Clone)]
pub(crate) struct IngestStoragePorts {
    pub fact_ingest: FactIngestHandle,
    pub operator_invocation_write: OperatorInvocationWriteHandle,
    pub embedding_text: EmbeddingTextHandle,
    pub embedding_write: EmbeddingWriteHandle,
    pub embedding_job: EmbeddingJobHandle,
    pub source_batch: SourceBatchHandle,
}

#[derive(Clone)]
pub(crate) struct MemoryAuthoringStoragePorts {
    pub memory_authoring: MemoryAuthoringHandle,
    pub owner_access_read: OwnerAccessReadHandle,
}

#[derive(Clone)]
pub(crate) struct PersonalityStoragePorts {
    pub personality_read: PersonalityReadHandle,
    pub personality_write: PersonalityWriteHandle,
    pub wake_config: WakeConfigHandle,
}

#[derive(Clone)]
pub(crate) struct PipelineStoragePorts {
    pub owner_access_read: OwnerAccessReadHandle,
}

#[derive(Clone)]
pub(crate) struct QueryStoragePorts {
    pub change_event: ChangeEventHandle,
    pub operator_invocation_read: OperatorInvocationReadHandle,
    pub memory_read: MemoryReadHandle,
    pub edge_read: EdgeReadHandle,
}

#[derive(Clone)]
pub(crate) struct ReadVerbStoragePorts {
    pub embedding_job: EmbeddingJobHandle,
    pub memory_read: MemoryReadHandle,
    pub memory_inspect: MemoryInspectHandle,
    pub change_event: ChangeEventHandle,
    pub citation: CitationHandle,
    pub personality_read: PersonalityReadHandle,
    pub fact_retention: FactRetentionHandle,
}

#[derive(Clone)]
pub(crate) struct EngineStoragePorts {
    pub access_read: AccessReadStoragePorts,
    pub access_admin: AccessAdminStoragePorts,
    pub fact_retention: FactRetentionStoragePorts,
    pub goal_command: GoalCommandStoragePorts,
    pub ingest: IngestStoragePorts,
    pub memory_authoring: MemoryAuthoringStoragePorts,
    pub personality: PersonalityStoragePorts,
    pub pipeline: PipelineStoragePorts,
    pub query: QueryStoragePorts,
    pub read_verb: ReadVerbStoragePorts,
}

#[derive(Default)]
pub struct StoragePortsBuilder {
    fact_ingest: Option<FactIngestHandle>,
    operator_invocation_write: Option<OperatorInvocationWriteHandle>,
    operator_invocation_read: Option<OperatorInvocationReadHandle>,
    memory_authoring: Option<MemoryAuthoringHandle>,
    memory_read: Option<MemoryReadHandle>,
    memory_inspect: Option<MemoryInspectHandle>,
    embedding_text: Option<EmbeddingTextHandle>,
    embedding_write: Option<EmbeddingWriteHandle>,
    embedding_job: Option<EmbeddingJobHandle>,
    goal_write: Option<GoalWriteHandle>,
    goal_support_read: Option<GoalSupportReadHandle>,
    goal_read: Option<GoalReadHandle>,
    change_event: Option<ChangeEventHandle>,
    edge_read: Option<EdgeReadHandle>,
    citation: Option<CitationHandle>,
    owner_access_read: Option<OwnerAccessReadHandle>,
    owner_membership_admin: Option<OwnerMembershipAdminHandle>,
    source_batch: Option<SourceBatchHandle>,
    personality_read: Option<PersonalityReadHandle>,
    personality_write: Option<PersonalityWriteHandle>,
    wake_config: Option<WakeConfigHandle>,
    fact_retention: Option<FactRetentionHandle>,
    compliance_erase: Option<ComplianceEraseHandle>,
    registry_projection: Option<RegistryProjectionHandle>,
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
            operator_invocation_write: rejecting.clone(),
            operator_invocation_read: rejecting.clone(),
            memory_authoring: rejecting.clone(),
            memory_read: rejecting.clone(),
            memory_inspect: rejecting.clone(),
            embedding_text: rejecting.clone(),
            embedding_write: rejecting.clone(),
            embedding_job: rejecting.clone(),
            goal_write: rejecting.clone(),
            goal_support_read: rejecting.clone(),
            goal_read: rejecting.clone(),
            change_event: rejecting.clone(),
            edge_read: rejecting.clone(),
            citation: rejecting.clone(),
            owner_access_read: rejecting.clone(),
            owner_membership_admin: rejecting.clone(),
            source_batch: rejecting.clone(),
            personality_read: rejecting.clone(),
            personality_write: rejecting.clone(),
            wake_config: rejecting.clone(),
            fact_retention: rejecting.clone(),
            compliance_erase: rejecting.clone(),
            registry_projection: rejecting.clone(),
        }
    }
}

impl From<StoragePorts> for EngineStoragePorts {
    fn from(ports: StoragePorts) -> Self {
        Self {
            access_read: AccessReadStoragePorts {
                owner_access_read: ports.owner_access_read.clone(),
            },
            access_admin: AccessAdminStoragePorts {
                owner_membership_admin: ports.owner_membership_admin.clone(),
            },
            fact_retention: FactRetentionStoragePorts {
                fact_retention: ports.fact_retention.clone(),
                compliance_erase: ports.compliance_erase.clone(),
            },
            goal_command: GoalCommandStoragePorts {
                goal_write: ports.goal_write.clone(),
                goal_support_read: ports.goal_support_read.clone(),
                owner_access_read: ports.owner_access_read.clone(),
            },
            ingest: IngestStoragePorts {
                fact_ingest: ports.fact_ingest.clone(),
                operator_invocation_write: ports.operator_invocation_write.clone(),
                embedding_text: ports.embedding_text.clone(),
                embedding_write: ports.embedding_write.clone(),
                embedding_job: ports.embedding_job.clone(),
                source_batch: ports.source_batch.clone(),
            },
            memory_authoring: MemoryAuthoringStoragePorts {
                memory_authoring: ports.memory_authoring.clone(),
                owner_access_read: ports.owner_access_read.clone(),
            },
            personality: PersonalityStoragePorts {
                personality_read: ports.personality_read.clone(),
                personality_write: ports.personality_write.clone(),
                wake_config: ports.wake_config.clone(),
            },
            pipeline: PipelineStoragePorts {
                owner_access_read: ports.owner_access_read.clone(),
            },
            query: QueryStoragePorts {
                change_event: ports.change_event.clone(),
                operator_invocation_read: ports.operator_invocation_read.clone(),
                memory_read: ports.memory_read.clone(),
                edge_read: ports.edge_read.clone(),
            },
            read_verb: ReadVerbStoragePorts {
                embedding_job: ports.embedding_job.clone(),
                memory_read: ports.memory_read.clone(),
                memory_inspect: ports.memory_inspect.clone(),
                change_event: ports.change_event.clone(),
                citation: ports.citation.clone(),
                personality_read: ports.personality_read.clone(),
                fact_retention: ports.fact_retention.clone(),
            },
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
    pub fn operator_invocation_write(mut self, handle: OperatorInvocationWriteHandle) -> Self {
        self.operator_invocation_write = Some(handle);
        self
    }

    #[must_use]
    pub fn operator_invocation_read(mut self, handle: OperatorInvocationReadHandle) -> Self {
        self.operator_invocation_read = Some(handle);
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
    pub fn goal_write(mut self, handle: GoalWriteHandle) -> Self {
        self.goal_write = Some(handle);
        self
    }

    #[must_use]
    pub fn goal_support_read(mut self, handle: GoalSupportReadHandle) -> Self {
        self.goal_support_read = Some(handle);
        self
    }

    #[must_use]
    pub fn goal_read(mut self, handle: GoalReadHandle) -> Self {
        self.goal_read = Some(handle);
        self
    }

    #[must_use]
    pub fn change_event(mut self, handle: ChangeEventHandle) -> Self {
        self.change_event = Some(handle);
        self
    }

    #[must_use]
    pub fn edge_read(mut self, handle: EdgeReadHandle) -> Self {
        self.edge_read = Some(handle);
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
    pub fn source_batch(mut self, handle: SourceBatchHandle) -> Self {
        self.source_batch = Some(handle);
        self
    }

    #[must_use]
    pub fn personality_read(mut self, handle: PersonalityReadHandle) -> Self {
        self.personality_read = Some(handle);
        self
    }

    #[must_use]
    pub fn personality_write(mut self, handle: PersonalityWriteHandle) -> Self {
        self.personality_write = Some(handle);
        self
    }

    #[must_use]
    pub fn wake_config(mut self, handle: WakeConfigHandle) -> Self {
        self.wake_config = Some(handle);
        self
    }

    #[must_use]
    pub fn fact_retention(mut self, handle: FactRetentionHandle) -> Self {
        self.fact_retention = Some(handle);
        self
    }

    #[must_use]
    pub fn compliance_erase(mut self, handle: ComplianceEraseHandle) -> Self {
        self.compliance_erase = Some(handle);
        self
    }

    #[must_use]
    pub fn registry_projection(mut self, handle: RegistryProjectionHandle) -> Self {
        self.registry_projection = Some(handle);
        self
    }

    /// Builds a complete storage port bundle.
    ///
    /// # Panics
    ///
    /// Panics when any required port handle was not configured.
    #[must_use]
    pub fn build(self) -> StoragePorts {
        StoragePorts {
            fact_ingest: self
                .fact_ingest
                .expect("fact_ingest storage port configured"),
            operator_invocation_write: self
                .operator_invocation_write
                .expect("operator_invocation_write storage port configured"),
            operator_invocation_read: self
                .operator_invocation_read
                .expect("operator_invocation_read storage port configured"),
            memory_authoring: self
                .memory_authoring
                .expect("memory_authoring storage port configured"),
            memory_read: self
                .memory_read
                .expect("memory_read storage port configured"),
            memory_inspect: self
                .memory_inspect
                .expect("memory_inspect storage port configured"),
            embedding_text: self
                .embedding_text
                .expect("embedding_text storage port configured"),
            embedding_write: self
                .embedding_write
                .expect("embedding_write storage port configured"),
            embedding_job: self
                .embedding_job
                .expect("embedding_job storage port configured"),
            goal_write: self.goal_write.expect("goal_write storage port configured"),
            goal_support_read: self
                .goal_support_read
                .expect("goal_support_read storage port configured"),
            goal_read: self.goal_read.expect("goal_read storage port configured"),
            change_event: self
                .change_event
                .expect("change_event storage port configured"),
            edge_read: self.edge_read.expect("edge_read storage port configured"),
            citation: self.citation.expect("citation storage port configured"),
            owner_access_read: self
                .owner_access_read
                .expect("owner_access_read storage port configured"),
            owner_membership_admin: self
                .owner_membership_admin
                .expect("owner_membership_admin storage port configured"),
            source_batch: self
                .source_batch
                .expect("source_batch storage port configured"),
            personality_read: self
                .personality_read
                .expect("personality_read storage port configured"),
            personality_write: self
                .personality_write
                .expect("personality_write storage port configured"),
            wake_config: self
                .wake_config
                .expect("wake_config storage port configured"),
            fact_retention: self
                .fact_retention
                .expect("fact_retention storage port configured"),
            compliance_erase: self
                .compliance_erase
                .expect("compliance_erase storage port configured"),
            registry_projection: self
                .registry_projection
                .expect("registry_projection storage port configured"),
        }
    }
}

#[derive(Debug)]
struct RejectingStorage;

#[async_trait::async_trait]
impl GoalSupportReadPort for RejectingStorage {}

#[async_trait::async_trait]
impl FactIngestPort for RejectingStorage {
    async fn ingest_fact_atomic(
        &self,
        _owner: &Owner,
        _draft: &FactWriteCommand,
        _embedding_model_id: Option<&str>,
    ) -> Result<FactIngestOutcome, StorageError> {
        Err(StorageError::Internal(
            "RejectingStorage rejects writes".into(),
        ))
    }

    async fn ingest_fact_with_typed_sidecar(
        &self,
        _authorized: &AuthorizedFactWrite,
        _sidecar_payload: &SidecarPayload,
        _embedding_model_id: Option<&str>,
    ) -> Result<FactIngestOutcome, StorageError> {
        Err(StorageError::Internal(
            "RejectingStorage rejects writes".into(),
        ))
    }

    async fn ingest_fact_with_citation_and_typed_sidecar(
        &self,
        _authorized: &AuthorizedFactWithCitation,
        _sidecar_payload: &SidecarPayload,
        _embedding_model_id: Option<&str>,
    ) -> Result<FactIngestOutcome, StorageError> {
        Err(StorageError::Internal(
            "RejectingStorage rejects writes".into(),
        ))
    }
}

#[async_trait::async_trait]
impl OperatorInvocationWritePort for RejectingStorage {
    async fn persist_mcp_call_atomic(
        &self,
        _input: &McpCallLogInput,
    ) -> Result<McpCallLogOutcome, StorageError> {
        Err(StorageError::Internal(
            "RejectingStorage rejects writes".into(),
        ))
    }
}

#[async_trait::async_trait]
impl OperatorInvocationReadPort for RejectingStorage {
    async fn read_mcp_call_history(
        &self,
        _req: &McpCallHistoryRequest,
    ) -> Result<McpCallHistoryResponse, StorageError> {
        Ok(McpCallHistoryResponse { calls: Vec::new() })
    }
}

#[async_trait::async_trait]
impl MemoryAuthoringPort for RejectingStorage {
    async fn author_derived(
        &self,
        _req: &AuthorDerivedRequest<'_>,
    ) -> Result<AuthorDerivedOutcome, StorageError> {
        Err(StorageError::Internal(
            "RejectingStorage rejects writes".into(),
        ))
    }

    async fn append_memory_edge(
        &self,
        _edge: &DerivedEdgeSpec<'_>,
        _proof: EdgeWriteProof,
    ) -> Result<EdgeId, StorageError> {
        Err(StorageError::Internal(
            "RejectingStorage rejects writes".into(),
        ))
    }
}

#[async_trait::async_trait]
impl MemoryReadPort for RejectingStorage {
    async fn load_fact_text(
        &self,
        _owner: &Owner,
        _memory_id: crate::MemoryId,
    ) -> Result<Option<String>, StorageError> {
        Ok(None)
    }

    async fn query_memories(
        &self,
        _req: &crate::verbs::query::QueryRequest,
        _schemas: &[crate::verbs::schema::SchemaInfo],
    ) -> Result<crate::verbs::query::QueryResponse, StorageError> {
        Ok(crate::verbs::query::QueryResponse {
            memories: Vec::new(),
            goals: Vec::new(),
            edges: Vec::new(),
            seq_high_water: None,
        })
    }

    async fn search_memories(
        &self,
        _req: &crate::verbs::query::MemorySearchRequest,
        _projections: &[crate::verbs::schema::MemorySearchProjection],
    ) -> Result<Vec<crate::verbs::query::MemorySearchResult>, StorageError> {
        Ok(Vec::new())
    }

    async fn walk_memory_lineage(
        &self,
        _read_owners: &[OwnerRef],
        _req: &crate::verbs::query::MemoryLineageRequest,
    ) -> Result<crate::verbs::query::MemoryLineageResponse, StorageError> {
        Ok(crate::verbs::query::MemoryLineageResponse {
            nodes: Vec::new(),
            edges: Vec::new(),
            truncated: false,
        })
    }
}

#[async_trait::async_trait]
impl MemoryInspectPort for RejectingStorage {
    async fn load_memory_by_id(
        &self,
        _memory_id: crate::MemoryId,
        _reader_personality_instance_id: Option<PersonalityInstanceId>,
        _sidecars: &[SidecarSpec],
    ) -> Result<Option<MemorySnapshot>, StorageError> {
        Ok(None)
    }
}

#[async_trait::async_trait]
impl EmbeddingTextPort for RejectingStorage {
    async fn load_embedding_text(
        &self,
        _owner: &Owner,
        _entity_kind: EntityKind,
        _memory_id: crate::MemoryId,
    ) -> Result<Option<String>, StorageError> {
        Ok(None)
    }

    async fn list_facts_missing_embedding(
        &self,
        _owner: &Owner,
        _model_id: &str,
        _limit: usize,
    ) -> Result<Vec<crate::MemoryId>, StorageError> {
        Ok(Vec::new())
    }
}

#[async_trait::async_trait]
impl EmbeddingWritePort for RejectingStorage {
    async fn upsert_fact_embedding(
        &self,
        _owner: &Owner,
        _memory_id: crate::MemoryId,
        _model_id: &str,
        _dim: usize,
        _vec: &[f32],
    ) -> Result<(), StorageError> {
        Ok(())
    }

    async fn upsert_memory_embedding(
        &self,
        _owner: &Owner,
        _entity_kind: EntityKind,
        _memory_id: crate::MemoryId,
        _model_id: &str,
        _dim: usize,
        _vec: &[f32],
    ) -> Result<(), StorageError> {
        Ok(())
    }
}

#[async_trait::async_trait]
impl EmbeddingJobPort for RejectingStorage {
    async fn claim_pending_embedding_jobs(
        &self,
        _model_id: &str,
        _limit: i64,
    ) -> Result<Vec<EmbeddingJobClaim>, StorageError> {
        Err(StorageError::Internal(
            "RejectingStorage rejects writes".into(),
        ))
    }

    async fn complete_embedding_job(&self, _claim: &EmbeddingJobClaim) -> Result<(), StorageError> {
        Err(StorageError::Internal(
            "RejectingStorage rejects writes".into(),
        ))
    }

    async fn fail_embedding_job(
        &self,
        _claim: &EmbeddingJobClaim,
        _error: &str,
    ) -> Result<(), StorageError> {
        Err(StorageError::Internal(
            "RejectingStorage rejects writes".into(),
        ))
    }

    async fn enqueue_missing_embedding_jobs(
        &self,
        _owner: &Owner,
        _model_id: &str,
        _limit: i64,
    ) -> Result<u64, StorageError> {
        Err(StorageError::Internal(
            "RejectingStorage rejects writes".into(),
        ))
    }

    async fn count_pending_embedding_jobs(&self, _owner: &Owner) -> Result<u64, StorageError> {
        Ok(0)
    }
}

#[async_trait::async_trait]
impl GoalWritePort for RejectingStorage {
    async fn create_goal_atomic(
        &self,
        _req: &CreateGoalAtomicRequest<'_>,
    ) -> Result<GoalWriteOutcome, StorageError> {
        Err(StorageError::Internal(
            "RejectingStorage rejects writes".into(),
        ))
    }

    async fn transition_goal_atomic(
        &self,
        _req: &TransitionGoalAtomicRequest<'_>,
    ) -> Result<GoalWriteOutcome, StorageError> {
        Err(StorageError::Internal(
            "RejectingStorage rejects writes".into(),
        ))
    }

    async fn achieve_goal_atomic(
        &self,
        _req: &AchieveGoalAtomicRequest<'_>,
    ) -> Result<GoalWriteOutcome, StorageError> {
        Err(StorageError::Internal(
            "RejectingStorage rejects writes".into(),
        ))
    }

    async fn modify_goal_atomic(
        &self,
        _req: &ModifyGoalAtomicRequest<'_>,
    ) -> Result<GoalWriteOutcome, StorageError> {
        Err(StorageError::Internal(
            "RejectingStorage rejects writes".into(),
        ))
    }

    async fn decompose_goal_atomic(
        &self,
        _req: &DecomposeGoalAtomicRequest<'_>,
    ) -> Result<DecomposeGoalOutcome, StorageError> {
        Err(StorageError::Internal(
            "RejectingStorage rejects writes".into(),
        ))
    }
}

#[async_trait::async_trait]
impl GoalReadPort for RejectingStorage {
    async fn list_active_goals(
        &self,
        _read_owners: &[OwnerRef],
        _self_perspective_memory_id: crate::MemoryId,
        _limit: usize,
    ) -> Result<Vec<ActiveGoalSummary>, StorageError> {
        Ok(Vec::new())
    }
}

#[async_trait::async_trait]
impl ChangeEventPort for RejectingStorage {
    async fn change_history(
        &self,
        _read_owners: &[OwnerRef],
        _req: &ChangeHistoryRequest,
    ) -> Result<ChangeHistoryResponse, StorageError> {
        Ok(ChangeHistoryResponse {
            events: Vec::new(),
            seq_high_water: None,
        })
    }

    async fn list_change_events_after(
        &self,
        _read_owners: &[OwnerRef],
        _after: uuid::Uuid,
        _limit: usize,
    ) -> Result<Vec<ChangeEventForWake>, StorageError> {
        Ok(Vec::new())
    }
}

#[async_trait::async_trait]
impl EdgeReadPort for RejectingStorage {
    async fn read_edges(
        &self,
        _read_owners: &[OwnerRef],
        _req: &crate::verbs::query::EdgeReadRequest,
    ) -> Result<crate::verbs::query::EdgeReadResponse, StorageError> {
        Ok(crate::verbs::query::EdgeReadResponse { edges: Vec::new() })
    }

    async fn edge_exists(
        &self,
        _read_owners: &[OwnerRef],
        _req: &crate::verbs::query::EdgeExistsRequest,
    ) -> Result<crate::verbs::query::EdgeExistsResponse, StorageError> {
        Ok(crate::verbs::query::EdgeExistsResponse { exists: false })
    }
}

#[async_trait::async_trait]
impl CitationPort for RejectingStorage {
    async fn fact_entity_id_for(
        &self,
        _owner: &Owner,
        _schema_id: &SchemaId,
        _schema_version: SchemaVersion,
        _natural_key: &[String],
    ) -> Result<Option<FactEntityId>, StorageError> {
        Ok(None)
    }

    async fn facts_citing_object(
        &self,
        _read_owners: &[OwnerRef],
        _cited_object_id: uuid::Uuid,
        _sidecars: &[SidecarSpec],
    ) -> Result<Vec<crate::personality::MemorySnapshot>, StorageError> {
        Ok(Vec::new())
    }

    async fn citation_of_fact(
        &self,
        _fact_memory_id: crate::MemoryId,
    ) -> Result<Option<crate::verbs::query::FactCitationReadback>, StorageError> {
        Ok(None)
    }

    async fn citation_of_entity_head(
        &self,
        _read_owners: &[OwnerRef],
        _fact_entity_id: FactEntityId,
    ) -> Result<Option<crate::verbs::query::FactCitationReadback>, StorageError> {
        Ok(None)
    }
}

#[async_trait::async_trait]
impl OwnerAccessReadPort for RejectingStorage {
    async fn resolve_membership(
        &self,
        _member: &OwnerRef,
    ) -> Result<Vec<MembershipRow>, StorageError> {
        Ok(Vec::new())
    }

    async fn visible_to_any(
        &self,
        _entity: EntityId,
        _read_owners: &[OwnerRef],
    ) -> Result<bool, StorageError> {
        Ok(false)
    }

    async fn home_owner(&self, _entity: EntityId) -> Result<Option<OwnerRef>, StorageError> {
        Ok(None)
    }
}

#[async_trait::async_trait]
impl OwnerMembershipAdminPort for RejectingStorage {
    async fn add_group_member(
        &self,
        _group_id: GroupId,
        _member_user_id: UserId,
        _relation: Relation,
        _granted_by: uuid::Uuid,
    ) -> Result<(), StorageError> {
        Err(StorageError::Internal(
            "RejectingStorage rejects writes".into(),
        ))
    }

    async fn remove_group_member(
        &self,
        _group_id: GroupId,
        _member_user_id: UserId,
    ) -> Result<(), StorageError> {
        Err(StorageError::Internal(
            "RejectingStorage rejects writes".into(),
        ))
    }

    async fn list_group_members(
        &self,
        _group_id: GroupId,
    ) -> Result<Vec<(UserId, Relation)>, StorageError> {
        Ok(Vec::new())
    }
}

#[async_trait::async_trait]
impl SourceBatchPort for RejectingStorage {
    async fn close_batch(
        &self,
        _principal: &OwnerRef,
        _source_batch_id: SourceBatchId,
    ) -> Result<CloseBatchOutcome, StorageError> {
        Err(StorageError::Internal(
            "RejectingStorage rejects writes".into(),
        ))
    }
}

#[async_trait::async_trait]
impl PersonalityReadPort for RejectingStorage {
    async fn list_personality_instances(
        &self,
        _owner: &Owner,
        _include_tombstoned: bool,
    ) -> Result<Vec<PersonalityInstanceRow>, StorageError> {
        Ok(Vec::new())
    }
}

#[async_trait::async_trait]
impl PersonalityWritePort for RejectingStorage {
    async fn tombstone_personality(
        &self,
        _req: &TombstonePersonalityRequest,
    ) -> Result<TombstonePersonalityResponse, StorageError> {
        Err(StorageError::Internal(
            "RejectingStorage rejects writes".into(),
        ))
    }

    async fn instantiate_personality(
        &self,
        _req: &InstantiatePersonalityRequest,
    ) -> Result<InstantiatePersonalityResponse, StorageError> {
        Err(StorageError::Internal(
            "RejectingStorage rejects writes".into(),
        ))
    }

    async fn ensure_master_token_personality(
        &self,
        _owner: &Owner,
        _master_token_id: uuid::Uuid,
    ) -> Result<MasterTokenPersonality, StorageError> {
        Err(StorageError::Internal(
            "mock: ensure_master_token_personality not stubbed".into(),
        ))
    }

    async fn ensure_subject_personality(
        &self,
        _owner: &Owner,
        _subject: &OwnerRef,
    ) -> Result<MasterTokenPersonality, StorageError> {
        Err(StorageError::Internal(
            "mock: ensure_subject_personality not stubbed".into(),
        ))
    }

    async fn append_personality_memories(
        &self,
        _req: &PersonalityWriteRequest<'_>,
    ) -> Result<PersonalityWriteOutcome, StorageError> {
        Err(StorageError::Internal(
            "RejectingStorage rejects writes".into(),
        ))
    }
}

#[async_trait::async_trait]
impl WakeConfigPort for RejectingStorage {
    async fn set_wake_entries(
        &self,
        _req: &SetWakeEntriesRequest,
    ) -> Result<SetWakeEntriesResponse, StorageError> {
        Err(StorageError::Internal(
            "RejectingStorage rejects writes".into(),
        ))
    }

    async fn set_wake_entries_within(
        &self,
        _owner: &Owner,
        _personality_instance_id: PersonalityInstanceId,
        _mutate: WakeEntriesMutator,
    ) -> Result<SetWakeEntriesResponse, StorageError> {
        Err(StorageError::Internal(
            "RejectingStorage rejects writes".into(),
        ))
    }
}

#[async_trait::async_trait]
impl FactRetentionPort for RejectingStorage {
    async fn upsert_fact_retention(
        &self,
        _owner: &Owner,
        _seconds: i64,
    ) -> Result<(), StorageError> {
        Err(StorageError::Internal(
            "RejectingStorage rejects writes".into(),
        ))
    }

    async fn get_fact_retention(&self, _owner: &Owner) -> Result<Option<i64>, StorageError> {
        Err(StorageError::Internal(
            "RejectingStorage rejects writes".into(),
        ))
    }

    async fn clear_fact_retention(&self, _owner: &Owner) -> Result<bool, StorageError> {
        Err(StorageError::Internal(
            "RejectingStorage rejects writes".into(),
        ))
    }
}

#[async_trait::async_trait]
impl ComplianceErasePort for RejectingStorage {
    async fn cleanup_due_facts(
        &self,
        _owner: &Owner,
        _fact_sidecar_tables: &[String],
        _edge_sidecar_tables: &[String],
        _citation_mapping_sidecar_tables: &[String],
        _cited_object_sidecar_tables: &[String],
    ) -> Result<CleanupDueFactsOutcome, StorageError> {
        Err(StorageError::Internal(
            "RejectingStorage rejects writes".into(),
        ))
    }

    async fn tombstone_fact(
        &self,
        _owner: &Owner,
        _fact_id: uuid::Uuid,
        _fact_sidecar_tables: &[String],
        _edge_sidecar_tables: &[String],
        _citation_mapping_sidecar_tables: &[String],
        _cited_object_sidecar_tables: &[String],
    ) -> Result<TombstoneFactOutcome, StorageError> {
        Err(StorageError::Internal(
            "RejectingStorage rejects writes".into(),
        ))
    }
}

#[async_trait::async_trait]
impl RegistryProjectionPort for RejectingStorage {
    async fn load_memory_batch_facts(
        &self,
        _owner: &Owner,
        _memory_id: crate::MemoryId,
        _sidecars: &[SidecarSpec],
    ) -> Result<Vec<crate::FactRow>, StorageError> {
        Ok(Vec::new())
    }

    async fn load_abstraction_heads(
        &self,
        _owner: &Owner,
        _sidecars: &[SidecarSpec],
        _limit: usize,
    ) -> Result<Vec<AbstractionRow>, StorageError> {
        Ok(Vec::new())
    }

    async fn load_perspective_heads(
        &self,
        _owner: &Owner,
        _instance: PersonalityInstanceId,
        _root_perspective_memory_id: crate::MemoryId,
        _sidecars: &[SidecarSpec],
        _limit: usize,
    ) -> Result<Vec<MemorySnapshot>, StorageError> {
        Ok(Vec::new())
    }

    async fn lookup_prior_personality_head(
        &self,
        _owner: &Owner,
        _instance: &PersonalityRef,
        _schema_id: &crate::SchemaId,
    ) -> Result<Option<crate::MemoryId>, StorageError> {
        Ok(None)
    }
}

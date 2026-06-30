#![allow(unused_variables)]

use proxima_core::storage_ports::*;
use proxima_core::verbs::change_history::{ChangeHistoryRequest, ChangeHistoryResponse};
use proxima_core::verbs::close_batch::CloseBatchOutcome;
use proxima_core::verbs::fact_cleanup::{CleanupDueFactsOutcome, TombstoneFactOutcome};
use proxima_core::verbs::goal_write::{
    AchieveGoalAtomicRequest, CreateGoalAtomicRequest, DecomposeGoalAtomicRequest,
    DecomposeGoalOutcome, GoalWriteOutcome, ModifyGoalAtomicRequest, TransitionGoalAtomicRequest,
};
use proxima_core::verbs::mcp_call_history::{McpCallHistoryRequest, McpCallHistoryResponse};
use proxima_core::*;

fn fake_error<T>() -> Result<T, StorageError> {
    Err(StorageError::Internal("storage port fake".into()))
}

#[derive(Debug)]
struct FactIngestFake;

#[async_trait::async_trait]
impl FactIngestPort for FactIngestFake {
    async fn ingest_fact_atomic(
        &self,
        owner: &Owner,
        draft: &FactWriteCommand,
        embedding_model_id: Option<&str>,
    ) -> Result<FactIngestOutcome, StorageError> {
        fake_error()
    }

    async fn ingest_fact_with_typed_sidecar(
        &self,
        authorized: &AuthorizedFactWrite,
        sidecar_payload: &SidecarPayload,
        embedding_model_id: Option<&str>,
    ) -> Result<FactIngestOutcome, StorageError> {
        fake_error()
    }

    async fn ingest_fact_with_citation_and_typed_sidecar(
        &self,
        authorized: &AuthorizedFactWithCitation,
        sidecar_payload: &SidecarPayload,
        embedding_model_id: Option<&str>,
    ) -> Result<FactIngestOutcome, StorageError> {
        fake_error()
    }
}

#[derive(Debug)]
struct OperatorMcpCallWriteFake;

#[async_trait::async_trait]
impl McpCallWritePort for OperatorMcpCallWriteFake {
    async fn persist_mcp_call_atomic(
        &self,
        input: &McpCallLogInput,
    ) -> Result<McpCallLogOutcome, StorageError> {
        fake_error()
    }
}

#[derive(Debug)]
struct OperatorMcpCallReadFake;

#[async_trait::async_trait]
impl McpCallReadPort for OperatorMcpCallReadFake {
    async fn read_mcp_call_history(
        &self,
        req: &McpCallHistoryRequest,
    ) -> Result<McpCallHistoryResponse, StorageError> {
        fake_error()
    }
}

#[derive(Debug)]
struct MemoryAuthoringFake;

#[async_trait::async_trait]
impl MemoryAuthoringPort for MemoryAuthoringFake {
    async fn author_derived(
        &self,
        req: &AuthorDerivedRequest<'_>,
    ) -> Result<AuthorDerivedOutcome, StorageError> {
        fake_error()
    }

    async fn append_memory_edge(
        &self,
        edge: &DerivedEdgeSpec<'_>,
        _proof: proxima_core::storage_ports::EdgeWriteProof,
    ) -> Result<EdgeId, StorageError> {
        fake_error()
    }

    async fn load_memory_kinds(
        &self,
        _owner: &Owner,
        _memory_ids: &[MemoryId],
    ) -> Result<Vec<MemoryKindRow>, StorageError> {
        fake_error()
    }

    async fn load_memory_edge_ids(
        &self,
        _owner: &Owner,
        _relation: &str,
        _source_memory_id: MemoryId,
        _target_memory_ids: &[MemoryId],
    ) -> Result<Vec<EdgeId>, StorageError> {
        fake_error()
    }
}

#[derive(Debug)]
struct MemoryReadFake;

#[async_trait::async_trait]
impl MemoryReadPort for MemoryReadFake {
    async fn load_fact_text(
        &self,
        owner: &Owner,
        memory_id: proxima_core::MemoryId,
    ) -> Result<Option<String>, StorageError> {
        fake_error()
    }

    async fn load_memory_graph_payloads(
        &self,
        _owner: &Owner,
        _memory_ids: &[MemoryId],
        _include_body: bool,
    ) -> Result<Vec<MemoryGraphPayloadRow>, StorageError> {
        fake_error()
    }

    async fn load_neighbor_memory_edges(
        &self,
        _read_owners: &[OwnerRef],
        _memory_ids: &[MemoryId],
        _limit: usize,
    ) -> Result<Vec<NeighborEdgeRow>, StorageError> {
        fake_error()
    }

    async fn load_edge_endpoint_kinds(
        &self,
        _edge_ids: &[EdgeId],
    ) -> Result<Vec<EdgeEndpointKindRow>, StorageError> {
        fake_error()
    }

    async fn query_memories(
        &self,
        req: &proxima_core::verbs::query::QueryRequest,
        schemas: &[proxima_core::verbs::schema::SchemaInfo],
    ) -> Result<proxima_core::verbs::query::QueryResponse, StorageError> {
        fake_error()
    }

    async fn search_memories(
        &self,
        req: &proxima_core::verbs::query::MemorySearchRequest,
        projections: &[proxima_core::verbs::schema::MemorySearchProjection],
    ) -> Result<Vec<proxima_core::verbs::query::MemorySearchResult>, StorageError> {
        fake_error()
    }

    async fn walk_memory_lineage(
        &self,
        read_owners: &[OwnerRef],
        req: &proxima_core::verbs::query::MemoryLineageRequest,
    ) -> Result<proxima_core::verbs::query::MemoryLineageResponse, StorageError> {
        fake_error()
    }
}

#[derive(Debug)]
struct MemoryInspectFake;

#[async_trait::async_trait]
impl MemoryInspectPort for MemoryInspectFake {
    async fn load_memory_by_id(
        &self,
        memory_id: proxima_core::MemoryId,
        sidecars: &[SidecarSpec],
    ) -> Result<Option<MemorySnapshot>, StorageError> {
        fake_error()
    }

    async fn list_memory_dependencies(
        &self,
        _owner: &Owner,
        _source_memory_id: proxima_core::MemoryId,
    ) -> Result<Vec<MemoryDependency>, StorageError> {
        fake_error()
    }
}

#[derive(Debug)]
struct EmbeddingTextFake;

#[async_trait::async_trait]
impl EmbeddingTextPort for EmbeddingTextFake {
    async fn load_embedding_text(
        &self,
        owner: &Owner,
        entity_kind: EntityKind,
        memory_id: proxima_core::MemoryId,
    ) -> Result<Option<String>, StorageError> {
        fake_error()
    }

    async fn list_facts_missing_embedding(
        &self,
        owner: &Owner,
        model_id: &str,
        limit: usize,
    ) -> Result<Vec<proxima_core::MemoryId>, StorageError> {
        fake_error()
    }
}

#[derive(Debug)]
struct EmbeddingWriteFake;

#[async_trait::async_trait]
impl EmbeddingWritePort for EmbeddingWriteFake {
    async fn upsert_fact_embedding(
        &self,
        owner: &Owner,
        memory_id: proxima_core::MemoryId,
        model_id: &str,
        dim: usize,
        vec: &[f32],
    ) -> Result<(), StorageError> {
        fake_error()
    }

    async fn upsert_memory_embedding(
        &self,
        owner: &Owner,
        entity_kind: EntityKind,
        memory_id: proxima_core::MemoryId,
        model_id: &str,
        dim: usize,
        vec: &[f32],
    ) -> Result<(), StorageError> {
        fake_error()
    }
}

#[derive(Debug)]
struct EmbeddingJobFake;

#[async_trait::async_trait]
impl EmbeddingJobPort for EmbeddingJobFake {
    async fn claim_pending_embedding_jobs(
        &self,
        model_id: &str,
        limit: i64,
    ) -> Result<Vec<EmbeddingJobClaim>, StorageError> {
        fake_error()
    }

    async fn complete_embedding_job(&self, claim: &EmbeddingJobClaim) -> Result<(), StorageError> {
        fake_error()
    }

    async fn fail_embedding_job(
        &self,
        claim: &EmbeddingJobClaim,
        error: &str,
    ) -> Result<(), StorageError> {
        fake_error()
    }

    async fn enqueue_missing_embedding_jobs(
        &self,
        owner: &Owner,
        model_id: &str,
        limit: i64,
    ) -> Result<u64, StorageError> {
        fake_error()
    }

    async fn count_pending_embedding_jobs(&self, owner: &Owner) -> Result<u64, StorageError> {
        fake_error()
    }
}

#[derive(Debug)]
struct GoalWriteFake;

#[async_trait::async_trait]
impl GoalWritePort for GoalWriteFake {
    async fn create_goal_atomic(
        &self,
        req: &CreateGoalAtomicRequest<'_>,
    ) -> Result<GoalWriteOutcome, StorageError> {
        fake_error()
    }

    async fn transition_goal_atomic(
        &self,
        req: &TransitionGoalAtomicRequest<'_>,
    ) -> Result<GoalWriteOutcome, StorageError> {
        fake_error()
    }

    async fn achieve_goal_atomic(
        &self,
        req: &AchieveGoalAtomicRequest<'_>,
    ) -> Result<GoalWriteOutcome, StorageError> {
        fake_error()
    }

    async fn modify_goal_atomic(
        &self,
        req: &ModifyGoalAtomicRequest<'_>,
    ) -> Result<GoalWriteOutcome, StorageError> {
        fake_error()
    }

    async fn decompose_goal_atomic(
        &self,
        req: &DecomposeGoalAtomicRequest<'_>,
    ) -> Result<DecomposeGoalOutcome, StorageError> {
        fake_error()
    }
}

#[derive(Debug)]
struct GoalReadFake;

#[async_trait::async_trait]
impl GoalReadPort for GoalReadFake {
    async fn list_active_goals(
        &self,
        read_owners: &[OwnerRef],
        self_perspective_memory_id: proxima_core::MemoryId,
        limit: usize,
    ) -> Result<Vec<ActiveGoalSummary>, StorageError> {
        fake_error()
    }
}

#[derive(Debug)]
struct ChangeEventFake;

#[async_trait::async_trait]
impl ChangeEventPort for ChangeEventFake {
    async fn change_history(
        &self,
        read_owners: &[OwnerRef],
        req: &ChangeHistoryRequest,
    ) -> Result<ChangeHistoryResponse, StorageError> {
        fake_error()
    }

    async fn list_change_events_after(
        &self,
        read_owners: &[OwnerRef],
        after: uuid::Uuid,
        limit: usize,
    ) -> Result<Vec<ChangeEventForWake>, StorageError> {
        fake_error()
    }

    async fn list_change_events_for_replay(
        &self,
        owner: &Owner,
        after: uuid::Uuid,
        until: Option<uuid::Uuid>,
        limit: usize,
    ) -> Result<Vec<ChangeEventForWake>, StorageError> {
        fake_error()
    }
}

#[derive(Debug)]
struct EdgeReadFake;

#[async_trait::async_trait]
impl EdgeReadPort for EdgeReadFake {
    async fn read_edges(
        &self,
        read_owners: &[OwnerRef],
        req: &proxima_core::verbs::query::EdgeReadRequest,
    ) -> Result<proxima_core::verbs::query::EdgeReadResponse, StorageError> {
        fake_error()
    }

    async fn edge_exists(
        &self,
        read_owners: &[OwnerRef],
        req: &proxima_core::verbs::query::EdgeExistsRequest,
    ) -> Result<proxima_core::verbs::query::EdgeExistsResponse, StorageError> {
        fake_error()
    }
}

#[derive(Debug)]
struct CitationFake;

#[async_trait::async_trait]
impl CitationPort for CitationFake {
    async fn fact_entity_id_for(
        &self,
        owner: &Owner,
        schema_id: &SchemaId,
        schema_version: SchemaVersion,
        natural_key: &[String],
    ) -> Result<Option<FactEntityId>, StorageError> {
        fake_error()
    }

    async fn facts_citing_object(
        &self,
        read_owners: &[OwnerRef],
        cited_object_id: uuid::Uuid,
        sidecars: &[SidecarSpec],
    ) -> Result<Vec<MemorySnapshot>, StorageError> {
        fake_error()
    }

    async fn citation_of_fact(
        &self,
        fact_memory_id: proxima_core::MemoryId,
    ) -> Result<Option<proxima_core::verbs::query::FactCitationReadback>, StorageError> {
        fake_error()
    }

    async fn citation_of_entity_head(
        &self,
        read_owners: &[OwnerRef],
        fact_entity_id: FactEntityId,
    ) -> Result<Option<proxima_core::verbs::query::FactCitationReadback>, StorageError> {
        fake_error()
    }
}

#[derive(Debug)]
struct OwnerAccessReadFake;

#[async_trait::async_trait]
impl OwnerAccessReadPort for OwnerAccessReadFake {
    async fn resolve_membership(
        &self,
        member: &OwnerRef,
    ) -> Result<Vec<MembershipRow>, StorageError> {
        fake_error()
    }

    async fn visible_to_any(
        &self,
        entity: EntityId,
        read_owners: &[OwnerRef],
    ) -> Result<bool, StorageError> {
        fake_error()
    }

    async fn home_owner(&self, entity: EntityId) -> Result<Option<OwnerRef>, StorageError> {
        fake_error()
    }
}

#[derive(Debug)]
struct OwnerMembershipAdminFake;

#[async_trait::async_trait]
impl OwnerMembershipAdminPort for OwnerMembershipAdminFake {
    async fn add_group_member(
        &self,
        group_id: GroupId,
        member_user_id: UserId,
        relation: Relation,
        granted_by: uuid::Uuid,
    ) -> Result<(), StorageError> {
        fake_error()
    }

    async fn remove_group_member(
        &self,
        group_id: GroupId,
        member_user_id: UserId,
    ) -> Result<(), StorageError> {
        fake_error()
    }

    async fn list_group_members(
        &self,
        group_id: GroupId,
    ) -> Result<Vec<(UserId, Relation)>, StorageError> {
        fake_error()
    }
}

#[derive(Debug)]
struct SourceBatchFake;

#[async_trait::async_trait]
impl SourceBatchPort for SourceBatchFake {
    async fn close_batch(
        &self,
        principal: &OwnerRef,
        source_batch_id: SourceBatchId,
    ) -> Result<CloseBatchOutcome, StorageError> {
        fake_error()
    }
}

#[derive(Debug)]
struct FactRetentionFake;

#[async_trait::async_trait]
impl FactRetentionPort for FactRetentionFake {
    async fn upsert_fact_retention(&self, owner: &Owner, seconds: i64) -> Result<(), StorageError> {
        fake_error()
    }

    async fn get_fact_retention(&self, owner: &Owner) -> Result<Option<i64>, StorageError> {
        fake_error()
    }

    async fn clear_fact_retention(&self, owner: &Owner) -> Result<bool, StorageError> {
        fake_error()
    }
}

#[derive(Debug)]
struct ComplianceEraseFake;

#[async_trait::async_trait]
impl ComplianceErasePort for ComplianceEraseFake {
    async fn cleanup_due_facts(
        &self,
        owner: &Owner,
        fact_sidecar_tables: &[String],
        edge_sidecar_tables: &[String],
        citation_mapping_sidecar_tables: &[String],
        cited_object_sidecar_tables: &[String],
    ) -> Result<CleanupDueFactsOutcome, StorageError> {
        fake_error()
    }

    async fn tombstone_fact(
        &self,
        owner: &Owner,
        fact_id: uuid::Uuid,
        fact_sidecar_tables: &[String],
        edge_sidecar_tables: &[String],
        citation_mapping_sidecar_tables: &[String],
        cited_object_sidecar_tables: &[String],
    ) -> Result<TombstoneFactOutcome, StorageError> {
        fake_error()
    }
}

#[derive(Debug)]
struct RegistryProjectionFake;

#[async_trait::async_trait]
impl RegistryProjectionPort for RegistryProjectionFake {
    async fn load_memory_batch_facts(
        &self,
        owner: &Owner,
        memory_id: proxima_core::MemoryId,
        sidecars: &[SidecarSpec],
    ) -> Result<Vec<proxima_core::FactRow>, StorageError> {
        fake_error()
    }

    async fn load_abstraction_heads(
        &self,
        owner: &Owner,
        sidecars: &[SidecarSpec],
        limit: usize,
    ) -> Result<Vec<AbstractionRow>, StorageError> {
        fake_error()
    }
}

fn assert_port<T: Send + Sync + 'static>() {}

#[test]
fn public_storage_ports_can_be_mocked_independently() {
    assert_port::<FactIngestFake>();
    assert_port::<OperatorMcpCallWriteFake>();
    assert_port::<OperatorMcpCallReadFake>();
    assert_port::<MemoryAuthoringFake>();
    assert_port::<MemoryReadFake>();
    assert_port::<MemoryInspectFake>();
    assert_port::<EmbeddingTextFake>();
    assert_port::<EmbeddingWriteFake>();
    assert_port::<EmbeddingJobFake>();
    assert_port::<GoalWriteFake>();
    assert_port::<GoalReadFake>();
    assert_port::<ChangeEventFake>();
    assert_port::<EdgeReadFake>();
    assert_port::<CitationFake>();
    assert_port::<OwnerAccessReadFake>();
    assert_port::<OwnerMembershipAdminFake>();
    assert_port::<SourceBatchFake>();
    assert_port::<FactRetentionFake>();
    assert_port::<ComplianceEraseFake>();
    assert_port::<RegistryProjectionFake>();
}

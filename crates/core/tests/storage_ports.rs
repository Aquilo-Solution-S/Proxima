#![allow(unused_variables)]

use proxima_core::storage_ports::*;
use proxima_core::verbs::change_history::{ChangeHistoryRequest, ChangeHistoryResponse};

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
        _permit: &OwnerWritePermit,
        draft: &FactWriteCommand,
        embedding_model_id: Option<&str>,
    ) -> Result<FactIngestOutcome, StorageError> {
        fake_error()
    }

    async fn ingest_fact_with_typed_sidecar(
        &self,
        authorized: &AuthorizedFactWrite,
        sidecar_payloads: &[SidecarPayload],
        embedding_model_id: Option<&str>,
    ) -> Result<FactIngestOutcome, StorageError> {
        fake_error()
    }

    async fn ingest_fact_with_citation_and_typed_sidecar(
        &self,
        authorized: &AuthorizedFactWithCitation,
        sidecar_payloads: &[SidecarPayload],
        embedding_model_id: Option<&str>,
    ) -> Result<FactIngestOutcome, StorageError> {
        fake_error()
    }

    async fn ingest_fact_with_citation_ref_and_typed_sidecar(
        &self,
        authorized: &proxima_core::verbs::fact_ingest::AuthorizedFactWithCitationRef,
        sidecar_payloads: &[SidecarPayload],
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
        _permit: &OwnerWritePermit,
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
        _permit: &OwnerWritePermit,
        _proof: proxima_core::storage_ports::OperatorWriteProof,
    ) -> Result<AuthorDerivedOutcome, StorageError> {
        fake_error()
    }

    async fn load_memory_kinds(
        &self,
        _owner: &Owner,
        _memory_ids: &[MemoryId],
    ) -> Result<Vec<MemoryKindRow>, StorageError> {
        fake_error()
    }

    async fn load_fact_source_batches(
        &self,
        _owner: &Owner,
        _memory_ids: &[MemoryId],
    ) -> Result<Vec<FactSourceBatchRow>, StorageError> {
        fake_error()
    }

    async fn forget_memory(
        &self,
        _permit: &OwnerWritePermit,
        _memory_id: MemoryId,
    ) -> Result<(), StorageError> {
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
        _identities: &[MemoryGraphIdentity],
        _include_body: bool,
    ) -> Result<Vec<MemoryGraphPayloadRow>, StorageError> {
        fake_error()
    }

    async fn load_sketches(
        &self,
        _read_owners: &[OwnerRef],
        _memory_ids: &[MemoryId],
    ) -> Result<Vec<proxima_core::read_models::MemorySketch>, StorageError> {
        fake_error()
    }

    async fn load_pin_nodes(
        &self,
        _read_owners: &[OwnerRef],
        _memory_ids: &[MemoryId],
    ) -> Result<Vec<proxima_core::PinNode>, StorageError> {
        fake_error()
    }

    async fn load_inbound_pin_nodes(
        &self,
        _read_owners: &[OwnerRef],
        _query: proxima_core::InboundPinQuery<'_>,
    ) -> Result<Vec<proxima_core::PinNode>, StorageError> {
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
    ) -> Result<proxima_core::verbs::query::MemorySearchPage, StorageError> {
        fake_error()
    }

    async fn walk_memory_lineage(
        &self,
        read_owners: &[OwnerRef],
        req: &proxima_core::verbs::query::MemoryLineageRequest,
    ) -> Result<proxima_core::verbs::query::MemoryLineageResponse, StorageError> {
        fake_error()
    }

    async fn owned_series_handle(
        &self,
        _owner: Owner,
        _schema_id: &proxima_core::SchemaId,
        _sidecar_table: &str,
        _columns: &[(&str, proxima_core::verbs::query::SidecarAtom)],
    ) -> Result<Option<uuid::Uuid>, StorageError> {
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

    async fn load_memories_by_ids(
        &self,
        _read_owners: &[OwnerRef],
        _memory_ids: &[proxima_core::MemoryId],
        _sidecars: &[SidecarSpec],
    ) -> Result<Vec<MemorySnapshot>, StorageError> {
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
        non_embeddable_schemas: &[String],
    ) -> Result<Option<String>, StorageError> {
        fake_error()
    }

    async fn load_embedding_texts(
        &self,
        _items: &[(Owner, EntityKind, proxima_core::MemoryId)],
        _non_embeddable_schemas: &[String],
    ) -> Result<Vec<Option<String>>, StorageError> {
        fake_error()
    }

    async fn list_facts_missing_embedding(
        &self,
        owner: &Owner,
        model_id: &str,
        limit: usize,
        _non_embeddable_schemas: &[String],
    ) -> Result<Vec<proxima_core::MemoryId>, StorageError> {
        fake_error()
    }
}

#[derive(Debug)]
struct EmbeddingWriteFake;

#[async_trait::async_trait]
impl EmbeddingWritePort for EmbeddingWriteFake {
    async fn insert_embedding(
        &self,
        _owner: &Owner,
        _entity: proxima_core::EmbeddableEntityRef,
        model_id: &str,
        dim: usize,
        vec: &[f32],
        _proof: proxima_core::storage_ports::EmbeddingWriteProof,
    ) -> Result<proxima_core::EmbeddingWriteOutcome, StorageError> {
        let _ = (model_id, dim, vec);
        fake_error()
    }

    async fn insert_embedding_chunks(
        &self,
        _owner: &Owner,
        _entity: proxima_core::EmbeddableEntityRef,
        model_id: &str,
        dim: usize,
        chunks: &[&[f32]],
        _proof: proxima_core::storage_ports::EmbeddingWriteProof,
    ) -> Result<proxima_core::EmbeddingWriteOutcome, StorageError> {
        let _ = (model_id, dim, chunks);
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

    async fn fail_embedding_job_permanently(
        &self,
        claim: &EmbeddingJobClaim,
        error: &str,
    ) -> Result<(), StorageError> {
        let _ = (claim, error);
        fake_error()
    }

    async fn release_embedding_jobs(
        &self,
        claims: &[EmbeddingJobClaim],
        error: &str,
    ) -> Result<(), StorageError> {
        let _ = (claims, error);
        fake_error()
    }

    async fn enqueue_missing_embedding_jobs(
        &self,
        _permit: &OwnerWritePermit,
        model_id: &str,
        limit: i64,
        _non_embeddable_schemas: &[String],
    ) -> Result<u64, StorageError> {
        fake_error()
    }

    async fn count_pending_embedding_jobs(&self, owner: &Owner) -> Result<u64, StorageError> {
        fake_error()
    }

    async fn count_failed_embedding_jobs(&self, owner: &Owner) -> Result<u64, StorageError> {
        fake_error()
    }
}

#[async_trait::async_trait]
impl EmbeddingMaintenancePort for EmbeddingJobFake {
    async fn embedding_ann_observability(
        &self,
        proof: OperatorMaintenanceProof,
    ) -> Result<EmbeddingAnnObservability, StorageError> {
        let _ = proof;
        fake_error()
    }

    async fn sweep_orphan_embedding_rows(
        &self,
        proof: OperatorMaintenanceProof,
    ) -> Result<EmbeddingOrphanSweepOutcome, StorageError> {
        let _ = proof;
        fake_error()
    }

    async fn reconcile_embeddings(
        &self,
        options: proxima_core::EmbeddingReconcileOptions<'_>,
        proof: OperatorMaintenanceProof,
    ) -> Result<proxima_core::EmbeddingReconcileOutcome, StorageError> {
        let _ = (options, proof);
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
        _permit: &OwnerWritePermit,
    ) -> Result<GoalWriteOutcome, StorageError> {
        fake_error()
    }

    async fn transition_goal_atomic(
        &self,
        req: &TransitionGoalAtomicRequest<'_>,
        _permit: &OwnerWritePermit,
    ) -> Result<GoalWriteOutcome, StorageError> {
        fake_error()
    }

    async fn achieve_goal_atomic(
        &self,
        req: &AchieveGoalAtomicRequest<'_>,
        _permit: &OwnerWritePermit,
    ) -> Result<GoalWriteOutcome, StorageError> {
        fake_error()
    }

    async fn modify_goal_atomic(
        &self,
        req: &ModifyGoalAtomicRequest<'_>,
        _permit: &OwnerWritePermit,
    ) -> Result<GoalWriteOutcome, StorageError> {
        fake_error()
    }

    async fn decompose_goal_atomic(
        &self,
        req: &DecomposeGoalAtomicRequest<'_>,
        _permit: &OwnerWritePermit,
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

    async fn load_goal_wake_configs(
        &self,
        read_owners: &[OwnerRef],
        goal_ids: &[proxima_core::GoalId],
    ) -> Result<Vec<proxima_core::read_models::GoalWakeConfigRow>, StorageError> {
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
struct CitationFake;

#[async_trait::async_trait]
impl CitationPort for CitationFake {
    async fn facts_citing_object(
        &self,
        read_owners: &[OwnerRef],
        cited_object_id: uuid::Uuid,
        sidecars: &[SidecarSpec],
        _after: Option<proxima_core::verbs::query::FactCitationCursor>,
        _limit: u32,
    ) -> Result<proxima_core::verbs::query::FactCitationPage, StorageError> {
        fake_error()
    }

    async fn citation_of_fact(
        &self,
        read_owners: &[proxima_core::OwnerRef],
        fact_memory_id: proxima_core::MemoryId,
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

    async fn visible_home_owner(
        &self,
        entity: EntityId,
        read_owners: &[OwnerRef],
    ) -> Result<Option<OwnerRef>, StorageError> {
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
    async fn bootstrap_group_admin(
        &self,
        group_id: GroupId,
        first_admin_user_id: UserId,
        granted_by: uuid::Uuid,
    ) -> Result<(), StorageError> {
        let _ = (group_id, first_admin_user_id, granted_by);
        fake_error()
    }

    async fn add_group_member(
        &self,
        _permit: &OwnerWritePermit,
        group_id: GroupId,
        member_user_id: UserId,
        relation: Relation,
        granted_by: uuid::Uuid,
    ) -> Result<(), StorageError> {
        fake_error()
    }

    async fn remove_group_member(
        &self,
        _permit: &OwnerWritePermit,
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

    async fn list_group_members_page(
        &self,
        _group_id: GroupId,
        _after: Option<(UserId, Relation)>,
        _limit: i64,
    ) -> Result<Vec<(UserId, Relation)>, StorageError> {
        fake_error()
    }
}

#[derive(Debug)]
struct SourceBatchFake;

impl SourceBatchPort for SourceBatchFake {}

#[derive(Debug)]
struct SourceCursorFake;

#[async_trait::async_trait]
impl SourceCursorPort for SourceCursorFake {
    async fn load_source_cursor(
        &self,
        owner: &Owner,
        source: &str,
    ) -> Result<Option<Cursor>, StorageError> {
        fake_error()
    }

    async fn store_source_cursor(
        &self,
        _permit: &OwnerWritePermit,
        source: &str,
        cursor: &Cursor,
    ) -> Result<(), StorageError> {
        fake_error()
    }

    async fn source_cursor_age(
        &self,
        owner: &Owner,
        source: &str,
    ) -> Result<Option<std::time::Duration>, StorageError> {
        fake_error()
    }
}

#[derive(Debug)]
struct FactRetentionFake;

#[async_trait::async_trait]
impl FactRetentionPort for FactRetentionFake {
    async fn upsert_fact_retention(
        &self,
        _permit: &OwnerWritePermit,
        seconds: i64,
    ) -> Result<(), StorageError> {
        fake_error()
    }

    async fn get_fact_retention(&self, owner: &Owner) -> Result<Option<i64>, StorageError> {
        fake_error()
    }

    async fn clear_fact_retention(&self, _permit: &OwnerWritePermit) -> Result<bool, StorageError> {
        fake_error()
    }

    async fn set_legal_hold(&self, _permit: &OwnerWritePermit) -> Result<(), StorageError> {
        fake_error()
    }

    async fn get_legal_hold(&self, owner: &Owner) -> Result<bool, StorageError> {
        fake_error()
    }

    async fn clear_legal_hold(&self, _permit: &OwnerWritePermit) -> Result<bool, StorageError> {
        fake_error()
    }
}

#[derive(Debug)]
struct ComplianceEraseFake;

#[async_trait::async_trait]
impl ComplianceErasePort for ComplianceEraseFake {
    async fn record_compliance_outcome(
        &self,
        _audit: &proxima_core::compliance::ComplianceAuditContext,
        _outcome: &proxima_core::compliance::ComplianceEraseOutcome,
    ) -> Result<(), StorageError> {
        fake_error()
    }

    async fn erase_group_owner_if_abandoned(
        &self,
        _auth: &proxima_core::compliance::EraseAuthorization,
        _group_id: GroupId,
        _object_purge_planned: bool,
        _fact_sidecar_tables: &[String],
        _goal_sidecar_tables: &[String],
        _citation_mapping_sidecar_tables: &[String],
        _cited_object_sidecar_tables: &[String],
    ) -> Result<proxima_core::compliance::ComplianceEraseOutcome, StorageError> {
        fake_error()
    }

    async fn erase_personal_owner_if_drop_verified(
        &self,
        _auth: &proxima_core::compliance::EraseAuthorization,
        _user_id: UserId,
        _object_purge_planned: bool,
        _fact_sidecar_tables: &[String],
        _goal_sidecar_tables: &[String],
        _citation_mapping_sidecar_tables: &[String],
        _cited_object_sidecar_tables: &[String],
    ) -> Result<proxima_core::compliance::ComplianceEraseOutcome, StorageError> {
        fake_error()
    }

    async fn erase_group_source_scope_if_owner_abandoned(
        &self,
        _auth: &proxima_core::compliance::EraseAuthorization,
        _group_id: GroupId,
        _source_id: &SourceId,
        _fact_sidecar_tables: &[String],
        _goal_sidecar_tables: &[String],
        _citation_mapping_sidecar_tables: &[String],
        _cited_object_sidecar_tables: &[String],
    ) -> Result<proxima_core::compliance::ComplianceEraseOutcome, StorageError> {
        fake_error()
    }

    async fn erase_personal_source_scope_if_drop_verified(
        &self,
        _auth: &proxima_core::compliance::EraseAuthorization,
        _user_id: UserId,
        _source_id: &SourceId,
        _fact_sidecar_tables: &[String],
        _goal_sidecar_tables: &[String],
        _citation_mapping_sidecar_tables: &[String],
        _cited_object_sidecar_tables: &[String],
    ) -> Result<proxima_core::compliance::ComplianceEraseOutcome, StorageError> {
        fake_error()
    }

    async fn export_owner_bundle(
        &self,
        _auth: &proxima_core::compliance::ExportAuthorization,
        _fact_sidecar_tables: &[String],
        _goal_sidecar_tables: &[String],
        _citation_mapping_sidecar_tables: &[String],
        _cited_object_sidecar_tables: &[String],
    ) -> Result<proxima_core::compliance::ComplianceExportBundle, StorageError> {
        fake_error()
    }

    async fn clear_cited_object_purge_pending(
        &self,
        _operation_id: uuid::Uuid,
    ) -> Result<(), StorageError> {
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
    assert_port::<CitationFake>();
    assert_port::<OwnerAccessReadFake>();
    assert_port::<OwnerMembershipAdminFake>();
    assert_port::<SourceBatchFake>();
    assert_port::<SourceCursorFake>();
    assert_port::<FactRetentionFake>();
    assert_port::<ComplianceEraseFake>();
    assert_port::<RegistryProjectionFake>();
}

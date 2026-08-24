use super::access::{OwnerAccessReadPort, OwnerMembershipAdminPort, OwnerTransferPort};
use super::change::ChangeEventPort;
use super::cursors::SourceCursorPort;
use super::embeddings::{
    EmbeddingJobPort, EmbeddingMaintenancePort, EmbeddingTextPort, EmbeddingWriteOutcome,
    EmbeddingWritePort, EmbeddingWriteProof,
};
use super::fact::{FactIngestPort, SourceBatchPort};
use super::goals::{GoalReadPort, GoalWakeCandidatePort, GoalWritePort};
use super::mcp::McpCallReadPort;
use super::memory::{
    CitationPort, MemoryAuthoringPort, MemoryInspectPort, MemoryReadPort, OperatorWriteProof,
};
use super::owner_inverse::{OwnerDropProofPort, OwnerEraseAuthorityPort, OwnerInversePort};
use super::proof::{OperatorMaintenanceProof, OwnerWritePermit};
use super::registry::RegistryProjectionPort;
use super::write_session::{WriteSession, WriteSessionFactory};

use crate::access::AccessError;
use crate::owner_inverse::OwnerEraseTarget;
use crate::read_models::{
    AbstractionRow, ActiveGoalSummary, ChangeEventForWake, FactRow, GoalWakeCandidate,
    GoalWakeCandidateRequest, MemorySnapshot, SidecarSpec,
};
use crate::storage::{AuthorDerivedOutcome, AuthorDerivedRequest, EmbeddingJobClaim, StorageError};
use crate::verbs::change_history::{ChangeHistoryRequest, ChangeHistoryResponse};

use crate::verbs::fact_ingest::{
    AuthorizedFactWithCitation, AuthorizedFactWithCitationRef, AuthorizedFactWrite,
    FactIngestOutcome, FactWriteCommand,
};
use crate::verbs::goal_write::{
    AchieveGoalAtomicRequest, CreateGoalAtomicRequest, DecomposeGoalAtomicRequest,
    DecomposeGoalOutcome, GoalWriteOutcome, ModifyGoalAtomicRequest, TransitionGoalAtomicRequest,
};
use crate::verbs::mcp_call_history::{McpCallHistoryRequest, McpCallHistoryResponse};
use crate::{
    EmbeddableEntityRef, EntityId, EntityKind, GroupId, MembershipRow, Owner, OwnerRef, Relation,
    SidecarPayload, SourceId, UserId,
};

#[derive(Debug)]
pub(super) struct RejectingStorage;

#[async_trait::async_trait]
impl FactIngestPort for RejectingStorage {
    async fn ingest_fact_atomic(
        &self,
        _permit: &OwnerWritePermit,
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
        _sidecar_payloads: &[SidecarPayload],
        _embedding_model_id: Option<&str>,
    ) -> Result<FactIngestOutcome, StorageError> {
        Err(StorageError::Internal(
            "RejectingStorage rejects writes".into(),
        ))
    }

    async fn ingest_fact_with_citation_and_typed_sidecar(
        &self,
        _authorized: &AuthorizedFactWithCitation,
        _sidecar_payloads: &[SidecarPayload],
        _embedding_model_id: Option<&str>,
    ) -> Result<FactIngestOutcome, StorageError> {
        Err(StorageError::Internal(
            "RejectingStorage rejects writes".into(),
        ))
    }

    async fn ingest_fact_with_citation_ref_and_typed_sidecar(
        &self,
        _authorized: &AuthorizedFactWithCitationRef,
        _sidecar_payloads: &[SidecarPayload],
        _embedding_model_id: Option<&str>,
    ) -> Result<FactIngestOutcome, StorageError> {
        Err(StorageError::Internal(
            "RejectingStorage rejects writes".into(),
        ))
    }
}

#[async_trait::async_trait]
impl McpCallReadPort for RejectingStorage {
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
        _permit: &OwnerWritePermit,
        _proof: OperatorWriteProof,
    ) -> Result<AuthorDerivedOutcome, StorageError> {
        Err(StorageError::Internal(
            "RejectingStorage rejects writes".into(),
        ))
    }

    async fn load_memory_kinds(
        &self,
        _owner: &Owner,
        _memory_ids: &[crate::MemoryId],
    ) -> Result<Vec<crate::MemoryKindRow>, StorageError> {
        Ok(Vec::new())
    }

    async fn load_fact_source_batches(
        &self,
        _owner: &Owner,
        _memory_ids: &[crate::MemoryId],
    ) -> Result<Vec<crate::FactSourceBatchRow>, StorageError> {
        Ok(Vec::new())
    }

    async fn forget_memory(
        &self,
        _permit: &OwnerWritePermit,
        _memory_id: crate::MemoryId,
    ) -> Result<(), StorageError> {
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
            next_cursor: None,
            seq_high_water: None,
        })
    }

    async fn search_memories(
        &self,
        _req: &crate::verbs::query::MemorySearchRequest,
        _projections: &[crate::verbs::schema::MemorySearchProjection],
    ) -> Result<crate::verbs::query::MemorySearchPage, StorageError> {
        Ok(crate::verbs::query::MemorySearchPage {
            results: Vec::new(),
            has_more: false,
        })
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
            next_cursor: None,
        })
    }

    async fn load_memory_graph_payloads(
        &self,
        _identities: &[crate::MemoryGraphIdentity],
        _include_body: bool,
    ) -> Result<Vec<crate::MemoryGraphPayloadRow>, StorageError> {
        Ok(Vec::new())
    }

    async fn load_sketches(
        &self,
        _read_owners: &[OwnerRef],
        _memory_ids: &[crate::MemoryId],
    ) -> Result<Vec<crate::read_models::MemorySketch>, StorageError> {
        Ok(Vec::new())
    }

    async fn load_pin_nodes(
        &self,
        _read_owners: &[OwnerRef],
        _memory_ids: &[crate::MemoryId],
    ) -> Result<Vec<crate::PinNode>, StorageError> {
        Ok(Vec::new())
    }

    async fn load_inbound_pin_nodes(
        &self,
        _read_owners: &[OwnerRef],
        _query: crate::InboundPinQuery<'_>,
    ) -> Result<Vec<crate::PinNode>, StorageError> {
        Ok(Vec::new())
    }

    async fn owned_series_handle(
        &self,
        _owner: crate::Owner,
        _schema_id: &crate::SchemaId,
        _sidecar_table: &str,
        _columns: &[(&str, crate::verbs::query::SidecarAtom)],
    ) -> Result<Option<uuid::Uuid>, StorageError> {
        Ok(None)
    }
}

#[async_trait::async_trait]
impl MemoryInspectPort for RejectingStorage {
    async fn load_memory_by_id(
        &self,
        _memory_id: crate::MemoryId,
        _sidecars: &[SidecarSpec],
    ) -> Result<Option<MemorySnapshot>, StorageError> {
        Ok(None)
    }

    async fn load_memories_by_ids(
        &self,
        _read_owners: &[OwnerRef],
        _memory_ids: &[crate::MemoryId],
        _sidecars: &[SidecarSpec],
    ) -> Result<Vec<MemorySnapshot>, StorageError> {
        Ok(Vec::new())
    }
}

#[async_trait::async_trait]
impl EmbeddingTextPort for RejectingStorage {
    async fn load_embedding_text(
        &self,
        _owner: &Owner,
        _entity_kind: EntityKind,
        _memory_id: crate::MemoryId,
        _non_embeddable_schemas: &[String],
    ) -> Result<Option<String>, StorageError> {
        Ok(None)
    }

    async fn load_embedding_texts(
        &self,
        items: &[(Owner, EntityKind, crate::MemoryId)],
        _non_embeddable_schemas: &[String],
    ) -> Result<Vec<Option<String>>, StorageError> {
        Ok(vec![None; items.len()])
    }

    async fn list_facts_missing_embedding(
        &self,
        _owner: &Owner,
        _model_id: &str,
        _limit: usize,
        _non_embeddable_schemas: &[String],
    ) -> Result<Vec<crate::MemoryId>, StorageError> {
        Ok(Vec::new())
    }
}

#[async_trait::async_trait]
impl EmbeddingWritePort for RejectingStorage {
    async fn insert_embedding(
        &self,
        _owner: &Owner,
        _entity: EmbeddableEntityRef,
        _model_id: &str,
        _dim: usize,
        _vec: &[f32],
        _proof: EmbeddingWriteProof,
    ) -> Result<EmbeddingWriteOutcome, StorageError> {
        Ok(EmbeddingWriteOutcome {
            embedding_version: 0,
        })
    }

    async fn insert_embedding_chunks(
        &self,
        _owner: &Owner,
        _entity: EmbeddableEntityRef,
        _model_id: &str,
        _dim: usize,
        _chunks: &[&[f32]],
        _proof: EmbeddingWriteProof,
    ) -> Result<EmbeddingWriteOutcome, StorageError> {
        Ok(EmbeddingWriteOutcome {
            embedding_version: 0,
        })
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

    async fn renew_embedding_jobs(
        &self,
        _claims: &[EmbeddingJobClaim],
    ) -> Result<u64, StorageError> {
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

    async fn fail_embedding_job_permanently(
        &self,
        _claim: &EmbeddingJobClaim,
        _error: &str,
    ) -> Result<(), StorageError> {
        Err(StorageError::Internal(
            "RejectingStorage rejects writes".into(),
        ))
    }

    async fn release_embedding_jobs(
        &self,
        _claims: &[EmbeddingJobClaim],
        _error: &str,
    ) -> Result<(), StorageError> {
        Err(StorageError::Internal(
            "RejectingStorage rejects writes".into(),
        ))
    }

    async fn enqueue_missing_embedding_jobs(
        &self,
        _permit: &OwnerWritePermit,
        _model_id: &str,
        _limit: i64,
        _non_embeddable_schemas: &[String],
    ) -> Result<u64, StorageError> {
        Err(StorageError::Internal(
            "RejectingStorage rejects writes".into(),
        ))
    }

    async fn count_pending_embedding_jobs(&self, _owner: &Owner) -> Result<u64, StorageError> {
        Ok(0)
    }

    async fn count_failed_embedding_jobs(&self, _owner: &Owner) -> Result<u64, StorageError> {
        Ok(0)
    }
}

#[async_trait::async_trait]
impl EmbeddingMaintenancePort for RejectingStorage {
    async fn embedding_ann_observability(
        &self,
        _policy: crate::EmbeddingRuntimePolicy,
        _proof: OperatorMaintenanceProof,
    ) -> Result<super::embeddings::EmbeddingAnnObservability, StorageError> {
        Err(StorageError::Internal(
            "RejectingStorage rejects operational embedding reads".into(),
        ))
    }

    async fn sweep_orphan_embedding_rows(
        &self,
        _proof: OperatorMaintenanceProof,
    ) -> Result<super::embeddings::EmbeddingOrphanSweepOutcome, StorageError> {
        Err(StorageError::Internal(
            "RejectingStorage rejects embedding maintenance".into(),
        ))
    }

    async fn reconcile_embeddings(
        &self,
        _options: super::embeddings::EmbeddingReconcileOptions<'_>,
        _policy: crate::EmbeddingRuntimePolicy,
        _proof: OperatorMaintenanceProof,
    ) -> Result<super::embeddings::EmbeddingReconcileOutcome, StorageError> {
        Err(StorageError::Internal(
            "RejectingStorage rejects embedding maintenance".into(),
        ))
    }
}

#[async_trait::async_trait]
impl GoalWritePort for RejectingStorage {
    async fn create_goal_atomic(
        &self,
        _req: &CreateGoalAtomicRequest<'_>,
        _permit: &OwnerWritePermit,
    ) -> Result<GoalWriteOutcome, StorageError> {
        Err(StorageError::Internal(
            "RejectingStorage rejects writes".into(),
        ))
    }

    async fn transition_goal_atomic(
        &self,
        _req: &TransitionGoalAtomicRequest<'_>,
        _permit: &OwnerWritePermit,
    ) -> Result<GoalWriteOutcome, StorageError> {
        Err(StorageError::Internal(
            "RejectingStorage rejects writes".into(),
        ))
    }

    async fn achieve_goal_atomic(
        &self,
        _req: &AchieveGoalAtomicRequest<'_>,
        _permit: &OwnerWritePermit,
    ) -> Result<GoalWriteOutcome, StorageError> {
        Err(StorageError::Internal(
            "RejectingStorage rejects writes".into(),
        ))
    }

    async fn modify_goal_atomic(
        &self,
        _req: &ModifyGoalAtomicRequest<'_>,
        _permit: &OwnerWritePermit,
    ) -> Result<GoalWriteOutcome, StorageError> {
        Err(StorageError::Internal(
            "RejectingStorage rejects writes".into(),
        ))
    }

    async fn decompose_goal_atomic(
        &self,
        _req: &DecomposeGoalAtomicRequest<'_>,
        _permit: &OwnerWritePermit,
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

    async fn load_goal_wake_configs(
        &self,
        _read_owners: &[OwnerRef],
        _goal_ids: &[crate::GoalId],
    ) -> Result<Vec<crate::read_models::GoalWakeConfigRow>, StorageError> {
        Ok(Vec::new())
    }
}

#[async_trait::async_trait]
impl GoalWakeCandidatePort for RejectingStorage {
    async fn list_goal_wake_candidates(
        &self,
        _req: &GoalWakeCandidateRequest<'_>,
    ) -> Result<Vec<GoalWakeCandidate>, StorageError> {
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
impl CitationPort for RejectingStorage {
    async fn facts_citing_object(
        &self,
        _read_owners: &[OwnerRef],
        _cited_object_id: uuid::Uuid,
        _sidecars: &[SidecarSpec],
        _after: Option<crate::verbs::query::FactCitationCursor>,
        _limit: u32,
    ) -> Result<crate::verbs::query::FactCitationPage, StorageError> {
        Ok(crate::verbs::query::FactCitationPage {
            facts: Vec::new(),
            next_cursor: None,
            has_more: false,
        })
    }

    async fn citation_of_fact(
        &self,
        _read_owners: &[OwnerRef],
        _fact_memory_id: crate::MemoryId,
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

    async fn visible_home_owner(
        &self,
        _entity: EntityId,
        _read_owners: &[OwnerRef],
    ) -> Result<Option<OwnerRef>, StorageError> {
        Ok(None)
    }

    async fn home_owner(&self, _entity: EntityId) -> Result<Option<OwnerRef>, StorageError> {
        Ok(None)
    }
}

#[async_trait::async_trait]
impl OwnerMembershipAdminPort for RejectingStorage {
    async fn bootstrap_group_admin(
        &self,
        _group_id: GroupId,
        _first_admin_user_id: UserId,
        _granted_by: uuid::Uuid,
    ) -> Result<(), StorageError> {
        Err(StorageError::Internal(
            "RejectingStorage rejects writes".into(),
        ))
    }

    async fn add_group_member(
        &self,
        _permit: &OwnerWritePermit,
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
        _permit: &OwnerWritePermit,
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

    async fn list_group_members_page(
        &self,
        _group_id: GroupId,
        _after: Option<(UserId, Relation)>,
        _limit: i64,
    ) -> Result<Vec<(UserId, Relation)>, StorageError> {
        Ok(Vec::new())
    }
}

#[async_trait::async_trait]
impl OwnerTransferPort for RejectingStorage {
    async fn transfer_to_owner(
        &self,
        _permit: &OwnerWritePermit,
        _entity: EntityId,
        _to_owner: OwnerRef,
        _surfaces: &crate::owner_inverse::OwnerSurfaces,
    ) -> Result<bool, StorageError> {
        Err(StorageError::Internal(
            "RejectingStorage rejects writes".into(),
        ))
    }
}

impl SourceBatchPort for RejectingStorage {}

#[async_trait::async_trait]
impl SourceCursorPort for RejectingStorage {
    async fn load_source_cursor(
        &self,
        _owner: &Owner,
        _source: &str,
    ) -> Result<Option<crate::Cursor>, StorageError> {
        Err(StorageError::Internal(
            "RejectingStorage rejects source cursor reads".into(),
        ))
    }

    async fn store_source_cursor(
        &self,
        _permit: &OwnerWritePermit,
        _source: &str,
        _cursor: &crate::Cursor,
    ) -> Result<(), StorageError> {
        Err(StorageError::Internal(
            "RejectingStorage rejects writes".into(),
        ))
    }

    async fn source_cursor_age(
        &self,
        _owner: &Owner,
        _source: &str,
    ) -> Result<Option<std::time::Duration>, StorageError> {
        Err(StorageError::Internal(
            "RejectingStorage rejects source cursor reads".into(),
        ))
    }
}

#[async_trait::async_trait]
impl OwnerInversePort for RejectingStorage {
    async fn erase_group_owner(
        &self,
        _auth: &crate::owner_inverse::EraseAuthorization,
        _group_id: GroupId,
        _object_purge_planned: bool,
        _tables: &crate::owner_inverse::OwnerSurfaces,
    ) -> Result<crate::owner_inverse::OwnerEraseOutcome, StorageError> {
        Err(StorageError::Internal(
            "RejectingStorage rejects writes".into(),
        ))
    }

    async fn erase_personal_owner(
        &self,
        _auth: &crate::owner_inverse::EraseAuthorization,
        _user_id: UserId,
        _object_purge_planned: bool,
        _tables: &crate::owner_inverse::OwnerSurfaces,
    ) -> Result<crate::owner_inverse::OwnerEraseOutcome, StorageError> {
        Err(StorageError::Internal(
            "RejectingStorage rejects writes".into(),
        ))
    }

    async fn erase_group_source_scope(
        &self,
        _auth: &crate::owner_inverse::EraseAuthorization,
        _group_id: GroupId,
        _source_id: &SourceId,
        _tables: &crate::owner_inverse::OwnerSurfaces,
    ) -> Result<crate::owner_inverse::OwnerEraseOutcome, StorageError> {
        Err(StorageError::Internal(
            "RejectingStorage rejects writes".into(),
        ))
    }

    async fn erase_personal_source_scope(
        &self,
        _auth: &crate::owner_inverse::EraseAuthorization,
        _user_id: UserId,
        _source_id: &SourceId,
        _tables: &crate::owner_inverse::OwnerSurfaces,
    ) -> Result<crate::owner_inverse::OwnerEraseOutcome, StorageError> {
        Err(StorageError::Internal(
            "RejectingStorage rejects writes".into(),
        ))
    }

    async fn export_owner_bundle(
        &self,
        _auth: &crate::owner_inverse::ExportAuthorization,
        _tables: &crate::owner_inverse::OwnerSurfaces,
    ) -> Result<crate::owner_inverse::OwnerExportBundle, StorageError> {
        Err(StorageError::Internal(
            "RejectingStorage rejects reads".into(),
        ))
    }
}

#[async_trait::async_trait]
impl OwnerEraseAuthorityPort for RejectingStorage {
    async fn may_erase_owner(
        &self,
        _authz: &crate::AuthzContext,
        _target: &OwnerEraseTarget,
    ) -> Result<bool, AccessError> {
        Err(AccessError::Resolution(
            "RejectingStorage rejects all auth".into(),
        ))
    }
}

#[async_trait::async_trait]
impl OwnerDropProofPort for RejectingStorage {
    async fn verify_personal_owner_dropped(
        &self,
        _user_id: UserId,
        _drop_event_id: &str,
    ) -> Result<bool, AccessError> {
        Err(AccessError::Resolution(
            "RejectingStorage rejects all auth".into(),
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
    ) -> Result<Vec<FactRow>, StorageError> {
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
}

#[async_trait::async_trait]
impl WriteSessionFactory for RejectingStorage {
    async fn begin(&self) -> Result<Box<dyn WriteSession>, StorageError> {
        Err(StorageError::Internal(
            "RejectingStorage rejects writes".into(),
        ))
    }
}

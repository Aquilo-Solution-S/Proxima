use std::sync::Arc;

use crate::storage_ports::{
    GoalCommandStoragePorts, GoalWritePort, OwnerAccessReadPort, QueryStoragePorts,
};
use crate::{OwnerId, StorageError};

#[derive(Debug)]
struct ReadOnlyFake;

#[async_trait::async_trait]
impl crate::MemoryReadPort for ReadOnlyFake {
    fn store_number(&self) -> u16 {
        0
    }

    async fn load_fact_text(
        &self,
        _scope: Option<&crate::OwnerScope>,
        _owner: &crate::OwnerId,
        _memory_id: crate::MemoryId,
    ) -> Result<Option<String>, StorageError> {
        Ok(None)
    }

    async fn query_memories(
        &self,
        _scope: Option<&crate::OwnerScope>,
        _read_owners: &[OwnerId],
        _req: &crate::verbs::query::QueryRequest,
        _schemas: &[crate::read_models::MemorySchemaSpec],
    ) -> Result<crate::verbs::query::QueryResponse, StorageError> {
        Ok(crate::verbs::query::QueryResponse {
            memories: Vec::new(),
            goals: Vec::new(),
            edges: Vec::new(),
            next_cursor: None,
        })
    }

    async fn count_memories(
        &self,
        _scope: Option<&crate::OwnerScope>,
        _read_owners: &[OwnerId],
    ) -> Result<crate::read_models::MemoryCounts, StorageError> {
        Ok(crate::read_models::MemoryCounts::default())
    }

    async fn search_memories(
        &self,
        _scope: Option<&crate::OwnerScope>,
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
        _scope: Option<&crate::OwnerScope>,
        _read_owners: &[OwnerId],
        _req: &crate::verbs::query::MemoryLineageRequest,
    ) -> Result<crate::verbs::query::MemoryLineageResponse, StorageError> {
        Ok(crate::verbs::query::MemoryLineageResponse {
            nodes: Vec::new(),
            edges: Vec::new(),
            truncated: false,
            next_cursor: None,
            external_origins: Vec::new(),
        })
    }

    async fn load_memory_graph_payloads(
        &self,
        _scope: Option<&crate::OwnerScope>,
        _identities: &[crate::MemoryGraphIdentity],
        _schemas: &[crate::read_models::MemorySchemaSpec],
        _include_body: bool,
    ) -> Result<Vec<crate::MemoryGraphPayloadRow>, StorageError> {
        Ok(Vec::new())
    }

    async fn load_sketches(
        &self,
        _scope: Option<&crate::OwnerScope>,
        _read_owners: &[OwnerId],
        _memory_ids: &[crate::MemoryId],
    ) -> Result<Vec<crate::read_models::MemorySketch>, StorageError> {
        Ok(Vec::new())
    }

    async fn load_pin_nodes(
        &self,
        _scope: Option<&crate::OwnerScope>,
        _read_owners: &[OwnerId],
        _memory_ids: &[crate::MemoryId],
    ) -> Result<Vec<crate::PinNode>, StorageError> {
        Ok(Vec::new())
    }

    async fn load_visible_goal_ids(
        &self,
        _scope: Option<&crate::OwnerScope>,
        _read_owners: &[OwnerId],
        _goal_ids: &[crate::GoalId],
    ) -> Result<Vec<crate::GoalId>, StorageError> {
        Ok(Vec::new())
    }

    async fn load_inbound_pin_nodes(
        &self,
        _scope: Option<&crate::OwnerScope>,
        _read_owners: &[OwnerId],
        _query: crate::InboundPinQuery<'_>,
    ) -> Result<Vec<crate::PinNode>, StorageError> {
        Ok(Vec::new())
    }

    async fn owned_series_handle(
        &self,
        _scope: Option<&crate::OwnerScope>,
        _owner: crate::OwnerId,
        _schema_id: &crate::SchemaId,
        _sidecar_table: &str,
        _columns: &[(&str, crate::verbs::query::SidecarAtom)],
    ) -> Result<Option<uuid::Uuid>, StorageError> {
        Ok(None)
    }
}

#[derive(Debug)]
struct GoalFake;

#[async_trait::async_trait]
impl GoalWritePort for GoalFake {
    async fn resolve_goal_replay(
        &self,
        _req: crate::verbs::goal_write::GoalReplayRequest<'_, '_>,
        _permit: &crate::storage_ports::OwnerWritePermit,
    ) -> Result<Option<crate::verbs::goal_write::GoalReplayOutcome>, StorageError> {
        Err(StorageError::Internal("goal fake rejects writes".into()))
    }

    async fn create_goal_atomic(
        &self,
        _req: &crate::verbs::goal_write::CreateGoalAtomicRequest<'_>,
        _permit: &crate::storage_ports::OwnerWritePermit,
    ) -> Result<crate::verbs::goal_write::GoalWriteOutcome, StorageError> {
        Err(StorageError::Internal("goal fake rejects writes".into()))
    }

    async fn transition_goal_atomic(
        &self,
        _req: &crate::verbs::goal_write::TransitionGoalAtomicRequest<'_>,
        _permit: &crate::storage_ports::OwnerWritePermit,
    ) -> Result<crate::verbs::goal_write::GoalWriteOutcome, StorageError> {
        Err(StorageError::Internal("goal fake rejects writes".into()))
    }

    async fn achieve_goal_atomic(
        &self,
        _req: &crate::verbs::goal_write::AchieveGoalAtomicRequest<'_>,
        _permit: &crate::storage_ports::OwnerWritePermit,
    ) -> Result<crate::verbs::goal_write::GoalWriteOutcome, StorageError> {
        Err(StorageError::Internal("goal fake rejects writes".into()))
    }

    async fn modify_goal_atomic(
        &self,
        _req: &crate::verbs::goal_write::ModifyGoalAtomicRequest<'_>,
        _permit: &crate::storage_ports::OwnerWritePermit,
    ) -> Result<crate::verbs::goal_write::GoalWriteOutcome, StorageError> {
        Err(StorageError::Internal("goal fake rejects writes".into()))
    }

    async fn decompose_goal_atomic(
        &self,
        _req: &crate::verbs::goal_write::DecomposeGoalAtomicRequest<'_>,
        _permit: &crate::storage_ports::OwnerWritePermit,
    ) -> Result<crate::verbs::goal_write::DecomposeGoalOutcome, StorageError> {
        Err(StorageError::Internal("goal fake rejects writes".into()))
    }
}

#[async_trait::async_trait]
impl OwnerAccessReadPort for GoalFake {
    async fn resolve_membership(
        &self,
        _owner_scope: Option<&crate::OwnerScope>,
        _member: &OwnerId,
    ) -> Result<Vec<crate::MembershipRow>, StorageError> {
        Ok(Vec::new())
    }

    async fn visible_home_owner(
        &self,
        _owner_scope: Option<&crate::OwnerScope>,
        _entity: crate::EntityId,
        _read_owners: &[OwnerId],
    ) -> Result<Option<(OwnerId, crate::AccessKind)>, StorageError> {
        Ok(None)
    }

    async fn home_owner(
        &self,
        _owner_scope: Option<&crate::OwnerScope>,
        _entity: crate::EntityId,
    ) -> Result<Option<OwnerId>, StorageError> {
        Ok(None)
    }
}

#[tokio::test]
async fn query_helper_accepts_only_query_read_handles() {
    let read = Arc::new(ReadOnlyFake);
    let ports = QueryStoragePorts { memory_read: read };
    let owner = OwnerId::from(crate::UserId::new(uuid::Uuid::now_v7()));
    let req = crate::verbs::query::QueryRequest::readable();

    let response = super::query::query_authorized(&ports, None, &[], &[owner], &req)
        .await
        .expect("query helper should compile against query ports only");

    assert!(response.memories.is_empty());
}

#[tokio::test]
async fn goal_helper_accepts_only_goal_command_handles() {
    let goal = Arc::new(GoalFake);
    let ports = GoalCommandStoragePorts {
        goal_write: goal.clone(),
        owner_access_read: goal,
    };
    let registry = crate::FlavorRegistry::new().freeze_or_panic_for_tests();
    let req = crate::verbs::goal_write::TransitionGoalAtomicRequest {
        owner: OwnerId::from(crate::UserId::new(uuid::Uuid::now_v7())),
        prior_goal_id: crate::GoalId::new(uuid::Uuid::now_v7()),
        transition: crate::verbs::goal_write::GoalTransitionKind::Pause {
            pending: crate::PendingWork::Keep,
        },
        authorship: crate::verbs::goal_write::GoalAuthorship::User,
        request_id: crate::verbs::goal_write::IdempotencyKey::generated("test"),
        context: crate::verbs::goal_write::GoalAtomicContext {
            registry: &registry,
            author_self_perspective_id: None,
            publication: None,
        },
    };
    let permit = crate::storage_ports::OwnerWritePermit::new(
        req.owner,
        crate::access::AccessKind::Goal,
        None,
    );

    let err = super::goal_write::transition_goal_authorized(&ports, &req, &permit)
        .await
        .expect_err("goal fake should reject writes");

    assert_eq!(err.code, crate::error::ErrorCode::Internal);
}

#[allow(dead_code)]
mod storage_port_tests_support {
    use super::*;

    #[derive(Debug)]
    pub struct GoalReadFake;

    #[async_trait::async_trait]
    impl crate::storage_ports::GoalReadPort for GoalReadFake {
        async fn list_active_goals(
            &self,
            _scope: Option<&crate::OwnerScope>,
            _read_owners: &[crate::OwnerId],
            _self_perspective_memory_id: crate::MemoryId,
            _limit: usize,
        ) -> Result<Vec<crate::read_models::ActiveGoalSummary>, StorageError> {
            Ok(Vec::new())
        }

        async fn load_goal_evidence(
            &self,
            _scope: Option<&crate::OwnerScope>,
            _owner: &crate::OwnerId,
            _goal_id: crate::GoalId,
        ) -> Result<Option<Vec<crate::MemoryId>>, StorageError> {
            Ok(None)
        }
    }

    #[derive(Debug)]
    pub struct MemoryInspectFake;

    #[async_trait::async_trait]
    impl crate::MemoryInspectPort for MemoryInspectFake {
        async fn load_memory_by_id(
            &self,
            _scope: Option<&crate::OwnerScope>,
            _memory_id: crate::MemoryId,
            _schemas: &[crate::read_models::MemorySchemaSpec],
        ) -> Result<Option<crate::read_models::MemorySnapshot>, StorageError> {
            Ok(None)
        }

        async fn load_memories_by_ids(
            &self,
            _scope: Option<&crate::OwnerScope>,
            _read_owners: &[crate::OwnerId],
            _memory_ids: &[crate::MemoryId],
            _schemas: &[crate::read_models::MemorySchemaSpec],
        ) -> Result<Vec<crate::read_models::MemorySnapshot>, StorageError> {
            Ok(Vec::new())
        }
    }

    #[derive(Debug)]
    pub struct EmbeddingJobFake;

    #[async_trait::async_trait]
    impl crate::EmbeddingJobPort for EmbeddingJobFake {
        async fn claim_pending_embedding_jobs(
            &self,
            _limit: i64,
            _skip_owners: &[crate::OwnerId],
            _skip_owner_spaces: &[(crate::OwnerId, crate::EmbeddingSpace)],
        ) -> Result<Vec<crate::storage::EmbeddingJobClaim>, StorageError> {
            Ok(Vec::new())
        }

        async fn complete_embedding_job(
            &self,
            _claim: &crate::storage::EmbeddingJobClaim,
        ) -> Result<(), StorageError> {
            Ok(())
        }

        async fn renew_embedding_jobs(
            &self,
            claims: &[crate::storage::EmbeddingJobClaim],
        ) -> Result<u64, StorageError> {
            Ok(u64::try_from(claims.len()).expect("test claim count fits u64"))
        }

        async fn reclaim_stale_embedding_jobs(
            &self,
            _older_than_seconds: i64,
        ) -> Result<u64, StorageError> {
            Ok(0)
        }

        async fn fail_embedding_job(
            &self,
            _claim: &crate::storage::EmbeddingJobClaim,
            _error: &str,
        ) -> Result<(), StorageError> {
            Ok(())
        }

        async fn fail_embedding_job_permanently(
            &self,
            _claim: &crate::storage::EmbeddingJobClaim,
            _error: &str,
        ) -> Result<(), StorageError> {
            Ok(())
        }

        async fn release_embedding_jobs(
            &self,
            _claims: &[crate::storage::EmbeddingJobClaim],
            _error: &str,
        ) -> Result<(), StorageError> {
            Ok(())
        }

        async fn enqueue_missing_embedding_jobs(
            &self,
            _permit: &crate::storage_ports::OwnerWritePermit,
            _space: &crate::EmbeddingSpace,
            _limit: i64,
            _non_embeddable_schemas: &[String],
        ) -> Result<u64, StorageError> {
            Ok(0)
        }

        async fn count_embedding_job_status(
            &self,
            _scope: Option<&crate::OwnerScope>,
            _owner: &crate::OwnerId,
        ) -> Result<crate::storage_ports::EmbeddingJobStatusCounts, StorageError> {
            Ok(crate::storage_ports::EmbeddingJobStatusCounts::default())
        }
    }

    #[async_trait::async_trait]
    impl crate::EmbeddingMaintenancePort for EmbeddingJobFake {
        async fn embedding_ann_observability(
            &self,
            _policy: crate::EmbeddingRuntimePolicy,
            _proof: crate::OperatorMaintenanceProof,
        ) -> Result<crate::EmbeddingAnnObservability, StorageError> {
            Ok(crate::EmbeddingAnnObservability {
                embedding_rows: 0,
                embedding_head_rows: 0,
                embedding_job_rows: 0,
                embedding_table_bytes: 0,
                embedding_total_relation_bytes: 0,
                hnsw_index_bytes: 0,
                backlog: crate::EmbeddingJobBacklog::default(),
                stale_processing_jobs: 0,
                orphan_rows: crate::EmbeddingOrphanCounts::default(),
                recall_canary: None,
            })
        }

        async fn sweep_orphan_embedding_rows(
            &self,
            _proof: crate::OperatorMaintenanceProof,
        ) -> Result<crate::EmbeddingOrphanSweepOutcome, StorageError> {
            Ok(crate::EmbeddingOrphanSweepOutcome::default())
        }

        async fn reconcile_embeddings(
            &self,
            _options: crate::EmbeddingReconcileOptions<'_>,
            _policy: crate::EmbeddingRuntimePolicy,
            _proof: crate::OperatorMaintenanceProof,
        ) -> Result<crate::EmbeddingReconcileOutcome, StorageError> {
            Ok(crate::EmbeddingReconcileOutcome {
                scanned: 3,
                enqueued: 2,
                skipped: 1,
            })
        }

        async fn embedding_owner_page(
            &self,
            _after: Option<crate::OwnerId>,
            _limit: i64,
            _proof: crate::OperatorMaintenanceProof,
        ) -> Result<Vec<crate::OwnerId>, StorageError> {
            Ok(Vec::new())
        }

        async fn embedding_coverage(
            &self,
            _owner: &crate::OwnerId,
            _spaces: &[crate::EmbeddingSpace],
            _non_embeddable_schemas: &[String],
            _proof: crate::OperatorMaintenanceProof,
        ) -> Result<Vec<(crate::EmbeddingSpace, crate::EmbeddingSpaceCounts)>, StorageError>
        {
            Ok(Vec::new())
        }

        async fn purge_embedding_spaces(
            &self,
            _owner: &crate::OwnerId,
            _keep: &[crate::EmbeddingSpace],
            _limit: i64,
            _proof: crate::OperatorMaintenanceProof,
        ) -> Result<crate::EmbeddingPurgeOutcome, StorageError> {
            Ok(crate::EmbeddingPurgeOutcome::default())
        }
    }

    #[derive(Debug)]
    pub struct CitationFake;

    #[async_trait::async_trait]
    impl crate::CitationPort for CitationFake {
        async fn facts_citing_object(
            &self,
            _scope: Option<&crate::OwnerScope>,
            _read_owners: &[OwnerId],
            _cited_object_id: uuid::Uuid,
            _schemas: &[crate::read_models::MemorySchemaSpec],
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
            _scope: Option<&crate::OwnerScope>,
            _read_owners: &[OwnerId],
            _fact_memory_id: crate::MemoryId,
        ) -> Result<Option<crate::verbs::query::FactCitationReadback>, StorageError> {
            Ok(None)
        }
    }
}

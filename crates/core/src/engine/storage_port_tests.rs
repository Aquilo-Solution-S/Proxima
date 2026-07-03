use std::sync::Arc;

use crate::storage_ports::{
    GoalCommandStoragePorts, GoalWritePort, OwnerAccessReadPort, QueryStoragePorts,
    ReadVerbStoragePorts,
};
use crate::{OwnerRef, StorageError};

#[derive(Debug)]
struct ReadOnlyFake;

#[async_trait::async_trait]
impl crate::MemoryReadPort for ReadOnlyFake {
    async fn load_fact_text(
        &self,
        _owner: &crate::Owner,
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

#[derive(Debug)]
struct GoalFake;

#[async_trait::async_trait]
impl GoalWritePort for GoalFake {
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
        _member: &OwnerRef,
    ) -> Result<Vec<crate::MembershipRow>, StorageError> {
        Ok(Vec::new())
    }

    async fn visible_to_any(
        &self,
        _entity: crate::EntityId,
        _read_owners: &[OwnerRef],
    ) -> Result<bool, StorageError> {
        Ok(false)
    }

    async fn home_owner(&self, _entity: crate::EntityId) -> Result<Option<OwnerRef>, StorageError> {
        Ok(None)
    }
}

#[tokio::test]
async fn query_helper_accepts_only_query_read_handles() {
    let read = Arc::new(ReadOnlyFake);
    let ports = QueryStoragePorts {
        change_event: Arc::new(storage_port_tests_support::ChangeEventFake),
        mcp_call_read: Arc::new(storage_port_tests_support::McpCallReadFake),
        memory_read: read,
        edge_read: Arc::new(storage_port_tests_support::EdgeReadFake),
    };
    let owner = OwnerRef::Personal(crate::UserId::new(uuid::Uuid::now_v7()));
    let registry = crate::FlavorRegistry::new().freeze_or_panic_for_tests();
    let req = crate::verbs::query::QueryRequest::for_owner(owner);

    let response = super::query::query_authorized(&ports, &registry, &[owner], &req)
        .await
        .expect("query helper should compile against query ports only");

    assert!(response.memories.is_empty());
}

#[tokio::test]
async fn read_verb_helper_accepts_only_read_verb_handles() {
    let ports = ReadVerbStoragePorts {
        embedding_job: Arc::new(storage_port_tests_support::EmbeddingJobFake),
        memory_read: Arc::new(ReadOnlyFake),
        memory_inspect: Arc::new(storage_port_tests_support::MemoryInspectFake),
        change_event: Arc::new(storage_port_tests_support::ChangeEventFake),
        citation: Arc::new(storage_port_tests_support::CitationFake),
        fact_retention: Arc::new(storage_port_tests_support::FactRetentionFake),
    };
    let owner = OwnerRef::Personal(crate::UserId::new(uuid::Uuid::now_v7()));
    let req = super::read_verbs::ListChangeEventsReadRequest {
        owner,
        after: uuid::Uuid::nil(),
        limit: 1,
    };

    let response = super::read_verbs::list_change_events_authorized(&ports, &[owner], &req)
        .await
        .expect("read helper should compile against read-verb ports only");

    assert!(response.events.is_empty());
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
        owner: OwnerRef::Personal(crate::UserId::new(uuid::Uuid::now_v7())),
        prior_goal_id: crate::GoalId::new(uuid::Uuid::now_v7()),
        next_state: crate::verbs::goal_write::GoalState::Paused,
        authorship: crate::verbs::goal_write::GoalAuthorship::User,
        request_id: crate::verbs::goal_write::IdempotencyKey::generated("test"),
        context: crate::verbs::goal_write::GoalAtomicContext {
            registry: &registry,
            embedding_model_id: None,
            author_self_perspective_id: None,
        },
    };
    let permit =
        crate::storage_ports::OwnerWritePermit::new(req.owner, crate::access::AccessKind::Goal);

    let err = super::goal_write::transition_goal_authorized(&ports, &req, &permit)
        .await
        .expect_err("goal fake should reject writes");

    assert_eq!(err.code, crate::error::ErrorCode::Internal);
}

mod storage_port_tests_support {
    use super::*;

    #[derive(Debug)]
    pub struct ChangeEventFake;

    #[async_trait::async_trait]
    impl crate::ChangeEventPort for ChangeEventFake {
        async fn change_history(
            &self,
            _read_owners: &[OwnerRef],
            _req: &crate::verbs::change_history::ChangeHistoryRequest,
        ) -> Result<crate::verbs::change_history::ChangeHistoryResponse, StorageError> {
            Ok(crate::verbs::change_history::ChangeHistoryResponse {
                events: Vec::new(),
                seq_high_water: None,
            })
        }

        async fn list_change_events_after(
            &self,
            _read_owners: &[OwnerRef],
            _after: uuid::Uuid,
            _limit: usize,
        ) -> Result<Vec<crate::read_models::ChangeEventForWake>, StorageError> {
            Ok(Vec::new())
        }
    }

    #[derive(Debug)]
    pub struct McpCallReadFake;

    #[async_trait::async_trait]
    impl crate::McpCallReadPort for McpCallReadFake {
        async fn read_mcp_call_history(
            &self,
            _req: &crate::verbs::mcp_call_history::McpCallHistoryRequest,
        ) -> Result<crate::verbs::mcp_call_history::McpCallHistoryResponse, StorageError> {
            Ok(crate::verbs::mcp_call_history::McpCallHistoryResponse { calls: Vec::new() })
        }
    }

    #[derive(Debug)]
    pub struct MemoryInspectFake;

    #[async_trait::async_trait]
    impl crate::MemoryInspectPort for MemoryInspectFake {
        async fn load_memory_by_id(
            &self,
            _memory_id: crate::MemoryId,
            _sidecars: &[crate::read_models::SidecarSpec],
        ) -> Result<Option<crate::read_models::MemorySnapshot>, StorageError> {
            Ok(None)
        }
    }

    #[derive(Debug)]
    pub struct EmbeddingJobFake;

    #[async_trait::async_trait]
    impl crate::EmbeddingJobPort for EmbeddingJobFake {
        async fn claim_pending_embedding_jobs(
            &self,
            _model_id: &str,
            _limit: i64,
        ) -> Result<Vec<crate::storage::EmbeddingJobClaim>, StorageError> {
            Ok(Vec::new())
        }

        async fn complete_embedding_job(
            &self,
            _claim: &crate::storage::EmbeddingJobClaim,
        ) -> Result<(), StorageError> {
            Ok(())
        }

        async fn fail_embedding_job(
            &self,
            _claim: &crate::storage::EmbeddingJobClaim,
            _error: &str,
        ) -> Result<(), StorageError> {
            Ok(())
        }

        async fn enqueue_missing_embedding_jobs(
            &self,
            _permit: &crate::storage_ports::OwnerWritePermit,
            _model_id: &str,
            _limit: i64,
        ) -> Result<u64, StorageError> {
            Ok(0)
        }

        async fn count_pending_embedding_jobs(
            &self,
            _owner: &crate::Owner,
        ) -> Result<u64, StorageError> {
            Ok(0)
        }
    }

    #[async_trait::async_trait]
    impl crate::EmbeddingMaintenancePort for EmbeddingJobFake {
        async fn embedding_ann_observability(
            &self,
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
    }

    #[derive(Debug)]
    pub struct CitationFake;

    #[async_trait::async_trait]
    impl crate::CitationPort for CitationFake {
        async fn fact_entity_id_for(
            &self,
            _owner: &crate::Owner,
            _schema_id: &crate::SchemaId,
            _schema_version: crate::SchemaVersion,
            _natural_key: &[String],
        ) -> Result<Option<crate::FactEntityId>, StorageError> {
            Ok(None)
        }

        async fn facts_citing_object(
            &self,
            _read_owners: &[OwnerRef],
            _cited_object_id: uuid::Uuid,
            _sidecars: &[crate::read_models::SidecarSpec],
        ) -> Result<Vec<crate::read_models::MemorySnapshot>, StorageError> {
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
            _fact_entity_id: crate::FactEntityId,
        ) -> Result<Option<crate::verbs::query::FactCitationReadback>, StorageError> {
            Ok(None)
        }
    }

    #[derive(Debug)]
    pub struct FactRetentionFake;

    #[async_trait::async_trait]
    impl crate::FactRetentionPort for FactRetentionFake {
        async fn upsert_fact_retention(
            &self,
            _permit: &crate::storage_ports::OwnerWritePermit,
            _seconds: i64,
        ) -> Result<(), StorageError> {
            Ok(())
        }

        async fn get_fact_retention(
            &self,
            _owner: &crate::Owner,
        ) -> Result<Option<i64>, StorageError> {
            Ok(None)
        }

        async fn clear_fact_retention(
            &self,
            _permit: &crate::storage_ports::OwnerWritePermit,
        ) -> Result<bool, StorageError> {
            Ok(false)
        }

        async fn set_legal_hold(
            &self,
            _permit: &crate::storage_ports::OwnerWritePermit,
        ) -> Result<(), StorageError> {
            Ok(())
        }

        async fn get_legal_hold(&self, _owner: &crate::Owner) -> Result<bool, StorageError> {
            Ok(false)
        }

        async fn clear_legal_hold(
            &self,
            _permit: &crate::storage_ports::OwnerWritePermit,
        ) -> Result<bool, StorageError> {
            Ok(false)
        }
    }

    #[derive(Debug)]
    pub struct EdgeReadFake;

    #[async_trait::async_trait]
    impl crate::EdgeReadPort for EdgeReadFake {
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
}

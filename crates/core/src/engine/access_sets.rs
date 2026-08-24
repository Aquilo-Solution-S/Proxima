use crate::OwnerRef;
use crate::access::{AccessKind, Relation};
use crate::authz::{AuthPath, AuthzContext};
use crate::error::ProtocolError;

use super::Engine;

pub struct AccessSets {
    read: Vec<OwnerRef>,
    write: Vec<(OwnerRef, Relation)>,
}

impl AccessSets {
    #[must_use]
    pub fn read_owners(&self) -> &[OwnerRef] {
        &self.read
    }

    #[must_use]
    pub fn can_read(&self, owner: &OwnerRef) -> bool {
        self.read.iter().any(|candidate| candidate == owner)
    }

    #[must_use]
    pub fn can_write(&self, owner: &OwnerRef, required: Relation) -> bool {
        self.write
            .iter()
            .any(|(candidate, relation)| candidate == owner && relation.dominates(required))
    }

    #[must_use]
    pub fn write_owners_for(&self, required: Relation) -> Vec<OwnerRef> {
        let mut owners = Vec::new();
        for (owner, relation) in &self.write {
            if relation.dominates(required) && !owners.iter().any(|candidate| candidate == owner) {
                owners.push(*owner);
            }
        }
        owners
    }
}

impl Engine {
    /// Resolve the per-request read-owner set and write-authority set.
    ///
    /// # Errors
    ///
    /// Returns `Internal` when storage-backed membership expansion fails.
    // Resolution is a pure function of the (server-resolved) authorization
    // context: it stays an Engine method (`&self`) for call-site stability and
    // because access resolution may consult storage again in future, but the
    // current body needs neither `self` nor `.await`.
    #[allow(clippy::unused_self, clippy::unused_async)]
    pub(in crate::engine) async fn resolve_access(
        &self,
        authz: &AuthzContext,
    ) -> Result<AccessSets, ProtocolError> {
        self.resolve_access_inner(authz, false)
    }

    #[allow(clippy::unused_self)]
    pub(in crate::engine) fn resolve_access_inner(
        &self,
        authz: &AuthzContext,
        redeemed_phase: bool,
    ) -> Result<AccessSets, ProtocolError> {
        let mut access = AccessSets {
            read: Vec::new(),
            write: Vec::new(),
        };
        if authz.auth_path() == AuthPath::Delegated && !redeemed_phase {
            return Err(ProtocolError::forbidden(
                "raw delegated authorization contexts are not Engine authority",
            ));
        }
        // Production authorization is uniformly server-resolved: `OwnerRoles`
        // carry per-owner role ceilings already resolved by the host/OIDC layer
        // (which itself expands group membership at auth time). A Denied context
        // — or any context without resolved roles — gets no access. Fail closed.
        if authz.auth_path() == AuthPath::Denied || !authz.is_server_resolved() {
            return Ok(access);
        }
        push_role_access(&mut access, authz);
        Ok(access)
    }
}

fn push_role_access(access: &mut AccessSets, authz: &AuthzContext) {
    for owner in authz.readable_owners(AccessKind::Goal) {
        push_read_owner(&mut access.read, owner);
    }
    for owner in authz.writable_owners(AccessKind::Fact) {
        push_write_owner(&mut access.write, owner, Relation::Ingest);
    }
    for owner in authz.writable_owners(AccessKind::Perspective) {
        push_write_owner(&mut access.write, owner, Relation::Editor);
    }
    for owner in authz.writable_owners(AccessKind::Goal) {
        push_write_owner(&mut access.write, owner, Relation::Admin);
    }
}

fn push_read_owner(read: &mut Vec<OwnerRef>, owner: OwnerRef) {
    if !read.contains(&owner) {
        read.push(owner);
    }
}

fn push_write_owner(write: &mut Vec<(OwnerRef, Relation)>, owner: OwnerRef, relation: Relation) {
    if !write
        .iter()
        .any(|(candidate, existing)| candidate == &owner && existing == &relation)
    {
        write.push((owner, relation));
    }
}

#[cfg(test)]
#[allow(clippy::too_many_lines, clippy::wildcard_imports)]
pub(in crate::engine) mod tests {
    use std::sync::Arc;

    use crate::change_history::{ChangeHistoryRequest, ChangeHistoryResponse};

    use crate::goal_write::{
        AchieveGoalAtomicRequest, CreateGoalAtomicRequest, DecomposeGoalAtomicRequest,
        DecomposeGoalOutcome, GoalWriteOutcome, ModifyGoalAtomicRequest,
        TransitionGoalAtomicRequest,
    };
    use crate::mcp_call_history::{McpCallHistoryRequest, McpCallHistoryResponse};
    use crate::storage_ports::{StoragePorts, WriteSession, WriteSessionFactory};
    use crate::*;

    #[derive(Debug)]
    pub(in crate::engine) struct MembershipStorage {
        pub(in crate::engine) member: OwnerRef,
        pub(in crate::engine) group: GroupId,
        pub(in crate::engine) membership_relation: Relation,
        pub(in crate::engine) home_owner: Option<OwnerRef>,
        pub(in crate::engine) entity_readable: bool,
        pub(in crate::engine) memory_kind: Option<EntityKind>,
    }

    #[async_trait::async_trait]
    impl FactIngestPort for MembershipStorage {
        async fn ingest_fact_atomic(
            &self,
            _permit: &crate::storage_ports::OwnerWritePermit,
            _draft: &FactWriteCommand,
            _embedding_model_id: Option<&str>,
        ) -> Result<FactIngestOutcome, StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }

        async fn ingest_fact_with_typed_sidecar(
            &self,
            _authorized: &AuthorizedFactWrite,
            _sidecar_payloads: &[SidecarPayload],
            _embedding_model_id: Option<&str>,
        ) -> Result<FactIngestOutcome, StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }

        async fn ingest_fact_with_citation_and_typed_sidecar(
            &self,
            _authorized: &AuthorizedFactWithCitation,
            _sidecar_payloads: &[SidecarPayload],
            _embedding_model_id: Option<&str>,
        ) -> Result<FactIngestOutcome, StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }

        async fn ingest_fact_with_citation_ref_and_typed_sidecar(
            &self,
            _authorized: &crate::verbs::fact_ingest::AuthorizedFactWithCitationRef,
            _sidecar_payloads: &[SidecarPayload],
            _embedding_model_id: Option<&str>,
        ) -> Result<FactIngestOutcome, StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }
    }

    #[async_trait::async_trait]
    impl McpCallReadPort for MembershipStorage {
        async fn read_mcp_call_history(
            &self,
            _req: &McpCallHistoryRequest,
        ) -> Result<McpCallHistoryResponse, StorageError> {
            Ok(McpCallHistoryResponse { calls: Vec::new() })
        }
    }

    #[async_trait::async_trait]
    impl MemoryAuthoringPort for MembershipStorage {
        async fn author_derived(
            &self,
            _req: &AuthorDerivedRequest<'_>,
            _permit: &crate::storage_ports::OwnerWritePermit,
            _proof: crate::storage_ports::OperatorWriteProof,
        ) -> Result<AuthorDerivedOutcome, StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }

        async fn load_memory_kinds(
            &self,
            _owner: &Owner,
            memory_ids: &[MemoryId],
        ) -> Result<Vec<MemoryKindRow>, StorageError> {
            Ok(self
                .memory_kind
                .map(|kind| {
                    memory_ids
                        .iter()
                        .map(|memory_id| MemoryKindRow {
                            memory_id: *memory_id,
                            kind,
                        })
                        .collect()
                })
                .unwrap_or_default())
        }

        async fn load_fact_source_batches(
            &self,
            _owner: &Owner,
            _memory_ids: &[MemoryId],
        ) -> Result<Vec<FactSourceBatchRow>, StorageError> {
            Ok(Vec::new())
        }

        async fn forget_memory(
            &self,
            _permit: &crate::storage_ports::OwnerWritePermit,
            _memory_id: MemoryId,
        ) -> Result<(), StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }
    }

    #[async_trait::async_trait]
    impl MemoryReadPort for MembershipStorage {
        async fn load_fact_text(
            &self,
            _owner: &Owner,
            _memory_id: MemoryId,
        ) -> Result<Option<String>, StorageError> {
            Ok(None)
        }

        async fn query_memories(
            &self,
            _req: &verbs::query::QueryRequest,
            _schemas: &[verbs::schema::SchemaInfo],
        ) -> Result<verbs::query::QueryResponse, StorageError> {
            Ok(verbs::query::QueryResponse {
                memories: Vec::new(),
                goals: Vec::new(),
                edges: Vec::new(),
                next_cursor: None,
                seq_high_water: None,
            })
        }

        async fn search_memories(
            &self,
            _req: &verbs::query::MemorySearchRequest,
            _projections: &[verbs::schema::MemorySearchProjection],
        ) -> Result<verbs::query::MemorySearchPage, StorageError> {
            Ok(verbs::query::MemorySearchPage {
                results: Vec::new(),
                has_more: false,
            })
        }

        async fn walk_memory_lineage(
            &self,
            _read_owners: &[OwnerRef],
            _req: &verbs::query::MemoryLineageRequest,
        ) -> Result<verbs::query::MemoryLineageResponse, StorageError> {
            Ok(verbs::query::MemoryLineageResponse {
                nodes: Vec::new(),
                edges: Vec::new(),
                truncated: false,
                next_cursor: None,
            })
        }

        async fn load_memory_graph_payloads(
            &self,
            _identities: &[MemoryGraphIdentity],
            _include_body: bool,
        ) -> Result<Vec<MemoryGraphPayloadRow>, StorageError> {
            Ok(Vec::new())
        }

        async fn load_sketches(
            &self,
            _read_owners: &[OwnerRef],
            _memory_ids: &[MemoryId],
        ) -> Result<Vec<crate::read_models::MemorySketch>, StorageError> {
            Ok(Vec::new())
        }

        async fn load_pin_nodes(
            &self,
            _read_owners: &[OwnerRef],
            _memory_ids: &[MemoryId],
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
            _owner: Owner,
            _schema_id: &crate::SchemaId,
            _sidecar_table: &str,
            _columns: &[(&str, crate::verbs::query::SidecarAtom)],
        ) -> Result<Option<uuid::Uuid>, StorageError> {
            Ok(None)
        }
    }

    #[async_trait::async_trait]
    impl MemoryInspectPort for MembershipStorage {
        async fn load_memory_by_id(
            &self,
            _memory_id: MemoryId,
            _sidecars: &[SidecarSpec],
        ) -> Result<Option<MemorySnapshot>, StorageError> {
            Ok(None)
        }

        async fn load_memories_by_ids(
            &self,
            _read_owners: &[OwnerRef],
            _memory_ids: &[MemoryId],
            _sidecars: &[SidecarSpec],
        ) -> Result<Vec<MemorySnapshot>, StorageError> {
            Ok(Vec::new())
        }
    }

    #[async_trait::async_trait]
    impl EmbeddingTextPort for MembershipStorage {
        async fn load_embedding_text(
            &self,
            _owner: &Owner,
            _entity_kind: EntityKind,
            _memory_id: MemoryId,
            _non_embeddable_schemas: &[String],
        ) -> Result<Option<String>, StorageError> {
            Ok(None)
        }

        async fn load_embedding_texts(
            &self,
            items: &[(Owner, EntityKind, MemoryId)],
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
        ) -> Result<Vec<MemoryId>, StorageError> {
            Ok(Vec::new())
        }
    }

    #[async_trait::async_trait]
    impl EmbeddingWritePort for MembershipStorage {
        async fn insert_embedding(
            &self,
            _owner: &Owner,
            _entity: crate::EmbeddableEntityRef,
            _model_id: &str,
            _dim: usize,
            _vec: &[f32],
            _proof: crate::storage_ports::EmbeddingWriteProof,
        ) -> Result<crate::EmbeddingWriteOutcome, StorageError> {
            Ok(crate::EmbeddingWriteOutcome {
                embedding_version: 0,
            })
        }

        async fn insert_embedding_chunks(
            &self,
            _owner: &Owner,
            _entity: crate::EmbeddableEntityRef,
            _model_id: &str,
            _dim: usize,
            _chunks: &[&[f32]],
            _proof: crate::storage_ports::EmbeddingWriteProof,
        ) -> Result<crate::EmbeddingWriteOutcome, StorageError> {
            Ok(crate::EmbeddingWriteOutcome {
                embedding_version: 0,
            })
        }
    }

    #[async_trait::async_trait]
    impl EmbeddingJobPort for MembershipStorage {
        async fn claim_pending_embedding_jobs(
            &self,
            _model_id: &str,
            _limit: i64,
        ) -> Result<Vec<EmbeddingJobClaim>, StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }

        async fn complete_embedding_job(
            &self,
            _claim: &EmbeddingJobClaim,
        ) -> Result<(), StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }

        async fn renew_embedding_jobs(
            &self,
            _claims: &[EmbeddingJobClaim],
        ) -> Result<u64, StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }

        async fn fail_embedding_job(
            &self,
            _claim: &EmbeddingJobClaim,
            _error: &str,
        ) -> Result<(), StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }

        async fn fail_embedding_job_permanently(
            &self,
            _claim: &EmbeddingJobClaim,
            _error: &str,
        ) -> Result<(), StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }

        async fn release_embedding_jobs(
            &self,
            _claims: &[EmbeddingJobClaim],
            _error: &str,
        ) -> Result<(), StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }

        async fn enqueue_missing_embedding_jobs(
            &self,
            _permit: &crate::storage_ports::OwnerWritePermit,
            _model_id: &str,
            _limit: i64,
            _non_embeddable_schemas: &[String],
        ) -> Result<u64, StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
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
    impl crate::EmbeddingMaintenancePort for MembershipStorage {
        async fn embedding_ann_observability(
            &self,
            _policy: crate::EmbeddingRuntimePolicy,
            _proof: crate::OperatorMaintenanceProof,
        ) -> Result<crate::EmbeddingAnnObservability, StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects operational embedding reads".into(),
            ))
        }

        async fn sweep_orphan_embedding_rows(
            &self,
            _proof: crate::OperatorMaintenanceProof,
        ) -> Result<crate::EmbeddingOrphanSweepOutcome, StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects embedding maintenance".into(),
            ))
        }

        async fn reconcile_embeddings(
            &self,
            _options: crate::EmbeddingReconcileOptions<'_>,
            _policy: crate::EmbeddingRuntimePolicy,
            _proof: crate::OperatorMaintenanceProof,
        ) -> Result<crate::EmbeddingReconcileOutcome, StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects embedding maintenance".into(),
            ))
        }
    }

    #[async_trait::async_trait]
    impl GoalWritePort for MembershipStorage {
        async fn create_goal_atomic(
            &self,
            _req: &CreateGoalAtomicRequest<'_>,
            _permit: &crate::storage_ports::OwnerWritePermit,
        ) -> Result<GoalWriteOutcome, StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }

        async fn transition_goal_atomic(
            &self,
            _req: &TransitionGoalAtomicRequest<'_>,
            _permit: &crate::storage_ports::OwnerWritePermit,
        ) -> Result<GoalWriteOutcome, StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }

        async fn achieve_goal_atomic(
            &self,
            _req: &AchieveGoalAtomicRequest<'_>,
            _permit: &crate::storage_ports::OwnerWritePermit,
        ) -> Result<GoalWriteOutcome, StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }

        async fn modify_goal_atomic(
            &self,
            _req: &ModifyGoalAtomicRequest<'_>,
            _permit: &crate::storage_ports::OwnerWritePermit,
        ) -> Result<GoalWriteOutcome, StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }

        async fn decompose_goal_atomic(
            &self,
            _req: &DecomposeGoalAtomicRequest<'_>,
            _permit: &crate::storage_ports::OwnerWritePermit,
        ) -> Result<DecomposeGoalOutcome, StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }
    }

    #[async_trait::async_trait]
    impl GoalReadPort for MembershipStorage {
        async fn list_active_goals(
            &self,
            _read_owners: &[OwnerRef],
            _self_perspective_memory_id: MemoryId,
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
    impl crate::storage_ports::GoalWakeCandidatePort for MembershipStorage {
        async fn list_goal_wake_candidates(
            &self,
            _req: &GoalWakeCandidateRequest<'_>,
        ) -> Result<Vec<GoalWakeCandidate>, StorageError> {
            Ok(Vec::new())
        }
    }

    #[async_trait::async_trait]
    impl ChangeEventPort for MembershipStorage {
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
    impl CitationPort for MembershipStorage {
        async fn facts_citing_object(
            &self,
            _read_owners: &[OwnerRef],
            _cited_object_id: uuid::Uuid,
            _sidecars: &[SidecarSpec],
            _after: Option<verbs::query::FactCitationCursor>,
            _limit: u32,
        ) -> Result<verbs::query::FactCitationPage, StorageError> {
            Ok(verbs::query::FactCitationPage {
                facts: Vec::new(),
                next_cursor: None,
                has_more: false,
            })
        }

        async fn citation_of_fact(
            &self,
            _read_owners: &[OwnerRef],
            _fact_memory_id: MemoryId,
        ) -> Result<Option<verbs::query::FactCitationReadback>, StorageError> {
            Ok(None)
        }
    }

    #[async_trait::async_trait]
    impl OwnerAccessReadPort for MembershipStorage {
        async fn resolve_membership(
            &self,
            member: &OwnerRef,
        ) -> Result<Vec<MembershipRow>, StorageError> {
            if member == &self.member {
                Ok(vec![MembershipRow {
                    group: self.group,
                    relation: self.membership_relation,
                }])
            } else {
                Ok(Vec::new())
            }
        }

        async fn visible_home_owner(
            &self,
            _entity: EntityId,
            _read_owners: &[OwnerRef],
        ) -> Result<Option<OwnerRef>, StorageError> {
            if self.entity_readable {
                Ok(self.home_owner)
            } else {
                Ok(None)
            }
        }

        async fn home_owner(&self, _entity: EntityId) -> Result<Option<OwnerRef>, StorageError> {
            Ok(self.home_owner)
        }
    }

    #[async_trait::async_trait]
    impl OwnerMembershipAdminPort for MembershipStorage {
        async fn bootstrap_group_admin(
            &self,
            _group_id: GroupId,
            _first_admin_user_id: UserId,
            _granted_by: uuid::Uuid,
        ) -> Result<(), StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }

        async fn add_group_member(
            &self,
            _permit: &crate::storage_ports::OwnerWritePermit,
            _group_id: GroupId,
            _member_user_id: UserId,
            _relation: Relation,
            _granted_by: uuid::Uuid,
        ) -> Result<(), StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }

        async fn remove_group_member(
            &self,
            _permit: &crate::storage_ports::OwnerWritePermit,
            _group_id: GroupId,
            _member_user_id: UserId,
        ) -> Result<(), StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }

        async fn list_group_members(
            &self,
            group_id: GroupId,
        ) -> Result<Vec<(UserId, Relation)>, StorageError> {
            if group_id == self.group
                && let OwnerRef::Personal(member) = self.member
            {
                Ok(vec![(member, self.membership_relation)])
            } else {
                Ok(Vec::new())
            }
        }

        async fn list_group_members_page(
            &self,
            group_id: GroupId,
            _after: Option<(UserId, Relation)>,
            _limit: i64,
        ) -> Result<Vec<(UserId, Relation)>, StorageError> {
            self.list_group_members(group_id).await
        }
    }

    #[async_trait::async_trait]
    impl OwnerTransferPort for MembershipStorage {
        async fn transfer_to_owner(
            &self,
            _permit: &crate::storage_ports::OwnerWritePermit,
            _entity: EntityId,
            _to_owner: OwnerRef,
            _surfaces: &proxima_core::owner_inverse::OwnerSurfaces,
        ) -> Result<bool, StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }
    }

    impl SourceBatchPort for MembershipStorage {}

    #[async_trait::async_trait]
    impl SourceCursorPort for MembershipStorage {
        async fn load_source_cursor(
            &self,
            _owner: &Owner,
            _source: &str,
        ) -> Result<Option<Cursor>, StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }

        async fn store_source_cursor(
            &self,
            _permit: &crate::storage_ports::OwnerWritePermit,
            _source: &str,
            _cursor: &Cursor,
        ) -> Result<(), StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }

        async fn source_cursor_age(
            &self,
            _owner: &Owner,
            _source: &str,
        ) -> Result<Option<std::time::Duration>, StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }
    }

    #[async_trait::async_trait]
    impl OwnerInversePort for MembershipStorage {
        async fn erase_group_owner(
            &self,
            _auth: &proxima_core::owner_inverse::EraseAuthorization,
            _group_id: GroupId,
            _object_purge_planned: bool,
            _tables: &proxima_core::owner_inverse::OwnerSurfaces,
        ) -> Result<proxima_core::owner_inverse::OwnerEraseOutcome, StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }

        async fn erase_personal_owner(
            &self,
            _auth: &proxima_core::owner_inverse::EraseAuthorization,
            _user_id: UserId,
            _object_purge_planned: bool,
            _tables: &proxima_core::owner_inverse::OwnerSurfaces,
        ) -> Result<proxima_core::owner_inverse::OwnerEraseOutcome, StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }

        async fn erase_group_source_scope(
            &self,
            _auth: &proxima_core::owner_inverse::EraseAuthorization,
            _group_id: GroupId,
            _source_id: &SourceId,
            _tables: &proxima_core::owner_inverse::OwnerSurfaces,
        ) -> Result<proxima_core::owner_inverse::OwnerEraseOutcome, StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }

        async fn erase_personal_source_scope(
            &self,
            _auth: &proxima_core::owner_inverse::EraseAuthorization,
            _user_id: UserId,
            _source_id: &SourceId,
            _tables: &proxima_core::owner_inverse::OwnerSurfaces,
        ) -> Result<proxima_core::owner_inverse::OwnerEraseOutcome, StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }

        async fn export_owner_bundle(
            &self,
            _auth: &proxima_core::owner_inverse::ExportAuthorization,
            _tables: &proxima_core::owner_inverse::OwnerSurfaces,
        ) -> Result<proxima_core::owner_inverse::OwnerExportBundle, StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects reads".into(),
            ))
        }
    }

    #[async_trait::async_trait]
    impl RegistryProjectionPort for MembershipStorage {
        async fn load_memory_batch_facts(
            &self,
            _owner: &Owner,
            _memory_id: MemoryId,
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
    impl WriteSessionFactory for MembershipStorage {
        async fn begin(&self) -> Result<Box<dyn WriteSession>, StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }
    }

    impl MembershipStorage {
        #[must_use]
        pub(in crate::engine) fn storage_ports(self) -> StoragePorts {
            let storage = Arc::new(self);
            StoragePorts::builder()
                .fact_ingest(storage.clone())
                .mcp_call_read(storage.clone())
                .memory_authoring(storage.clone())
                .memory_read(storage.clone())
                .memory_inspect(storage.clone())
                .embedding_text(storage.clone())
                .embedding_write(storage.clone())
                .embedding_job(storage.clone())
                .embedding_maintenance(storage.clone())
                .goal_write(storage.clone())
                .goal_read(storage.clone())
                .goal_wake_candidate(storage.clone())
                .change_event(storage.clone())
                .citation(storage.clone())
                .owner_access_read(storage.clone())
                .owner_membership_admin(storage.clone())
                .owner_transfer(storage.clone())
                .source_batch(storage.clone())
                .source_cursor(storage.clone())
                .owner_erase(storage.clone())
                .registry_projection(storage.clone())
                .write_session(storage)
                .build()
        }
    }

    #[tokio::test]
    async fn server_resolved_context_uses_owner_role_ceilings() {
        let subject = UserId::new(uuid::Uuid::now_v7());
        let personal = OwnerRef::Personal(subject);
        let group = OwnerRef::Group(GroupId::new(uuid::Uuid::now_v7()));
        let roles = OwnerRoles::for_subject(subject, [(group, Role::editor())]).unwrap();
        let engine = Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests());
        let authz = AuthzContext::server_resolved(roles, AuthPath::HostBearer);

        let access = engine
            .resolve_access(&authz)
            .await
            .expect("server-resolved roles should not need storage expansion");

        assert_eq!(
            access.read_owners().len(),
            2,
            "read set is exactly the caller's own owner plus its group"
        );
        assert!(access.read_owners().contains(&personal));
        assert!(access.read_owners().contains(&group));
        assert!(access.can_write(&personal, Relation::Admin));
        assert!(access.can_write(&personal, Relation::Editor));
        assert!(access.can_write(&personal, Relation::Ingest));
        assert!(!access.can_write(&group, Relation::Admin));
        assert!(access.can_write(&group, Relation::Editor));
        assert!(access.can_write(&group, Relation::Ingest));
    }
}

use crate::access::{AccessKind, AccessScope, Relation, world};
use crate::authz::{AuthPath, AuthzContext};
use crate::error::ProtocolError;
use crate::{MembershipRow, OwnerRef};

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
    pub(in crate::engine) async fn resolve_access(
        &self,
        authz: &AuthzContext,
    ) -> Result<AccessSets, ProtocolError> {
        if authz.auth_path() == AuthPath::Denied {
            return Ok(AccessSets {
                read: Vec::new(),
                write: Vec::new(),
            });
        }

        let mut access = AccessSets {
            read: Vec::new(),
            write: Vec::new(),
        };

        if authz.is_server_resolved() {
            push_role_access(&mut access, authz);
            return Ok(access);
        }

        if authz.accessible_owners().next().is_none() {
            return Ok(access);
        }

        match authz.access_scope() {
            AccessScope::Granted => {
                let principal = authz.principal();
                if matches!(principal, OwnerRef::Personal(_)) && authz.can_access_owner(&principal)
                {
                    push_read_owner(&mut access.read, principal);
                    push_full_authority(&mut access.write, principal);

                    let memberships = self
                        .storage()
                        .access_read
                        .owner_access_read
                        .resolve_membership(&principal)
                        .await
                        .map_err(|err| {
                            ProtocolError::internal(format!("resolve_membership: {err}"))
                        })?;
                    push_memberships(&mut access, memberships);
                }
            }
            AccessScope::Unrestricted => {
                for principal in authz.accessible_owners() {
                    push_read_owner(&mut access.read, principal);
                    push_full_authority(&mut access.write, principal);
                }
            }
        }

        push_read_owner(&mut access.read, world());
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
    push_read_owner(&mut access.read, world());
}

fn push_read_owner(read: &mut Vec<OwnerRef>, owner: OwnerRef) {
    if !read.contains(&owner) {
        read.push(owner);
    }
}

fn push_full_authority(write: &mut Vec<(OwnerRef, Relation)>, principal: OwnerRef) {
    push_write_owner(write, principal, Relation::Admin);
    push_write_owner(write, principal, Relation::Editor);
    push_write_owner(write, principal, Relation::Ingest);
}

fn push_write_owner(write: &mut Vec<(OwnerRef, Relation)>, owner: OwnerRef, relation: Relation) {
    if is_world(&owner) {
        return;
    }
    if !write
        .iter()
        .any(|(candidate, existing)| candidate == &owner && existing == &relation)
    {
        write.push((owner, relation));
    }
}

fn push_memberships(access: &mut AccessSets, memberships: Vec<MembershipRow>) {
    for MembershipRow { group, relation } in memberships {
        let owner = OwnerRef::Group(group);
        push_read_owner(&mut access.read, owner);
        if relation != Relation::Viewer && !is_world(&owner) {
            push_write_owner(&mut access.write, owner, relation);
        }
    }
}

fn is_world(principal: &OwnerRef) -> bool {
    principal == &world()
}

#[cfg(test)]
#[allow(clippy::too_many_lines, clippy::wildcard_imports)]
pub(in crate::engine) mod tests {
    use std::sync::Arc;

    use crate::change_history::{ChangeHistoryRequest, ChangeHistoryResponse};
    use crate::close_batch::CloseBatchOutcome;
    use crate::goal_write::{
        AchieveGoalAtomicRequest, CreateGoalAtomicRequest, DecomposeGoalAtomicRequest,
        DecomposeGoalOutcome, GoalWriteOutcome, ModifyGoalAtomicRequest,
        TransitionGoalAtomicRequest,
    };
    use crate::mcp_call_history::{McpCallHistoryRequest, McpCallHistoryResponse};
    use crate::storage_ports::StoragePorts;
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
            _owner: &OwnerRef,
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
            _sidecar_payload: &SidecarPayload,
            _embedding_model_id: Option<&str>,
        ) -> Result<FactIngestOutcome, StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }

        async fn ingest_fact_with_citation_and_typed_sidecar(
            &self,
            _authorized: &AuthorizedFactWithCitation,
            _sidecar_payload: &SidecarPayload,
            _embedding_model_id: Option<&str>,
        ) -> Result<FactIngestOutcome, StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }
    }

    #[async_trait::async_trait]
    impl McpCallWritePort for MembershipStorage {
        async fn persist_mcp_call_atomic(
            &self,
            _input: &McpCallLogInput,
        ) -> Result<McpCallLogOutcome, StorageError> {
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
        ) -> Result<AuthorDerivedOutcome, StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }

        async fn append_memory_edge(
            &self,
            _edge: &DerivedEdgeSpec<'_>,
            _proof: crate::storage_ports::EdgeWriteProof,
        ) -> Result<EdgeId, StorageError> {
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
                            kind: Some(kind),
                        })
                        .collect()
                })
                .unwrap_or_default())
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
        ) -> Result<Vec<verbs::query::MemorySearchResult>, StorageError> {
            Ok(Vec::new())
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
            })
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
    }

    #[async_trait::async_trait]
    impl EmbeddingTextPort for MembershipStorage {
        async fn load_embedding_text(
            &self,
            _owner: &Owner,
            _entity_kind: EntityKind,
            _memory_id: MemoryId,
        ) -> Result<Option<String>, StorageError> {
            Ok(None)
        }

        async fn list_facts_missing_embedding(
            &self,
            _owner: &Owner,
            _model_id: &str,
            _limit: usize,
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

        async fn fail_embedding_job(
            &self,
            _claim: &EmbeddingJobClaim,
            _error: &str,
        ) -> Result<(), StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }

        async fn enqueue_missing_embedding_jobs(
            &self,
            _owner: &Owner,
            _model_id: &str,
            _limit: i64,
        ) -> Result<u64, StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }

        async fn count_pending_embedding_jobs(&self, _owner: &Owner) -> Result<u64, StorageError> {
            Ok(0)
        }
    }

    #[async_trait::async_trait]
    impl GoalWritePort for MembershipStorage {
        async fn create_goal_atomic(
            &self,
            _req: &CreateGoalAtomicRequest<'_>,
        ) -> Result<GoalWriteOutcome, StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }

        async fn transition_goal_atomic(
            &self,
            _req: &TransitionGoalAtomicRequest<'_>,
        ) -> Result<GoalWriteOutcome, StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }

        async fn achieve_goal_atomic(
            &self,
            _req: &AchieveGoalAtomicRequest<'_>,
        ) -> Result<GoalWriteOutcome, StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }

        async fn modify_goal_atomic(
            &self,
            _req: &ModifyGoalAtomicRequest<'_>,
        ) -> Result<GoalWriteOutcome, StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }

        async fn decompose_goal_atomic(
            &self,
            _req: &DecomposeGoalAtomicRequest<'_>,
        ) -> Result<DecomposeGoalOutcome, StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }
    }

    #[async_trait::async_trait]
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
    impl EdgeReadPort for MembershipStorage {
        async fn read_edges(
            &self,
            _read_owners: &[OwnerRef],
            _req: &verbs::query::EdgeReadRequest,
        ) -> Result<verbs::query::EdgeReadResponse, StorageError> {
            Ok(verbs::query::EdgeReadResponse { edges: Vec::new() })
        }

        async fn edge_exists(
            &self,
            _read_owners: &[OwnerRef],
            _req: &verbs::query::EdgeExistsRequest,
        ) -> Result<verbs::query::EdgeExistsResponse, StorageError> {
            Ok(verbs::query::EdgeExistsResponse { exists: false })
        }
    }

    #[async_trait::async_trait]
    impl CitationPort for MembershipStorage {
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
        ) -> Result<Vec<MemorySnapshot>, StorageError> {
            Ok(Vec::new())
        }

        async fn citation_of_fact(
            &self,
            _fact_memory_id: MemoryId,
        ) -> Result<Option<verbs::query::FactCitationReadback>, StorageError> {
            Ok(None)
        }

        async fn citation_of_entity_head(
            &self,
            _read_owners: &[OwnerRef],
            _fact_entity_id: FactEntityId,
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

        async fn visible_to_any(
            &self,
            _entity: EntityId,
            _read_owners: &[OwnerRef],
        ) -> Result<bool, StorageError> {
            Ok(self.entity_readable)
        }

        async fn home_owner(&self, _entity: EntityId) -> Result<Option<OwnerRef>, StorageError> {
            Ok(self.home_owner)
        }
    }

    #[async_trait::async_trait]
    impl OwnerMembershipAdminPort for MembershipStorage {
        async fn add_group_member(
            &self,
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
            _group_id: GroupId,
            _member_user_id: UserId,
        ) -> Result<(), StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
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
    impl SourceBatchPort for MembershipStorage {
        async fn close_batch(
            &self,
            _principal: &OwnerRef,
            _source_batch_id: SourceBatchId,
        ) -> Result<CloseBatchOutcome, StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }
    }

    #[async_trait::async_trait]
    impl FactRetentionPort for MembershipStorage {
        async fn upsert_fact_retention(
            &self,
            _owner: &Owner,
            _seconds: i64,
        ) -> Result<(), StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }

        async fn get_fact_retention(&self, _owner: &Owner) -> Result<Option<i64>, StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }

        async fn clear_fact_retention(&self, _owner: &Owner) -> Result<bool, StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }
    }

    #[async_trait::async_trait]
    impl ComplianceErasePort for MembershipStorage {
        async fn record_compliance_outcome(
            &self,
            _audit: &proxima_core::compliance::ComplianceAuditContext,
            _outcome: &proxima_core::compliance::ComplianceEraseOutcome,
        ) -> Result<(), StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }

        async fn erase_group_owner_if_abandoned(
            &self,
            _auth: &proxima_core::compliance::EraseAuthorization,
            _group_id: GroupId,
            _fact_sidecar_tables: &[String],
            _goal_sidecar_tables: &[String],
            _edge_sidecar_tables: &[String],
            _citation_mapping_sidecar_tables: &[String],
            _cited_object_sidecar_tables: &[String],
        ) -> Result<proxima_core::compliance::ComplianceEraseOutcome, StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }

        async fn erase_personal_owner_if_drop_verified(
            &self,
            _auth: &proxima_core::compliance::EraseAuthorization,
            _user_id: UserId,
            _fact_sidecar_tables: &[String],
            _goal_sidecar_tables: &[String],
            _edge_sidecar_tables: &[String],
            _citation_mapping_sidecar_tables: &[String],
            _cited_object_sidecar_tables: &[String],
        ) -> Result<proxima_core::compliance::ComplianceEraseOutcome, StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }

        async fn erase_group_source_scope_if_owner_abandoned(
            &self,
            _auth: &proxima_core::compliance::EraseAuthorization,
            _group_id: GroupId,
            _source_id: &SourceId,
            _fact_sidecar_tables: &[String],
            _goal_sidecar_tables: &[String],
            _edge_sidecar_tables: &[String],
            _citation_mapping_sidecar_tables: &[String],
            _cited_object_sidecar_tables: &[String],
        ) -> Result<proxima_core::compliance::ComplianceEraseOutcome, StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }

        async fn erase_personal_source_scope_if_drop_verified(
            &self,
            _auth: &proxima_core::compliance::EraseAuthorization,
            _user_id: UserId,
            _source_id: &SourceId,
            _fact_sidecar_tables: &[String],
            _goal_sidecar_tables: &[String],
            _edge_sidecar_tables: &[String],
            _citation_mapping_sidecar_tables: &[String],
            _cited_object_sidecar_tables: &[String],
        ) -> Result<proxima_core::compliance::ComplianceEraseOutcome, StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
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

    impl MembershipStorage {
        #[must_use]
        pub(in crate::engine) fn storage_ports(self) -> StoragePorts {
            let storage = Arc::new(self);
            StoragePorts::builder()
                .fact_ingest(storage.clone())
                .mcp_call_write(storage.clone())
                .mcp_call_read(storage.clone())
                .memory_authoring(storage.clone())
                .memory_read(storage.clone())
                .memory_inspect(storage.clone())
                .embedding_text(storage.clone())
                .embedding_write(storage.clone())
                .embedding_job(storage.clone())
                .goal_write(storage.clone())
                .goal_read(storage.clone())
                .change_event(storage.clone())
                .edge_read(storage.clone())
                .citation(storage.clone())
                .owner_access_read(storage.clone())
                .owner_membership_admin(storage.clone())
                .source_batch(storage.clone())
                .fact_retention(storage.clone())
                .compliance_erase(storage.clone())
                .registry_projection(storage)
                .build()
        }
    }

    #[tokio::test]
    async fn granted_context_resolves_read_set_and_write_set() {
        let p = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
        let g1 = GroupId::new(uuid::Uuid::now_v7());
        let g1_owner = OwnerRef::Group(g1);
        let world_owner = world();
        let engine = Engine::compose_or_panic_for_tests(
            MembershipStorage {
                member: p,
                group: g1,
                membership_relation: Relation::Viewer,
                home_owner: None,
                entity_readable: false,
                memory_kind: None,
            }
            .storage_ports(),
            |_| {},
        );
        let authz = AuthzContext::scoped_access(
            p,
            [p],
            ToolScope::All,
            AccessScope::Granted,
            AuthPath::HostBearer,
        );

        let access = engine
            .resolve_access(&authz)
            .await
            .expect("granted access should resolve");

        assert!(access.read_owners().contains(&p));
        assert!(access.read_owners().contains(&g1_owner));
        assert!(access.read_owners().contains(&world_owner));
        assert!(access.can_write(&p, Relation::Editor));
        assert!(!access.can_write(&g1_owner, Relation::Editor));
        assert!(!access.can_write(&g1_owner, Relation::Viewer));
        assert!(!access.can_write(&world_owner, Relation::Viewer));
    }

    #[tokio::test]
    async fn granted_context_does_not_treat_foreign_accessible_personal_owner_as_role() {
        let subject = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
        let foreign = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
        let engine = Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests());
        let authz = AuthzContext::scoped_access(
            subject,
            [foreign],
            ToolScope::All,
            AccessScope::Granted,
            AuthPath::HostBearer,
        );

        let access = engine
            .resolve_access(&authz)
            .await
            .expect("foreign accessible owner should deny without storage lookup");

        assert!(!access.read_owners().contains(&foreign));
        assert!(access.read_owners().contains(&world()));
        assert!(!access.can_write(&foreign, Relation::Admin));
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

        assert!(access.read_owners().contains(&world()));
        assert!(access.read_owners().contains(&personal));
        assert!(access.read_owners().contains(&group));
        assert!(access.can_write(&personal, Relation::Admin));
        assert!(access.can_write(&personal, Relation::Editor));
        assert!(access.can_write(&personal, Relation::Ingest));
        assert!(!access.can_write(&group, Relation::Admin));
        assert!(access.can_write(&group, Relation::Editor));
        assert!(access.can_write(&group, Relation::Ingest));
        assert!(!access.can_write(&world(), Relation::Ingest));
    }

    #[tokio::test]
    async fn unrestricted_context_keeps_world_readable_but_never_writable() {
        let p = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
        let g1 = GroupId::new(uuid::Uuid::now_v7());
        let world_owner = world();
        let engine = Engine::compose_or_panic_for_tests(
            MembershipStorage {
                member: p,
                group: g1,
                membership_relation: Relation::Viewer,
                home_owner: None,
                entity_readable: false,
                memory_kind: None,
            }
            .storage_ports(),
            |_| {},
        );
        let authz = AuthzContext::scoped_access(
            p,
            [p, world_owner],
            ToolScope::All,
            AccessScope::Unrestricted,
            AuthPath::HostBearer,
        );

        let access = engine
            .resolve_access(&authz)
            .await
            .expect("unrestricted access should resolve");

        assert!(access.read_owners().contains(&world_owner));
        assert!(access.can_write(&p, Relation::Editor));
        assert!(!access.can_write(&world_owner, Relation::Admin));
        assert!(!access.can_write(&world_owner, Relation::Editor));
        assert!(!access.can_write(&world_owner, Relation::Ingest));
    }

    #[tokio::test]
    async fn granted_world_membership_is_read_only() {
        let p = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
        let world_owner = world();
        let engine = Engine::compose_or_panic_for_tests(
            MembershipStorage {
                member: p,
                group: crate::WORLD_GROUP_ID,
                membership_relation: Relation::Editor,
                home_owner: None,
                entity_readable: false,
                memory_kind: None,
            }
            .storage_ports(),
            |_| {},
        );
        let authz = AuthzContext::scoped_access(
            p,
            [p],
            ToolScope::All,
            AccessScope::Granted,
            AuthPath::HostBearer,
        );

        let access = engine
            .resolve_access(&authz)
            .await
            .expect("granted access should resolve");

        assert!(access.read_owners().contains(&world_owner));
        assert!(!access.can_write(&world_owner, Relation::Editor));
    }
}

use crate::access::{AccessScope, Relation, world};
use crate::authz::{AuthPath, AuthzContext};
use crate::error::ProtocolError;
use crate::{MembershipRow, Principal};

use super::Engine;

pub struct AccessSets {
    read: Vec<Principal>,
    write: Vec<(Principal, Relation)>,
}

impl AccessSets {
    #[must_use]
    pub fn read_owners(&self) -> &[Principal] {
        &self.read
    }

    #[must_use]
    pub fn can_write(&self, owner: &Principal, required: Relation) -> bool {
        self.write
            .iter()
            .any(|(candidate, relation)| candidate == owner && relation.dominates(required))
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
        if authz.auth_path == AuthPath::Denied || authz.identity.accessible_principals.is_empty() {
            return Ok(AccessSets {
                read: Vec::new(),
                write: Vec::new(),
            });
        }

        let mut access = AccessSets {
            read: Vec::new(),
            write: Vec::new(),
        };

        match authz.capabilities.access {
            AccessScope::Granted => {
                for principal in &authz.identity.accessible_principals {
                    if matches!(principal, Principal::User(_)) {
                        access.read.push(principal.clone());
                        push_full_authority(&mut access.write, principal.clone());

                        let memberships = self
                            .storage()
                            .resolve_membership(principal)
                            .await
                            .map_err(|err| {
                                ProtocolError::internal(format!("resolve_membership: {err}"))
                            })?;
                        push_memberships(&mut access, memberships);
                    }
                }
            }
            AccessScope::Unrestricted => {
                for principal in &authz.identity.accessible_principals {
                    access.read.push(principal.clone());
                    push_full_authority(&mut access.write, principal.clone());
                }
            }
        }

        access.read.push(world());
        Ok(access)
    }
}

fn push_full_authority(write: &mut Vec<(Principal, Relation)>, principal: Principal) {
    if is_world(&principal) {
        return;
    }
    write.push((principal.clone(), Relation::Admin));
    write.push((principal.clone(), Relation::Editor));
    write.push((principal, Relation::Ingest));
}

fn push_memberships(access: &mut AccessSets, memberships: Vec<MembershipRow>) {
    for MembershipRow { group, relation } in memberships {
        let owner = Principal::Group(group);
        access.read.push(owner.clone());
        if relation != Relation::Viewer && !is_world(&owner) {
            access.write.push((owner, relation));
        }
    }
}

fn is_world(principal: &Principal) -> bool {
    principal == &world()
}

#[cfg(test)]
#[allow(clippy::too_many_lines, clippy::wildcard_imports)]
pub(in crate::engine) mod tests {
    use std::collections::HashSet;
    use std::sync::Arc;

    use crate::close_batch::CloseBatchOutcome;
    use crate::event_history::{EventHistoryRequest, EventHistoryResponse};
    use crate::fact_cleanup::{CleanupDueFactsOutcome, TombstoneFactOutcome};
    use crate::goal_write::{
        AchieveGoalAtomicRequest, CreateGoalAtomicRequest, DecomposeGoalAtomicRequest,
        DecomposeGoalOutcome, GoalWriteOutcome, ModifyGoalAtomicRequest,
        TransitionGoalAtomicRequest,
    };
    use crate::mcp_call_history::{McpCallHistoryRequest, McpCallHistoryResponse};
    use crate::*;

    #[derive(Debug)]
    pub(in crate::engine) struct MembershipStorage {
        pub(in crate::engine) member: Principal,
        pub(in crate::engine) group: GroupId,
        pub(in crate::engine) membership_relation: Relation,
        pub(in crate::engine) home_owner: Option<Principal>,
        pub(in crate::engine) entity_readable: bool,
        pub(in crate::engine) memory_kind: Option<EntityKind>,
    }

    #[async_trait::async_trait]
    impl Storage for MembershipStorage {
        async fn ingest_event_atomic(
            &self,
            _draft: &EventDraft,
            _embedding_model_id: Option<&str>,
        ) -> Result<EventIngestOutcome, StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }

        async fn persist_mcp_call_atomic(
            &self,
            _input: &McpCallLogInput,
        ) -> Result<McpCallLogOutcome, StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }

        async fn ingest_event_with_typed_sidecar(
            &self,
            _authorized: &AuthorizedEventIngest,
            _sidecar_payload: &SidecarPayload,
            _embedding_model_id: Option<&str>,
        ) -> Result<EventIngestOutcome, StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }

        async fn ingest_fact_with_citation_and_typed_sidecar(
            &self,
            _authorized: &AuthorizedFactWithCitation,
            _sidecar_payload: &SidecarPayload,
            _embedding_model_id: Option<&str>,
        ) -> Result<EventIngestOutcome, StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }

        async fn fact_entity_id_for(
            &self,
            _owner: &Owner,
            _schema_id: &SchemaId,
            _schema_version: SchemaVersion,
            _natural_key: &[String],
        ) -> Result<Option<FactEntityId>, StorageError> {
            Ok(None)
        }

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

        async fn load_fact_text(
            &self,
            _owner: &Owner,
            _memory_id: MemoryId,
        ) -> Result<Option<String>, StorageError> {
            Ok(None)
        }

        async fn load_embedding_text(
            &self,
            _owner: &Owner,
            _entity_kind: EntityKind,
            _memory_id: MemoryId,
        ) -> Result<Option<String>, StorageError> {
            Ok(None)
        }

        async fn upsert_fact_embedding(
            &self,
            _owner: &Owner,
            _memory_id: MemoryId,
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
            _memory_id: MemoryId,
            _model_id: &str,
            _dim: usize,
            _vec: &[f32],
        ) -> Result<(), StorageError> {
            Ok(())
        }

        async fn list_facts_missing_embedding(
            &self,
            _owner: &Owner,
            _model_id: &str,
            _limit: usize,
        ) -> Result<Vec<MemoryId>, StorageError> {
            Ok(Vec::new())
        }

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

        async fn event_history(
            &self,
            _read_owners: &[Principal],
            _req: &EventHistoryRequest,
        ) -> Result<EventHistoryResponse, StorageError> {
            Ok(EventHistoryResponse {
                events: Vec::new(),
                seq_high_water: None,
            })
        }

        async fn read_mcp_call_history(
            &self,
            _req: &McpCallHistoryRequest,
        ) -> Result<McpCallHistoryResponse, StorageError> {
            Ok(McpCallHistoryResponse { calls: Vec::new() })
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
                seq_high_water: None,
            })
        }

        async fn read_edges(
            &self,
            _read_owners: &[Principal],
            _req: &verbs::query::EdgeReadRequest,
        ) -> Result<verbs::query::EdgeReadResponse, StorageError> {
            Ok(verbs::query::EdgeReadResponse { edges: Vec::new() })
        }

        async fn edge_exists(
            &self,
            _read_owners: &[Principal],
            _req: &verbs::query::EdgeExistsRequest,
        ) -> Result<verbs::query::EdgeExistsResponse, StorageError> {
            Ok(verbs::query::EdgeExistsResponse { exists: false })
        }

        async fn search_memories(
            &self,
            _req: &verbs::query::MemorySearchRequest,
            _projections: &[verbs::schema::MemorySearchProjection],
        ) -> Result<Vec<verbs::query::MemorySearchResult>, StorageError> {
            Ok(Vec::new())
        }

        async fn facts_citing_object(
            &self,
            _read_owners: &[Principal],
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
            _read_owners: &[Principal],
            _fact_entity_id: FactEntityId,
        ) -> Result<Option<verbs::query::FactCitationReadback>, StorageError> {
            Ok(None)
        }

        async fn walk_memory_lineage(
            &self,
            _read_owners: &[Principal],
            _req: &verbs::query::MemoryLineageRequest,
        ) -> Result<verbs::query::MemoryLineageResponse, StorageError> {
            Ok(verbs::query::MemoryLineageResponse {
                nodes: Vec::new(),
                edges: Vec::new(),
                truncated: false,
            })
        }

        async fn list_active_goals(
            &self,
            _read_owners: &[Principal],
            _self_perspective_memory_id: MemoryId,
            _limit: usize,
        ) -> Result<Vec<ActiveGoalSummary>, StorageError> {
            Ok(Vec::new())
        }

        async fn resolve_membership(
            &self,
            member: &Principal,
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

        async fn entity_is_readable(
            &self,
            _entity: EntityId,
            _read_owners: &[Principal],
        ) -> Result<bool, StorageError> {
            Ok(self.entity_readable)
        }

        async fn entity_home_owner(
            &self,
            _entity: EntityId,
        ) -> Result<Option<Principal>, StorageError> {
            Ok(self.home_owner.clone())
        }

        async fn add_entity_owner_share(
            &self,
            _entity: EntityId,
            _owner: &Principal,
            _granted_by: Option<uuid::Uuid>,
        ) -> Result<(), StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }

        async fn remove_entity_owner_share(
            &self,
            _entity: EntityId,
            _owner: &Principal,
        ) -> Result<RemoveOwnerOutcome, StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }

        async fn list_entity_owners(
            &self,
            _entity: EntityId,
        ) -> Result<Vec<EntityOwnerRow>, StorageError> {
            Ok(Vec::new())
        }

        async fn list_world_entities(
            &self,
            _limit: usize,
            _sidecars: &[SidecarSpec],
        ) -> Result<Vec<MemorySnapshot>, StorageError> {
            Ok(Vec::new())
        }

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

        async fn close_batch(
            &self,
            _principal: &Principal,
            _source_batch_id: SourceBatchId,
        ) -> Result<CloseBatchOutcome, StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }

        async fn list_personality_instances(
            &self,
            _owner: &Owner,
            _include_tombstoned: bool,
        ) -> Result<Vec<PersonalityInstanceRow>, StorageError> {
            Ok(Vec::new())
        }

        async fn tombstone_personality(
            &self,
            _req: &TombstonePersonalityRequest,
        ) -> Result<TombstonePersonalityResponse, StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }

        async fn instantiate_personality(
            &self,
            _req: &InstantiatePersonalityRequest,
        ) -> Result<InstantiatePersonalityResponse, StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }

        async fn ensure_master_token_personality(
            &self,
            _owner: &Owner,
            _master_token_id: uuid::Uuid,
        ) -> Result<MasterTokenPersonality, StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }

        async fn ensure_subject_personality(
            &self,
            _owner: &Owner,
            _subject: &Principal,
        ) -> Result<MasterTokenPersonality, StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }

        async fn set_wake_entries(
            &self,
            _req: &SetWakeEntriesRequest,
        ) -> Result<SetWakeEntriesResponse, StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }

        async fn set_wake_entries_within(
            &self,
            _owner: &Owner,
            _personality_instance_id: PersonalityInstanceId,
            _mutate: WakeEntriesMutator,
        ) -> Result<SetWakeEntriesResponse, StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }

        async fn list_read_scope(
            &self,
            _req: &ListReadScopeRequest,
        ) -> Result<ListReadScopeResponse, StorageError> {
            Ok(ListReadScopeResponse {
                readable_personality_instance_ids: Vec::new(),
            })
        }

        async fn set_read_scope(
            &self,
            _req: &SetReadScopeRequest,
        ) -> Result<SetReadScopeResponse, StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }

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

        async fn cleanup_due_facts(
            &self,
            _owner: &Owner,
            _fact_sidecar_tables: &[String],
            _edge_sidecar_tables: &[String],
            _citation_mapping_sidecar_tables: &[String],
            _cited_object_sidecar_tables: &[String],
        ) -> Result<CleanupDueFactsOutcome, StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
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
                "MembershipStorage rejects writes".into(),
            ))
        }

        async fn list_change_events_after(
            &self,
            _read_owners: &[Principal],
            _after: uuid::Uuid,
            _limit: usize,
        ) -> Result<Vec<ChangeEventForWake>, StorageError> {
            Ok(Vec::new())
        }

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

        async fn load_perspective_heads(
            &self,
            _owner: &Owner,
            _instance: PersonalityInstanceId,
            _root_perspective_memory_id: MemoryId,
            _sidecars: &[SidecarSpec],
            _limit: usize,
        ) -> Result<Vec<MemorySnapshot>, StorageError> {
            Ok(Vec::new())
        }

        async fn lookup_prior_personality_head(
            &self,
            _owner: &Owner,
            _instance: &PersonalityRef,
            _schema_id: &SchemaId,
        ) -> Result<Option<MemoryId>, StorageError> {
            Ok(None)
        }

        async fn append_personality_memories(
            &self,
            _req: &PersonalityWriteRequest<'_>,
        ) -> Result<PersonalityWriteOutcome, StorageError> {
            Err(StorageError::Internal(
                "MembershipStorage rejects writes".into(),
            ))
        }

        async fn load_memory_by_id(
            &self,
            _memory_id: MemoryId,
            _reader_personality_instance_id: Option<PersonalityInstanceId>,
            _sidecars: &[SidecarSpec],
        ) -> Result<Option<MemorySnapshot>, StorageError> {
            Ok(None)
        }
    }

    #[tokio::test]
    async fn granted_context_resolves_read_set_and_write_set() {
        let p = Principal::User(UserId::new(uuid::Uuid::now_v7()));
        let g1 = GroupId::new(uuid::Uuid::now_v7());
        let g1_owner = Principal::Group(g1);
        let world_owner = world();
        let engine = Engine::compose(
            Arc::new(MembershipStorage {
                member: p.clone(),
                group: g1,
                membership_relation: Relation::Viewer,
                home_owner: None,
                entity_readable: false,
                memory_kind: None,
            }),
            |_| {},
        );
        let authz = AuthzContext {
            identity: Identity {
                principal: p.clone(),
                accessible_principals: HashSet::from([p.clone()]),
                expires_at: None,
                auth_epoch: 0,
            },
            capabilities: CapabilitySet {
                tool_scope: ToolScope::All,
                access: AccessScope::Granted,
            },
            auth_path: AuthPath::HostBearer,
        };

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
    async fn unrestricted_context_keeps_world_readable_but_never_writable() {
        let p = Principal::User(UserId::new(uuid::Uuid::now_v7()));
        let g1 = GroupId::new(uuid::Uuid::now_v7());
        let world_owner = world();
        let engine = Engine::compose(
            Arc::new(MembershipStorage {
                member: p.clone(),
                group: g1,
                membership_relation: Relation::Viewer,
                home_owner: None,
                entity_readable: false,
                memory_kind: None,
            }),
            |_| {},
        );
        let authz = AuthzContext {
            identity: Identity {
                principal: p.clone(),
                accessible_principals: HashSet::from([p.clone(), world_owner.clone()]),
                expires_at: None,
                auth_epoch: 0,
            },
            capabilities: CapabilitySet {
                tool_scope: ToolScope::All,
                access: AccessScope::Unrestricted,
            },
            auth_path: AuthPath::HostBearer,
        };

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
        let p = Principal::User(UserId::new(uuid::Uuid::now_v7()));
        let world_owner = world();
        let engine = Engine::compose(
            Arc::new(MembershipStorage {
                member: p.clone(),
                group: crate::access::WORLD_GROUP_ID,
                membership_relation: Relation::Editor,
                home_owner: None,
                entity_readable: false,
                memory_kind: None,
            }),
            |_| {},
        );
        let authz = AuthzContext {
            identity: Identity {
                principal: p.clone(),
                accessible_principals: HashSet::from([p.clone()]),
                expires_at: None,
                auth_epoch: 0,
            },
            capabilities: CapabilitySet {
                tool_scope: ToolScope::All,
                access: AccessScope::Granted,
            },
            auth_path: AuthPath::HostBearer,
        };

        let access = engine
            .resolve_access(&authz)
            .await
            .expect("granted access should resolve");

        assert!(access.read_owners().contains(&world_owner));
        assert!(!access.can_write(&world_owner, Relation::Editor));
    }
}

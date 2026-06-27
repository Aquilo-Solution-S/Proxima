use crate::access::Relation;
use crate::authz::AuthzContext;
use crate::error::ProtocolError;
use crate::personality::{
    ChangeEventForWake, ListReadScopeRequest, ListReadScopeResponse, MemorySnapshot,
    PersonalityInstanceId, PersonalityInstanceRow, SidecarSpec,
};
use crate::storage::{EdgeEndpointKindRow, MemoryGraphPayloadRow, NeighborEdgeRow, StorageError};
use crate::verbs::query::{FactCitationReadback, MemorySearchRequest, MemorySearchResult};
use crate::verbs::schema::PayloadKind;
use crate::{EdgeId, FactEntityId, MemoryId, Principal, SchemaId, SchemaVersion};

use super::{Engine, PermitMode};

const NEIGHBOR_EDGE_LIMIT: usize = 200;

#[derive(Debug, Clone)]
pub struct SearchReadRequest {
    pub search: MemorySearchRequest,
    pub include_body: bool,
    pub include_neighbor_edges: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchReadResponse {
    pub memories: Vec<MemorySearchResult>,
    pub payloads: Vec<MemoryGraphPayloadRow>,
    pub neighbor_edges: Vec<NeighborEdgeRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetMemoryReadRequest {
    pub memory_id: MemoryId,
    pub reader_personality_instance_id: Option<PersonalityInstanceId>,
    pub include_neighbor_edges: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetMemoryReadResponse {
    pub memory: Option<MemorySnapshot>,
    pub neighbor_edges: Vec<NeighborEdgeRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetGraphReadRequest {
    pub principal: Principal,
    pub include_tombstoned: bool,
}

#[derive(Debug, Clone)]
pub struct GetGraphReadResponse {
    pub pending_embedding_jobs: u64,
    pub fact_retention_seconds: Option<i64>,
    pub personalities: Vec<PersonalityInstanceRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListEventsReadRequest {
    pub principal: Principal,
    pub after: uuid::Uuid,
    pub limit: usize,
}

#[derive(Debug, Clone)]
pub struct ListEventsReadResponse {
    pub events: Vec<ChangeEventForWake>,
    pub edge_endpoint_kinds: Vec<EdgeEndpointKindRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactCitationReadRequest {
    pub principal: Principal,
    pub fact_memory_id: MemoryId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityHeadCitationReadRequest {
    pub principal: Principal,
    pub fact_entity_id: FactEntityId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactsCitingObjectReadRequest {
    pub principal: Principal,
    pub cited_object_id: uuid::Uuid,
}

impl Engine {
    /// Owner-scoped memory search plus domain hydration for MCP projections.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context cannot access `req.search.principal`
    /// or lacks [`Relation::Viewer`]; returns `Internal` when storage reads fail.
    pub async fn search(
        &self,
        authz: &AuthzContext,
        req: &SearchReadRequest,
    ) -> Result<SearchReadResponse, ProtocolError> {
        let search_permit = self
            .authorize_request(authz, &req.search.principal, Relation::Viewer)
            .await?;

        let mut effective = req.search.clone();
        effective.principal = search_permit.owner().clone();
        if let Some(subject_personality) = search_permit.subject_personality() {
            effective.reader_personality_instance_id = Some(subject_personality);
        }
        let memories = self
            .storage
            .search_memories(&effective, self.registry.search_projections())
            .await
            .map_err(|err| storage_error("search_memories", &err))?;

        let memory_ids = memories.iter().map(|row| row.memory_id).collect::<Vec<_>>();
        let payloads = if memory_ids.is_empty() {
            Vec::new()
        } else {
            self.storage
                .load_memory_graph_payloads(search_permit.owner(), &memory_ids, req.include_body)
                .await
                .map_err(|err| storage_error("load_memory_graph_payloads", &err))?
        };
        let neighbor_edges = if req.include_neighbor_edges {
            if memory_ids.is_empty() {
                Vec::new()
            } else {
                self.storage
                    .load_neighbor_memory_edges(
                        search_permit.owner(),
                        &memory_ids,
                        NEIGHBOR_EDGE_LIMIT,
                    )
                    .await
                    .map_err(|err| storage_error("load_neighbor_memory_edges", &err))?
            }
        } else {
            Vec::new()
        };

        Ok(SearchReadResponse {
            memories,
            payloads,
            neighbor_edges,
        })
    }

    /// Single-memory read plus optional neighbor-edge domain rows.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context cannot access `req.memory_id`, and
    /// `Internal` when storage reads fail.
    pub async fn get_memory(
        &self,
        authz: &AuthzContext,
        req: &GetMemoryReadRequest,
    ) -> Result<GetMemoryReadResponse, ProtocolError> {
        let permit = self
            .authorize_entry_request(authz, req.memory_id, Relation::Viewer)
            .await?;
        let sidecars = self.sidecar_specs();
        if let PermitMode::PublicRead { resource } = permit.mode() {
            let memory = self
                .storage
                .load_memory_by_id(permit.owner(), *resource, None, &sidecars)
                .await
                .map_err(|err| storage_error("load_memory_by_id", &err))?;
            return Ok(GetMemoryReadResponse {
                memory,
                neighbor_edges: Vec::new(),
            });
        }
        let (memory_id, reader_personality_instance_id) = match permit.mode() {
            PermitMode::OwnerScoped {
                subject_personality,
            } => (
                req.memory_id,
                (*subject_personality).or(req.reader_personality_instance_id),
            ),
            PermitMode::EntryScoped {
                resource,
                subject_personality,
            } => (*resource, Some(*subject_personality)),
            PermitMode::PublicRead { .. } => unreachable!("PublicRead returned above"),
        };
        let memory = self
            .storage
            .load_memory_by_id(
                permit.owner(),
                memory_id,
                reader_personality_instance_id,
                &sidecars,
            )
            .await
            .map_err(|err| storage_error("load_memory_by_id", &err))?;
        let neighbor_edges = if req.include_neighbor_edges {
            self.storage
                .load_neighbor_memory_edges(permit.owner(), &[memory_id], NEIGHBOR_EDGE_LIMIT)
                .await
                .map_err(|err| storage_error("load_neighbor_memory_edges", &err))?
        } else {
            Vec::new()
        };
        Ok(GetMemoryReadResponse {
            memory,
            neighbor_edges,
        })
    }

    /// Owner-scoped graph overview domain read.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context cannot access `req.principal` or
    /// lacks [`Relation::Admin`], and `Internal` when storage reads fail.
    pub async fn get_graph(
        &self,
        authz: &AuthzContext,
        req: &GetGraphReadRequest,
    ) -> Result<GetGraphReadResponse, ProtocolError> {
        let permit = self
            .authorize_request(authz, &req.principal, Relation::Admin)
            .await?;
        let pending_embedding_jobs = self
            .storage
            .count_pending_embedding_jobs(permit.owner())
            .await
            .map_err(|err| storage_error("count_pending_embedding_jobs", &err))?;
        let personalities = self
            .storage
            .list_personality_instances(permit.owner(), req.include_tombstoned)
            .await
            .map_err(|err| storage_error("list_personality_instances", &err))?;
        let fact_retention_seconds = self
            .storage
            .get_fact_retention(permit.owner())
            .await
            .map_err(|err| storage_error("get_fact_retention", &err))?;
        Ok(GetGraphReadResponse {
            pending_embedding_jobs,
            fact_retention_seconds,
            personalities,
        })
    }

    /// Owner-scoped forward change-event read plus edge endpoint-kind domain rows.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context cannot access `req.principal` or
    /// lacks [`Relation::Viewer`], and `Internal` when storage reads fail.
    pub async fn list_events(
        &self,
        authz: &AuthzContext,
        req: &ListEventsReadRequest,
    ) -> Result<ListEventsReadResponse, ProtocolError> {
        let permit = self
            .authorize_request(authz, &req.principal, Relation::Viewer)
            .await?;
        let events = self
            .storage
            .list_change_events_after(permit.owner(), req.after, req.limit)
            .await
            .map_err(|err| storage_error("list_change_events_after", &err))?;
        let edge_ids = events
            .iter()
            .filter_map(|row| match &row.event.kind {
                crate::change_event::ChangeEventKind::EdgeAppend { edge_id, .. }
                | crate::change_event::ChangeEventKind::EdgeDelete { edge_id, .. } => {
                    Some(EdgeId::new(*edge_id))
                }
                crate::change_event::ChangeEventKind::EntityAppend { .. }
                | crate::change_event::ChangeEventKind::EntityDelete { .. } => None,
            })
            .collect::<Vec<_>>();
        let edge_endpoint_kinds = if edge_ids.is_empty() {
            Vec::new()
        } else {
            self.storage
                .load_edge_endpoint_kinds(&edge_ids)
                .await
                .map_err(|err| storage_error("load_edge_endpoint_kinds", &err))?
        };
        Ok(ListEventsReadResponse {
            events,
            edge_endpoint_kinds,
        })
    }

    /// Owner-scoped inverse citation read for one Fact memory.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context cannot access `req.principal` or
    /// lacks [`Relation::Viewer`], and `Internal` when storage reads fail.
    pub async fn read_fact_citation(
        &self,
        authz: &AuthzContext,
        req: &FactCitationReadRequest,
    ) -> Result<Option<FactCitationReadback>, ProtocolError> {
        let permit = self
            .authorize_request(authz, &req.principal, Relation::Viewer)
            .await?;
        self.storage
            .citation_of_fact(permit.owner(), req.fact_memory_id)
            .await
            .map_err(|err| storage_error("citation_of_fact", &err))
    }

    /// Owner-scoped inverse citation read for a stateful Fact entity head.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context cannot access `req.principal` or
    /// lacks [`Relation::Viewer`], and `Internal` when storage reads fail.
    pub async fn read_entity_head_citation(
        &self,
        authz: &AuthzContext,
        req: &EntityHeadCitationReadRequest,
    ) -> Result<Option<FactCitationReadback>, ProtocolError> {
        let permit = self
            .authorize_request(authz, &req.principal, Relation::Viewer)
            .await?;
        self.storage
            .citation_of_entity_head(permit.owner(), req.fact_entity_id)
            .await
            .map_err(|err| storage_error("citation_of_entity_head", &err))
    }

    /// Owner-scoped citation-to-Fact read-back.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context cannot access `req.principal` or
    /// lacks [`Relation::Viewer`], and `Internal` when storage reads fail.
    pub async fn facts_citing_object(
        &self,
        authz: &AuthzContext,
        req: &FactsCitingObjectReadRequest,
    ) -> Result<Vec<MemorySnapshot>, ProtocolError> {
        let permit = self
            .authorize_request(authz, &req.principal, Relation::Viewer)
            .await?;
        let sidecars = self.sidecar_specs();
        self.storage
            .facts_citing_object(permit.owner(), req.cited_object_id, &sidecars)
            .await
            .map_err(|err| storage_error("facts_citing_object", &err))
    }

    /// Owner-scoped read-scope matrix projection for one reader personality.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context cannot access `req.principal` or
    /// lacks [`Relation::Admin`], and `Internal` when storage reads fail.
    pub async fn list_read_scope(
        &self,
        authz: &AuthzContext,
        req: &ListReadScopeRequest,
    ) -> Result<ListReadScopeResponse, ProtocolError> {
        let permit = self
            .authorize_request(authz, &req.principal, Relation::Admin)
            .await?;
        let effective = ListReadScopeRequest {
            principal: permit.owner().clone(),
            reader_personality_instance_id: req.reader_personality_instance_id,
        };
        self.storage
            .list_read_scope(&effective)
            .await
            .map_err(|err| storage_error("list_read_scope", &err))
    }

    pub(in crate::engine) fn sidecar_specs(&self) -> Vec<SidecarSpec> {
        self.registry
            .list()
            .into_iter()
            .filter(|schema| {
                matches!(
                    schema.kind,
                    PayloadKind::Fact | PayloadKind::Abstraction | PayloadKind::Perspective
                ) && schema.sidecar_table.is_some()
            })
            .map(|schema| SidecarSpec {
                schema_id: SchemaId::new(schema.schema_id.as_str().to_string()),
                schema_version: SchemaVersion::new(schema.schema_version.into_inner()),
                sidecar_table: schema.sidecar_table.expect("filtered to sidecar schemas"),
            })
            .collect()
    }
}

fn storage_error(context: &str, err: &StorageError) -> ProtocolError {
    ProtocolError::internal(format!("{context}: {err}"))
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use crate::access::AccessScope;
    use crate::authz::{AuthPath, AuthzContext, CapabilitySet, Identity, ToolScope};
    use crate::error::ErrorCode;
    use crate::verbs::query::{
        MemorySearchRequest, SearchMode, SearchOrder, SupersessionStatus, TagMatch,
    };
    use crate::{Engine, FactEntityId, FlavorRegistry, GroupId, MemoryId, Principal, UserId};

    use super::{
        EntityHeadCitationReadRequest, FactCitationReadRequest, FactsCitingObjectReadRequest,
        GetGraphReadRequest, GetMemoryReadRequest, ListEventsReadRequest, SearchReadRequest,
    };

    fn engine() -> Engine {
        Engine::new(FlavorRegistry::new().freeze())
    }

    fn owner() -> Principal {
        Principal::User(UserId::new(uuid::Uuid::now_v7()))
    }

    fn group_owner() -> Principal {
        Principal::Group(GroupId::new(uuid::Uuid::now_v7()))
    }

    fn search_req(owner: &Principal) -> SearchReadRequest {
        SearchReadRequest {
            search: MemorySearchRequest {
                principal: owner.clone(),
                query: "needle".into(),
                mode: SearchMode::Lexical,
                supersession: SupersessionStatus::HeadsOnly,
                limit: 8,
                kind: None,
                schema_id: None,
                tags: Vec::new(),
                tag_match: TagMatch::Any,
                since: None,
                until: None,
                order: SearchOrder::Relevance,
                query_embedding: None,
                embedding_model_id: None,
                reader_personality_instance_id: None,
            },
            include_body: false,
            include_neighbor_edges: false,
        }
    }

    fn assert_forbidden(err: &crate::ProtocolError) {
        assert_eq!(err.code, ErrorCode::Forbidden);
    }

    fn granted_authz(owner: &Principal) -> AuthzContext {
        let mut accessible_principals = HashSet::new();
        accessible_principals.insert(owner.clone());
        AuthzContext {
            identity: Identity {
                principal: owner.clone(),
                accessible_principals,
                expires_at: None,
                auth_epoch: 0,
            },
            capabilities: CapabilitySet {
                tool_scope: ToolScope::All,
                access: AccessScope::Granted,
            },
            auth_path: AuthPath::HostBearer,
        }
    }

    #[tokio::test]
    async fn search_denies_denied_context() {
        let owner = owner();
        let err = engine()
            .search(&AuthzContext::denied(&owner), &search_req(&owner))
            .await
            .expect_err("denied context must fail");
        assert_forbidden(&err);
    }

    #[tokio::test]
    async fn search_body_or_neighbor_hydration_requires_viewer_relation() {
        let caller = owner();
        let space = group_owner();
        let mut req = search_req(&space);
        req.include_neighbor_edges = true;
        let err = engine()
            .search(&granted_authz(&caller), &req)
            .await
            .expect_err("neighbor edges require viewer");
        assert_forbidden(&err);
    }

    #[tokio::test]
    async fn get_memory_denies_denied_context() {
        let owner = owner();
        let req = GetMemoryReadRequest {
            memory_id: MemoryId::new(uuid::Uuid::now_v7()),
            reader_personality_instance_id: None,
            include_neighbor_edges: false,
        };
        let err = engine()
            .get_memory(&AuthzContext::denied(&owner), &req)
            .await
            .expect_err("denied context must fail");
        assert_forbidden(&err);
    }

    #[tokio::test]
    async fn get_graph_denies_denied_context() {
        let owner = owner();
        let req = GetGraphReadRequest {
            principal: owner.clone(),
            include_tombstoned: false,
        };
        let err = engine()
            .get_graph(&AuthzContext::denied(&owner), &req)
            .await
            .expect_err("denied context must fail");
        assert_forbidden(&err);
    }

    #[tokio::test]
    async fn list_events_denies_denied_context() {
        let owner = owner();
        let req = ListEventsReadRequest {
            principal: owner.clone(),
            after: uuid::Uuid::nil(),
            limit: 1,
        };
        let err = engine()
            .list_events(&AuthzContext::denied(&owner), &req)
            .await
            .expect_err("denied context must fail");
        assert_forbidden(&err);
    }

    #[tokio::test]
    async fn read_fact_citation_denies_denied_context() {
        let owner = owner();
        let req = FactCitationReadRequest {
            principal: owner.clone(),
            fact_memory_id: MemoryId::new(uuid::Uuid::now_v7()),
        };
        let err = engine()
            .read_fact_citation(&AuthzContext::denied(&owner), &req)
            .await
            .expect_err("denied context must fail");
        assert_forbidden(&err);
    }

    #[tokio::test]
    async fn read_entity_head_citation_denies_denied_context() {
        let owner = owner();
        let req = EntityHeadCitationReadRequest {
            principal: owner.clone(),
            fact_entity_id: FactEntityId::new(uuid::Uuid::now_v7()),
        };
        let err = engine()
            .read_entity_head_citation(&AuthzContext::denied(&owner), &req)
            .await
            .expect_err("denied context must fail");
        assert_forbidden(&err);
    }

    #[tokio::test]
    async fn facts_citing_object_denies_denied_context() {
        let owner = owner();
        let req = FactsCitingObjectReadRequest {
            principal: owner.clone(),
            cited_object_id: uuid::Uuid::now_v7(),
        };
        let err = engine()
            .facts_citing_object(&AuthzContext::denied(&owner), &req)
            .await
            .expect_err("denied context must fail");
        assert_forbidden(&err);
    }

    #[tokio::test]
    async fn list_read_scope_denies_denied_context() {
        let owner = owner();
        let req = crate::ListReadScopeRequest {
            principal: owner.clone(),
            reader_personality_instance_id: crate::PersonalityInstanceId::new(uuid::Uuid::now_v7()),
        };
        let err = engine()
            .list_read_scope(&AuthzContext::denied(&owner), &req)
            .await
            .expect_err("denied context must fail");
        assert_forbidden(&err);
    }
}

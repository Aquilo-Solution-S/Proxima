use crate::access::Relation;
use crate::authz::AuthzContext;
use crate::change_event::{ChangeEventKind, EdgeTargetProjection};
use crate::error::ProtocolError;
use crate::read_models::{ChangeEventForWake, MemorySnapshot, SidecarSpec};
use crate::storage::{EdgeEndpointKindRow, MemoryGraphPayloadRow, NeighborEdgeRow, StorageError};
use crate::storage_ports::ReadVerbStoragePorts;
use crate::verbs::query::{FactCitationReadback, MemorySearchRequest, MemorySearchResult};
use crate::verbs::schema::{MemorySearchProjection, PayloadKind};
use crate::{EdgeId, EntityId, FactEntityId, MemoryId, OwnerRef, SchemaId, SchemaVersion};

use super::Engine;

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
    pub include_neighbor_edges: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetMemoryReadResponse {
    pub memory: Option<MemorySnapshot>,
    pub neighbor_edges: Vec<NeighborEdgeRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetGraphReadRequest {
    pub principal: OwnerRef,
    pub include_tombstoned: bool,
}

#[derive(Debug, Clone)]
pub struct GetGraphReadResponse {
    pub pending_embedding_jobs: u64,
    pub fact_retention_seconds: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListChangeEventsReadRequest {
    pub principal: OwnerRef,
    pub after: uuid::Uuid,
    pub limit: usize,
}

#[derive(Debug, Clone)]
pub struct ListChangeEventsReadResponse {
    pub events: Vec<ChangeEventForWake>,
    pub edge_endpoint_kinds: Vec<EdgeEndpointKindRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactCitationReadRequest {
    pub principal: OwnerRef,
    pub fact_memory_id: MemoryId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityHeadCitationReadRequest {
    pub principal: OwnerRef,
    pub fact_entity_id: FactEntityId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactsCitingObjectReadRequest {
    pub principal: OwnerRef,
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
        let read_permit = self
            .authorize_request(authz, &req.search.principal, Relation::Viewer)
            .await?;
        search_authorized(
            &self.storage.read_verb,
            self.registry.search_projections(),
            std::slice::from_ref(read_permit.owner()),
            read_permit.owner(),
            req,
        )
        .await
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
        let _permit = self
            .authorize_entry_read(authz, EntityId::Memory(req.memory_id))
            .await?;
        let read_owners = self.authorize_read(authz).await?;
        let sidecars = self.sidecar_specs();
        get_memory_authorized(&self.storage.read_verb, &read_owners, &sidecars, req).await
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
        get_graph_authorized(
            &self.storage.read_verb,
            permit.owner(),
            req.include_tombstoned,
        )
        .await
    }

    /// Read-set-scoped forward change-event read plus edge endpoint-kind domain rows.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context authorizes no read owners, and
    /// `Internal` when storage reads fail.
    pub async fn list_change_events(
        &self,
        authz: &AuthzContext,
        req: &ListChangeEventsReadRequest,
    ) -> Result<ListChangeEventsReadResponse, ProtocolError> {
        let read_owners = self.authorize_read(authz).await?;
        list_change_events_authorized(&self.storage.read_verb, &read_owners, req).await
    }

    /// Confirmed-id inverse citation read for one Fact memory.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context cannot access `req.fact_memory_id`,
    /// and `Internal` when storage reads fail.
    pub async fn read_fact_citation(
        &self,
        authz: &AuthzContext,
        req: &FactCitationReadRequest,
    ) -> Result<Option<FactCitationReadback>, ProtocolError> {
        self.authorize_entry_read(authz, EntityId::Memory(req.fact_memory_id))
            .await?;
        read_fact_citation_authorized(&self.storage.read_verb, req.fact_memory_id).await
    }

    /// Read-set-scoped inverse citation read for a stateful Fact entity head.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context authorizes no read owners, and
    /// `Internal` when storage reads fail.
    pub async fn read_entity_head_citation(
        &self,
        authz: &AuthzContext,
        req: &EntityHeadCitationReadRequest,
    ) -> Result<Option<FactCitationReadback>, ProtocolError> {
        let read_owners = self.authorize_read(authz).await?;
        read_entity_head_citation_authorized(
            &self.storage.read_verb,
            &read_owners,
            req.fact_entity_id,
        )
        .await
    }

    /// Read-set-scoped citation-to-Fact read-back.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context authorizes no read owners, and
    /// `Internal` when storage reads fail.
    pub async fn facts_citing_object(
        &self,
        authz: &AuthzContext,
        req: &FactsCitingObjectReadRequest,
    ) -> Result<Vec<MemorySnapshot>, ProtocolError> {
        let read_owners = self.authorize_read(authz).await?;
        let sidecars = self.sidecar_specs();
        facts_citing_object_authorized(
            &self.storage.read_verb,
            &read_owners,
            req.cited_object_id,
            &sidecars,
        )
        .await
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

pub(in crate::engine) async fn search_authorized(
    ports: &ReadVerbStoragePorts,
    search_projections: &[MemorySearchProjection],
    read_owners: &[OwnerRef],
    hydration_owner: &OwnerRef,
    req: &SearchReadRequest,
) -> Result<SearchReadResponse, ProtocolError> {
    let mut effective = req.search.clone();
    effective.read_owners = read_owners.to_vec();
    let memories = ports
        .memory_read
        .search_memories(&effective, search_projections)
        .await
        .map_err(|err| storage_error("search_memories", &err))?;

    let memory_ids = memories.iter().map(|row| row.memory_id).collect::<Vec<_>>();
    let payloads = if memory_ids.is_empty() {
        Vec::new()
    } else {
        ports
            .memory_read
            .load_memory_graph_payloads(hydration_owner, &memory_ids, req.include_body)
            .await
            .map_err(|err| storage_error("load_memory_graph_payloads", &err))?
    };
    let neighbor_edges = if req.include_neighbor_edges {
        if memory_ids.is_empty() {
            Vec::new()
        } else {
            ports
                .memory_read
                .load_neighbor_memory_edges(read_owners, &memory_ids, NEIGHBOR_EDGE_LIMIT)
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

pub(in crate::engine) async fn get_memory_authorized(
    ports: &ReadVerbStoragePorts,
    read_owners: &[OwnerRef],
    sidecars: &[SidecarSpec],
    req: &GetMemoryReadRequest,
) -> Result<GetMemoryReadResponse, ProtocolError> {
    let memory = ports
        .memory_inspect
        .load_memory_by_id(req.memory_id, sidecars)
        .await
        .map_err(|err| storage_error("load_memory_by_id", &err))?;
    let neighbor_edges = if req.include_neighbor_edges {
        ports
            .memory_read
            .load_neighbor_memory_edges(read_owners, &[req.memory_id], NEIGHBOR_EDGE_LIMIT)
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

pub(in crate::engine) async fn get_graph_authorized(
    ports: &ReadVerbStoragePorts,
    owner: &OwnerRef,
    _include_tombstoned: bool,
) -> Result<GetGraphReadResponse, ProtocolError> {
    let pending_embedding_jobs = ports
        .embedding_job
        .count_pending_embedding_jobs(owner)
        .await
        .map_err(|err| storage_error("count_pending_embedding_jobs", &err))?;
    let fact_retention_seconds = ports
        .fact_retention
        .get_fact_retention(owner)
        .await
        .map_err(|err| storage_error("get_fact_retention", &err))?;
    Ok(GetGraphReadResponse {
        pending_embedding_jobs,
        fact_retention_seconds,
    })
}

pub(in crate::engine) async fn list_change_events_authorized(
    ports: &ReadVerbStoragePorts,
    read_owners: &[OwnerRef],
    req: &ListChangeEventsReadRequest,
) -> Result<ListChangeEventsReadResponse, ProtocolError> {
    let events = ports
        .change_event
        .list_change_events_after(read_owners, req.after, req.limit)
        .await
        .map_err(|err| storage_error("list_change_events_after", &err))?;
    let target_visible_by_edge = events
        .iter()
        .filter_map(|row| match &row.event.kind {
            ChangeEventKind::EdgeAppend {
                edge_id,
                target: EdgeTargetProjection::Visible { .. },
                ..
            }
            | ChangeEventKind::EdgeDelete {
                edge_id,
                target: EdgeTargetProjection::Visible { .. },
                ..
            } => Some((*edge_id, true)),
            ChangeEventKind::EdgeAppend { edge_id, .. }
            | ChangeEventKind::EdgeDelete { edge_id, .. } => Some((*edge_id, false)),
            ChangeEventKind::EntityAppend { .. } | ChangeEventKind::EntityDelete { .. } => None,
        })
        .collect::<std::collections::HashMap<_, _>>();
    let edge_ids = target_visible_by_edge
        .keys()
        .copied()
        .map(EdgeId::new)
        .collect::<Vec<_>>();
    let edge_endpoint_kinds = if edge_ids.is_empty() {
        Vec::new()
    } else {
        ports
            .memory_read
            .load_edge_endpoint_kinds(&edge_ids)
            .await
            .map_err(|err| storage_error("load_edge_endpoint_kinds", &err))?
            .into_iter()
            .map(|mut row| {
                if !target_visible_by_edge
                    .get(&row.edge_id.into_inner())
                    .copied()
                    .unwrap_or(false)
                {
                    row.target_kind = None;
                }
                row
            })
            .collect()
    };
    Ok(ListChangeEventsReadResponse {
        events,
        edge_endpoint_kinds,
    })
}

pub(in crate::engine) async fn read_fact_citation_authorized(
    ports: &ReadVerbStoragePorts,
    fact_memory_id: MemoryId,
) -> Result<Option<FactCitationReadback>, ProtocolError> {
    ports
        .citation
        .citation_of_fact(fact_memory_id)
        .await
        .map_err(|err| storage_error("citation_of_fact", &err))
}

pub(in crate::engine) async fn read_entity_head_citation_authorized(
    ports: &ReadVerbStoragePorts,
    read_owners: &[OwnerRef],
    fact_entity_id: FactEntityId,
) -> Result<Option<FactCitationReadback>, ProtocolError> {
    ports
        .citation
        .citation_of_entity_head(read_owners, fact_entity_id)
        .await
        .map_err(|err| storage_error("citation_of_entity_head", &err))
}

pub(in crate::engine) async fn facts_citing_object_authorized(
    ports: &ReadVerbStoragePorts,
    read_owners: &[OwnerRef],
    cited_object_id: uuid::Uuid,
    sidecars: &[SidecarSpec],
) -> Result<Vec<MemorySnapshot>, ProtocolError> {
    ports
        .citation
        .facts_citing_object(read_owners, cited_object_id, sidecars)
        .await
        .map_err(|err| storage_error("facts_citing_object", &err))
}

fn storage_error(context: &str, err: &StorageError) -> ProtocolError {
    ProtocolError::internal(format!("{context}: {err}"))
}

#[cfg(test)]
mod tests {
    use crate::access::AccessScope;
    use crate::authz::{AuthPath, AuthzContext, ToolScope};
    use crate::error::ErrorCode;
    use crate::verbs::query::{
        MemorySearchRequest, SearchMode, SearchOrder, SupersessionStatus, TagMatch,
    };
    use crate::{Engine, FactEntityId, FlavorRegistry, GroupId, MemoryId, OwnerRef, UserId};

    type ResolvedAuthz = AuthzContext;

    use super::{
        EntityHeadCitationReadRequest, FactCitationReadRequest, FactsCitingObjectReadRequest,
        GetGraphReadRequest, GetMemoryReadRequest, ListChangeEventsReadRequest, SearchReadRequest,
    };

    fn engine() -> Engine {
        Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests())
    }

    fn owner() -> OwnerRef {
        OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()))
    }

    fn group_owner() -> OwnerRef {
        OwnerRef::Group(GroupId::new(uuid::Uuid::now_v7()))
    }

    fn search_req(owner: &OwnerRef) -> SearchReadRequest {
        SearchReadRequest {
            search: MemorySearchRequest {
                principal: *owner,
                read_owners: vec![*owner],
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
            },
            include_body: false,
            include_neighbor_edges: false,
        }
    }

    fn assert_forbidden(err: &crate::ProtocolError) {
        assert_eq!(err.code, ErrorCode::Forbidden);
    }

    fn granted_authz(owner: &OwnerRef) -> ResolvedAuthz {
        AuthzContext::scoped_access(
            *owner,
            [*owner],
            ToolScope::All,
            AccessScope::Granted,
            AuthPath::HostBearer,
        )
    }

    #[tokio::test]
    async fn search_denies_denied_context() {
        let owner = owner();
        let err = engine()
            .search(&AuthzContext::denied_for_owner(&owner), &search_req(&owner))
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
            include_neighbor_edges: false,
        };
        let err = engine()
            .get_memory(&AuthzContext::denied_for_owner(&owner), &req)
            .await
            .expect_err("denied context must fail");
        assert_forbidden(&err);
    }

    #[tokio::test]
    async fn get_graph_denies_denied_context() {
        let owner = owner();
        let req = GetGraphReadRequest {
            principal: owner,
            include_tombstoned: false,
        };
        let err = engine()
            .get_graph(&AuthzContext::denied_for_owner(&owner), &req)
            .await
            .expect_err("denied context must fail");
        assert_forbidden(&err);
    }

    #[tokio::test]
    async fn list_change_events_denies_denied_context() {
        let owner = owner();
        let req = ListChangeEventsReadRequest {
            principal: owner,
            after: uuid::Uuid::nil(),
            limit: 1,
        };
        let err = engine()
            .list_change_events(&AuthzContext::denied_for_owner(&owner), &req)
            .await
            .expect_err("denied context must fail");
        assert_forbidden(&err);
    }

    #[tokio::test]
    async fn read_fact_citation_denies_denied_context() {
        let owner = owner();
        let req = FactCitationReadRequest {
            principal: owner,
            fact_memory_id: MemoryId::new(uuid::Uuid::now_v7()),
        };
        let err = engine()
            .read_fact_citation(&AuthzContext::denied_for_owner(&owner), &req)
            .await
            .expect_err("denied context must fail");
        assert_forbidden(&err);
    }

    #[tokio::test]
    async fn read_entity_head_citation_denies_denied_context() {
        let owner = owner();
        let req = EntityHeadCitationReadRequest {
            principal: owner,
            fact_entity_id: FactEntityId::new(uuid::Uuid::now_v7()),
        };
        let err = engine()
            .read_entity_head_citation(&AuthzContext::denied_for_owner(&owner), &req)
            .await
            .expect_err("denied context must fail");
        assert_forbidden(&err);
    }

    #[tokio::test]
    async fn facts_citing_object_denies_denied_context() {
        let owner = owner();
        let req = FactsCitingObjectReadRequest {
            principal: owner,
            cited_object_id: uuid::Uuid::now_v7(),
        };
        let err = engine()
            .facts_citing_object(&AuthzContext::denied_for_owner(&owner), &req)
            .await
            .expect_err("denied context must fail");
        assert_forbidden(&err);
    }
}

use crate::access::Relation;
use crate::authz::AuthzContext;
use crate::change_event::{ChangeEventKind, EdgeTargetProjection};
use crate::error::ProtocolError;
use crate::read_models::{
    ChangeEventForWake, GoalWakeCandidate, GoalWakeCandidateRequest, MemorySnapshot, SidecarSpec,
};
use crate::storage::{EdgeEndpointKindRow, MemoryGraphPayloadRow, NeighborEdgeRow, StorageError};
use crate::storage_ports::ReadVerbStoragePorts;
use crate::verbs::query::{
    FactCitationReadback, MAX_RELEVANCE_SEARCH_DEPTH, MemorySearchRequest, MemorySearchResult,
    SearchCursor,
};
use crate::verbs::schema::{MemorySearchProjection, PayloadKind};
use crate::{
    EdgeId, EntityId, EntityKind, FactEntityId, MemoryId, OwnerRef, SchemaId, SchemaVersion,
};

use super::Engine;

const NEIGHBOR_EDGE_LIMIT: usize = 200;
pub const MAX_WAKE_CANDIDATE_LIMIT: usize = 200;

#[derive(Debug, Clone)]
pub struct SearchReadRequest {
    pub search: MemorySearchRequest,
    pub include_body: bool,
    pub include_neighbor_edges: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchReadResponse {
    pub memories: Vec<MemorySearchResult>,
    /// At least one further match exists past the last returned row;
    /// see [`crate::verbs::query::MemorySearchPage`].
    pub has_more: bool,
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
    pub owner: OwnerRef,
}

#[derive(Debug, Clone)]
pub struct GetGraphReadResponse {
    pub pending_embedding_jobs: u64,
    pub failed_embedding_jobs: u64,
    pub fact_retention_seconds: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListChangeEventsReadRequest {
    pub after: uuid::Uuid,
    pub limit: usize,
}

#[derive(Debug, Clone)]
pub struct ListChangeEventsReadResponse {
    pub events: Vec<ChangeEventForWake>,
    pub edge_endpoint_kinds: Vec<EdgeEndpointKindRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListWakeCandidatesReadRequest {
    pub trigger_fact_id: MemoryId,
    pub limit: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListWakeCandidatesReadResponse {
    pub candidates: Vec<GoalWakeCandidate>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactCitationReadRequest {
    pub fact_memory_id: MemoryId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityHeadCitationReadRequest {
    pub fact_entity_id: FactEntityId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactsCitingObjectReadRequest {
    pub cited_object_id: uuid::Uuid,
}

impl Engine {
    /// Owner-scoped memory search plus domain hydration for MCP projections.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` when `min_score`/`semantic_weight` fall
    /// outside `0.0..=1.0`, or when `after` disagrees with `order` or
    /// exceeds the relevance pagination depth bound; `Forbidden` when the
    /// context cannot access `req.search.owner` or lacks
    /// [`Relation::Viewer`]; `Internal` when storage reads fail.
    pub async fn search(
        &self,
        authz: &AuthzContext,
        req: &SearchReadRequest,
    ) -> Result<SearchReadResponse, ProtocolError> {
        validate_search_request(&req.search)?;
        let read_permit = self
            .authorize_request(authz, &req.search.owner, Relation::Viewer)
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
    /// Returns `Forbidden` when the context cannot access `req.owner` or
    /// lacks [`Relation::Admin`], and `Internal` when storage reads fail.
    pub async fn get_graph(
        &self,
        authz: &AuthzContext,
        req: &GetGraphReadRequest,
    ) -> Result<GetGraphReadResponse, ProtocolError> {
        let permit = self
            .authorize_request(authz, &req.owner, Relation::Admin)
            .await?;
        get_graph_authorized(&self.storage.read_verb, permit.owner()).await
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

    /// Wake-candidate admission read: armed Active Goal heads whose wake
    /// trigger matches one readable trigger Fact, narrowed to the caller's
    /// resolved read/write owner sets and the intersection of the caller's
    /// tool scope with the engine's composed deployment tool scope
    /// (`Engine::with_deployment_tool_scope`).
    ///
    /// This is a read model only — no scheduler, executor, tool invocation
    /// row, or emitted Fact write path exists behind it (04 §Execution).
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context cannot read `req.trigger_fact_id`;
    /// `InvalidArgument` when the trigger is not a live Fact memory or
    /// `req.limit` is zero; and `Internal` when storage reads fail.
    pub async fn list_goal_wake_candidates(
        &self,
        authz: &AuthzContext,
        req: &ListWakeCandidatesReadRequest,
    ) -> Result<ListWakeCandidatesReadResponse, ProtocolError> {
        if req.limit == 0 {
            return Err(ProtocolError::invalid_argument(
                "limit",
                "limit must be at least 1",
            ));
        }
        let permit = self
            .authorize_entry_read(authz, EntityId::Memory(req.trigger_fact_id))
            .await?;
        let trigger_owner = *permit.owner();
        let access = self.resolve_access(authz).await?;
        let snapshot = self
            .storage
            .read_verb
            .memory_inspect
            .load_memory_by_id(req.trigger_fact_id, &[])
            .await
            .map_err(|err| storage_error("load_memory_by_id", &err))?
            .ok_or_else(|| {
                ProtocolError::invalid_argument("fact", "wake trigger fact not found")
            })?;
        if snapshot.kind != EntityKind::Fact.as_str() {
            return Err(ProtocolError::invalid_argument(
                "fact",
                "wake trigger must be a Fact memory",
            ));
        }
        let actor_write_owners = access.write_owners_for(Relation::Editor);
        let candidates = self
            .storage
            .read_verb
            .goal_wake_candidate
            .list_goal_wake_candidates(&GoalWakeCandidateRequest {
                actor_read_owners: access.read_owners(),
                actor_write_owners: &actor_write_owners,
                trigger_owner,
                trigger_fact_id: req.trigger_fact_id,
                trigger_schema_id: &snapshot.schema_id,
                trigger_schema_version: snapshot.schema_version,
                actor_tool_scope: authz.tool_scope(),
                deployment_tool_scope: &self.deployment_tool_scope,
                limit: req.limit.min(MAX_WAKE_CANDIDATE_LIMIT),
            })
            .await
            .map_err(|err| storage_error("list_goal_wake_candidates", &err))?;
        Ok(ListWakeCandidatesReadResponse { candidates })
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

/// Bounds check for caller-supplied search knobs, shared by every
/// entry into the search verb. Storage re-validates the cursor/order
/// pairing as defense in depth for direct port callers.
fn validate_search_request(search: &MemorySearchRequest) -> Result<(), ProtocolError> {
    if let Some(floor) = search.min_score
        && !(floor.is_finite() && (0.0..=1.0).contains(&floor))
    {
        return Err(ProtocolError::invalid_argument(
            "min_score",
            "min_score must be within 0.0..=1.0",
        ));
    }
    if let Some(weight) = search.semantic_weight
        && !(weight.is_finite() && (0.0..=1.0).contains(&weight))
    {
        return Err(ProtocolError::invalid_argument(
            "semantic_weight",
            "semantic_weight must be within 0.0..=1.0",
        ));
    }
    match search.after {
        Some(after) if after.order() != search.order => Err(ProtocolError::invalid_argument(
            "cursor",
            "search cursor order does not match request order",
        )),
        Some(after @ SearchCursor::Relevance { .. })
            if after.seen() > MAX_RELEVANCE_SEARCH_DEPTH =>
        {
            Err(ProtocolError::invalid_argument(
                "cursor",
                "relevance pagination depth exceeded; narrow the query or switch to order=recency",
            ))
        }
        _ => Ok(()),
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
    let page = ports
        .memory_read
        .search_memories(&effective, search_projections)
        .await
        .map_err(|err| storage_error("search_memories", &err))?;
    let memories = page.results;

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
        has_more: page.has_more,
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
) -> Result<GetGraphReadResponse, ProtocolError> {
    let (job_status, fact_retention_seconds) = tokio::try_join!(
        async {
            ports
                .embedding_job
                .count_embedding_job_status(owner)
                .await
                .map_err(|err| storage_error("count_embedding_job_status", &err))
        },
        async {
            ports
                .fact_retention
                .get_fact_retention(owner)
                .await
                .map_err(|err| storage_error("get_fact_retention", &err))
        },
    )?;
    Ok(GetGraphReadResponse {
        pending_embedding_jobs: job_status.pending,
        failed_embedding_jobs: job_status.failed,
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
    use crate::authz::{AuthPath, AuthzContext};
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
                owner: *owner,
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
                min_score: None,
                semantic_weight: None,
                after: None,
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
        AuthzContext::single_owner(owner, AuthPath::HostBearer)
    }

    #[tokio::test]
    async fn search_rejects_out_of_range_floor_weight_and_cursor() {
        use crate::verbs::query::{MAX_RELEVANCE_SEARCH_DEPTH, SearchCursor};

        let owner = owner();
        let engine = engine();
        let authz = granted_authz(&owner);

        let mut bad_floor = search_req(&owner);
        bad_floor.search.min_score = Some(1.5);
        let err = engine
            .search(&authz, &bad_floor)
            .await
            .expect_err("floor above 1.0 must fail");
        assert_eq!(err.code, ErrorCode::InvalidArgument);

        let mut nan_weight = search_req(&owner);
        nan_weight.search.semantic_weight = Some(f32::NAN);
        let err = engine
            .search(&authz, &nan_weight)
            .await
            .expect_err("NaN weight must fail");
        assert_eq!(err.code, ErrorCode::InvalidArgument);

        let mut order_mismatch = search_req(&owner);
        order_mismatch.search.after = Some(SearchCursor::Recency {
            created_at: time::OffsetDateTime::UNIX_EPOCH,
            memory_id: MemoryId::new(uuid::Uuid::now_v7()),
            seen: 8,
        });
        let err = engine
            .search(&authz, &order_mismatch)
            .await
            .expect_err("recency cursor with relevance order must fail");
        assert_eq!(err.code, ErrorCode::InvalidArgument);

        let mut too_deep = search_req(&owner);
        too_deep.search.after = Some(SearchCursor::Relevance {
            score_bits: 0.5_f32.to_bits(),
            memory_id: MemoryId::new(uuid::Uuid::now_v7()),
            seen: MAX_RELEVANCE_SEARCH_DEPTH + 1,
        });
        let err = engine
            .search(&authz, &too_deep)
            .await
            .expect_err("relevance depth beyond the bound must fail");
        assert_eq!(err.code, ErrorCode::InvalidArgument);
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
        let req = GetGraphReadRequest { owner };
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
    async fn list_goal_wake_candidates_denies_denied_context() {
        let owner = owner();
        let req = super::ListWakeCandidatesReadRequest {
            trigger_fact_id: MemoryId::new(uuid::Uuid::now_v7()),
            limit: 10,
        };
        let err = engine()
            .list_goal_wake_candidates(&AuthzContext::denied_for_owner(&owner), &req)
            .await
            .expect_err("denied context must fail");
        assert_forbidden(&err);
    }

    #[tokio::test]
    async fn list_goal_wake_candidates_rejects_zero_limit() {
        let owner = owner();
        let req = super::ListWakeCandidatesReadRequest {
            trigger_fact_id: MemoryId::new(uuid::Uuid::now_v7()),
            limit: 0,
        };
        let err = engine()
            .list_goal_wake_candidates(&granted_authz(&owner), &req)
            .await
            .expect_err("zero limit must fail");
        assert_eq!(err.code, ErrorCode::InvalidArgument);
    }

    #[tokio::test]
    async fn read_fact_citation_denies_denied_context() {
        let owner = owner();
        let req = FactCitationReadRequest {
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
            cited_object_id: uuid::Uuid::now_v7(),
        };
        let err = engine()
            .facts_citing_object(&AuthzContext::denied_for_owner(&owner), &req)
            .await
            .expect_err("denied context must fail");
        assert_forbidden(&err);
    }
}

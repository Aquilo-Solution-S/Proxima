use crate::access::Relation;
use crate::authz::AuthzContext;
use crate::edge::Edge;
use crate::error::ProtocolError;
use crate::read_models::{
    ChangeEventForWake, GoalWakeCandidate, GoalWakeCandidateRequest, MemorySnapshot, SidecarSpec,
};
use crate::storage::{MemoryGraphIdentity, MemoryGraphPayloadRow, StorageError};
use crate::storage_ports::ReadVerbStoragePorts;
use crate::verbs::query::{
    FactCitationReadback, MAX_RELEVANCE_SEARCH_DEPTH, MemorySearchRequest, MemorySearchResult,
    SearchCursor, SearchMode,
};
use crate::verbs::schema::{MemorySearchProjection, PayloadKind};
use crate::{EntityKind, MemoryId, OwnerRef, SchemaId, SchemaVersion};

use super::Engine;
use super::pipeline::ENTRY_NOT_FOUND_MESSAGE;

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
    pub neighbor_edges: Vec<Edge>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetMemoryReadRequest {
    pub memory_id: MemoryId,
    pub include_neighbor_edges: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetMemoryReadResponse {
    pub memory: Option<MemorySnapshot>,
    pub neighbor_edges: Vec<Edge>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetMemoriesReadRequest {
    pub memory_ids: Vec<MemoryId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetMemoriesReadResponse {
    /// Snapshots for the visible subset of the requested ids; unknown and
    /// invisible ids are simply absent (deliberately indistinguishable).
    pub memories: Vec<MemorySnapshot>,
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListWakeCandidatesReadRequest {
    pub trigger_fact_id: MemoryId,
    pub limit: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListWakeCandidatesReadResponse {
    pub candidates: Vec<GoalWakeCandidate>,
    /// More admitted candidates exist past `limit`; re-poll with a higher
    /// limit (bounded by [`MAX_WAKE_CANDIDATE_LIMIT`]) or narrow the
    /// trigger. Truncation is a signal, never silent.
    pub has_more: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactCitationReadRequest {
    pub fact_memory_id: MemoryId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactsCitingObjectReadRequest {
    pub cited_object_id: uuid::Uuid,
    /// Max citing Facts per page.
    pub limit: u32,
    /// Keyset resume point from a previous page's `next_cursor`; `None`
    /// starts from the newest citing Fact.
    pub after: Option<crate::verbs::query::FactCitationCursor>,
}

impl Engine {
    /// Owner-scoped memory search plus domain hydration for MCP projections.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` when `min_score`/`semantic_weight` fall
    /// outside `0.0..=1.0`, when `semantic_weight` is set on a mode other
    /// than [`crate::verbs::query::SearchMode::Hybrid`], or when `after`
    /// disagrees with `order` or exceeds the relevance pagination depth
    /// bound; `Forbidden` when the context cannot access
    /// `req.search.owner` or lacks [`Relation::Viewer`]; `Internal` when
    /// storage reads fail.
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
            req,
        )
        .await
    }

    /// Single-memory read plus optional neighbor-edge domain rows.
    ///
    /// # Errors
    ///
    /// Returns `NotFound` when `req.memory_id` does not exist or is not
    /// visible to the caller (deliberately indistinguishable), `Forbidden`
    /// when the context itself authorizes nothing, and `Internal` when
    /// storage reads fail.
    pub async fn get_memory(
        &self,
        authz: &AuthzContext,
        req: &GetMemoryReadRequest,
    ) -> Result<GetMemoryReadResponse, ProtocolError> {
        // Home, visibility, and the row are one owner-scoped inspect.
        // Absent and invisible are both "not returned" — do not probe
        // `home_owner` / `visible_to_any` first.
        let read_owners = self.authorize_read(authz).await?;
        let sidecars = self.sidecar_specs();
        get_memory_authorized(&self.storage.read_verb, &read_owners, &sidecars, req).await
    }

    /// Batch single-memory read: snapshots for the subset of
    /// `req.memory_ids` visible to the caller's read-owner set. Unknown
    /// and invisible ids are omitted rather than erroring, so a caller
    /// can only observe "not returned" (deliberately indistinguishable).
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context authorizes no read owners, and
    /// `Internal` when storage reads fail.
    pub async fn get_memories(
        &self,
        authz: &AuthzContext,
        req: &GetMemoriesReadRequest,
    ) -> Result<GetMemoriesReadResponse, ProtocolError> {
        let read_owners = self.authorize_read(authz).await?;
        let sidecars = self.sidecar_specs();
        let memories = self
            .storage
            .read_verb
            .memory_inspect
            .load_memories_by_ids(&read_owners, &req.memory_ids, &sidecars)
            .await
            .map_err(|err| storage_error("load_memories_by_ids", &err))?;
        Ok(GetMemoriesReadResponse { memories })
    }

    /// Owner-scoped persisted one-liners. Missing/unreadable ids are absent.
    ///
    /// # Errors
    ///
    /// `Forbidden` when the context authorizes no reads; `Internal` on storage.
    pub async fn load_sketches(
        &self,
        authz: &AuthzContext,
        memory_ids: &[MemoryId],
    ) -> Result<Vec<crate::read_models::MemorySketch>, ProtocolError> {
        let read_owners = self.authorize_read(authz).await?;
        self.storage
            .read_verb
            .memory_read
            .load_sketches(&read_owners, memory_ids)
            .await
            .map_err(|err| storage_error("load_sketches", &err))
    }

    /// Owner-scoped pin carriers for the given `t`s. Missing/unreadable
    /// ids are absent.
    ///
    /// # Errors
    ///
    /// `Forbidden` when the context authorizes no reads; `Internal` on storage.
    pub async fn pin_nodes(
        &self,
        authz: &AuthzContext,
        memory_ids: &[MemoryId],
    ) -> Result<Vec<crate::PinNode>, ProtocolError> {
        let read_owners = self.authorize_read(authz).await?;
        self.storage
            .read_verb
            .memory_read
            .load_pin_nodes(&read_owners, memory_ids)
            .await
            .map_err(|err| storage_error("load_pin_nodes", &err))
    }

    /// Newest-first inbound pin page.
    ///
    /// # Errors
    ///
    /// `Forbidden` when the context authorizes no reads; `Internal` on storage.
    pub async fn inbound_pin_nodes(
        &self,
        authz: &AuthzContext,
        query: crate::storage_ports::InboundPinQuery<'_>,
    ) -> Result<Vec<crate::PinNode>, ProtocolError> {
        let read_owners = self.authorize_read(authz).await?;
        self.storage
            .read_verb
            .memory_read
            .load_inbound_pin_nodes(&read_owners, query)
            .await
            .map_err(|err| storage_error("load_inbound_pin_nodes", &err))
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
        let read_owners = self.authorize_read(authz).await?;
        let access = self.resolve_access(authz).await?;
        let mut found = self
            .storage
            .read_verb
            .memory_inspect
            .load_memories_by_ids(&read_owners, &[req.trigger_fact_id], &[])
            .await
            .map_err(|err| storage_error("load_memories_by_ids", &err))?;
        let snapshot = found
            .pop()
            .ok_or_else(|| ProtocolError::forbidden(ENTRY_NOT_FOUND_MESSAGE))?;
        if snapshot.kind != EntityKind::Fact {
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
                trigger_owner: snapshot.owner,
                trigger_fact_id: req.trigger_fact_id,
                trigger_schema_id: &snapshot.schema_id,
                trigger_schema_version: snapshot.schema_version,
                actor_tool_scope: authz.tool_scope(),
                deployment_tool_scope: &self.deployment_tool_scope,
                // One extra row proves has_more without changing the page.
                limit: req.limit.min(MAX_WAKE_CANDIDATE_LIMIT).saturating_add(1),
            })
            .await
            .map_err(|err| storage_error("list_goal_wake_candidates", &err))?;
        let page_len = req.limit.min(MAX_WAKE_CANDIDATE_LIMIT);
        let has_more = candidates.len() > page_len;
        let mut candidates = candidates;
        candidates.truncate(page_len);
        Ok(ListWakeCandidatesReadResponse {
            candidates,
            has_more,
        })
    }

    /// Wake-config read-back for goal introspection reads
    /// (`proxima://goal/{id}` / `proxima://goals`). Returns only configs
    /// whose goal owner is within the caller's read set; goals without a
    /// wake config are absent, which is data rather than an error.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context authorizes no read owners, and
    /// `Internal` when storage reads fail.
    pub async fn read_goal_wake_configs(
        &self,
        authz: &AuthzContext,
        goal_ids: &[crate::GoalId],
    ) -> Result<Vec<crate::read_models::GoalWakeConfigRow>, ProtocolError> {
        self.operation_authority(authz)?;
        if goal_ids.is_empty() {
            return Ok(Vec::new());
        }
        let read_owners = self.authorize_read(authz).await?;
        self.storage
            .read_verb
            .goal_read
            .load_goal_wake_configs(&read_owners, goal_ids)
            .await
            .map_err(|err| storage_error("load_goal_wake_configs", &err))
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
        let read_owners = self.authorize_read(authz).await?;
        let found = self
            .storage
            .read_verb
            .memory_inspect
            .load_memories_by_ids(&read_owners, &[req.fact_memory_id], &[])
            .await
            .map_err(|err| storage_error("load_memories_by_ids", &err))?;
        if found.is_empty() {
            return Err(ProtocolError::forbidden(ENTRY_NOT_FOUND_MESSAGE));
        }
        read_fact_citation_authorized(&self.storage.read_verb, &read_owners, req.fact_memory_id)
            .await
    }

    /// Read-set-scoped inverse citation read for a stateful Fact entity head.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context authorizes no read owners, and
    /// `Internal` when storage reads fail.
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
    ) -> Result<crate::verbs::query::FactCitationPage, ProtocolError> {
        let read_owners = self.authorize_read(authz).await?;
        let sidecars = self.sidecar_specs();
        facts_citing_object_authorized(&self.storage.read_verb, &read_owners, req, &sidecars).await
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

/// Admission check for caller-supplied search knobs, shared by every
/// entry into the search verb: bounds, the cursor/order pairing, and
/// the modes each knob is meaningful in. Storage re-validates the
/// cursor/order pairing as defense in depth for direct port callers.
fn validate_search_request(search: &MemorySearchRequest) -> Result<(), ProtocolError> {
    if let Some(floor) = search.min_score
        && !(floor.is_finite() && (0.0..=1.0).contains(&floor))
    {
        return Err(ProtocolError::invalid_argument(
            "min_score",
            "min_score must be within 0.0..=1.0",
        ));
    }
    if let Some(weight) = search.semantic_weight {
        if !(weight.is_finite() && (0.0..=1.0).contains(&weight)) {
            return Err(ProtocolError::invalid_argument(
                "semantic_weight",
                "semantic_weight must be within 0.0..=1.0",
            ));
        }
        // Only hybrid fusion reads the weight; lexical and semantic ranking
        // discard it. Saying so beats accepting a knob that does nothing,
        // which reads as a ranking that ignored the caller.
        if !matches!(search.mode, SearchMode::Hybrid) {
            return Err(ProtocolError::invalid_argument(
                "semantic_weight",
                "semantic_weight applies only to mode=hybrid",
            ));
        }
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

    let identities = memories
        .iter()
        .map(|row| MemoryGraphIdentity {
            memory_id: row.memory_id,
            kind: row.kind,
            schema_id: row.schema_id.clone(),
        })
        .collect::<Vec<_>>();
    let memory_ids = identities
        .iter()
        .map(|identity| identity.memory_id)
        .collect::<Vec<_>>();
    let payloads = if identities.is_empty() {
        Vec::new()
    } else {
        ports
            .memory_read
            .load_memory_graph_payloads(&identities, req.include_body)
            .await
            .map_err(|err| storage_error("load_memory_graph_payloads", &err))?
    };
    let neighbor_edges = if req.include_neighbor_edges {
        super::pin_read::neighbor_edges_from_nodes(
            &ports.memory_read,
            read_owners,
            &memory_ids,
            NEIGHBOR_EDGE_LIMIT,
        )
        .await?
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
    let mut found = ports
        .memory_inspect
        .load_memories_by_ids(read_owners, &[req.memory_id], sidecars)
        .await
        .map_err(|err| storage_error("load_memories_by_ids", &err))?;
    let Some(memory) = found.pop() else {
        return Err(ProtocolError::not_found("memory not found"));
    };
    let neighbor_edges = if req.include_neighbor_edges {
        super::pin_read::neighbor_edges_from_nodes(
            &ports.memory_read,
            read_owners,
            &[req.memory_id],
            NEIGHBOR_EDGE_LIMIT,
        )
        .await?
    } else {
        Vec::new()
    };
    Ok(GetMemoryReadResponse {
        memory: Some(memory),
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
    Ok(ListChangeEventsReadResponse { events })
}

pub(in crate::engine) async fn read_fact_citation_authorized(
    ports: &ReadVerbStoragePorts,
    read_owners: &[OwnerRef],
    fact_memory_id: MemoryId,
) -> Result<Option<FactCitationReadback>, ProtocolError> {
    ports
        .citation
        .citation_of_fact(read_owners, fact_memory_id)
        .await
        .map_err(|err| storage_error("citation_of_fact", &err))
}

pub(in crate::engine) async fn facts_citing_object_authorized(
    ports: &ReadVerbStoragePorts,
    read_owners: &[OwnerRef],
    req: &FactsCitingObjectReadRequest,
    sidecars: &[SidecarSpec],
) -> Result<crate::verbs::query::FactCitationPage, ProtocolError> {
    ports
        .citation
        .facts_citing_object(
            read_owners,
            req.cited_object_id,
            sidecars,
            req.after,
            req.limit,
        )
        .await
        .map_err(|err| storage_error("facts_citing_object", &err))
}

fn storage_error(context: &str, err: &StorageError) -> ProtocolError {
    super::errors::internal_storage_error(context, err)
}

#[cfg(test)]
mod tests {
    use crate::authz::{AuthPath, AuthzContext};
    use crate::error::ErrorCode;
    use crate::verbs::query::{
        MemorySearchRequest, SearchMode, SearchOrder, SupersessionStatus, TagMatch,
    };
    use crate::{Engine, FlavorRegistry, GroupId, MemoryId, OwnerRef, UserId};

    type ResolvedAuthz = AuthzContext;

    use super::{
        FactCitationReadRequest, FactsCitingObjectReadRequest, GetGraphReadRequest,
        GetMemoryReadRequest, ListChangeEventsReadRequest, SearchReadRequest,
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
        // Hybrid, so this isolates the bounds rule from the mode rule below.
        nan_weight.search.mode = SearchMode::Hybrid;
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

    /// Only hybrid fuses a lexical and a semantic component, so only
    /// hybrid reads the weight between them; storage discards it in the
    /// other two modes. Accepting it there returns a ranking that ignored
    /// an argument the caller set. `core_search_memories` has always
    /// rejected the pairing — the verb it delegates to, which every
    /// embedding host and flavor also enters through, did not.
    #[tokio::test]
    async fn search_rejects_a_fusion_weight_no_mode_would_use() {
        let owner = owner();
        let engine = engine();
        let authz = granted_authz(&owner);

        for mode in [SearchMode::Lexical, SearchMode::Semantic] {
            let mut req = search_req(&owner);
            req.search.mode = mode;
            req.search.semantic_weight = Some(0.5);
            let err = engine
                .search(&authz, &req)
                .await
                .expect_err("a weight outside hybrid must fail");
            assert_eq!(err.code, ErrorCode::InvalidArgument);
            assert!(
                err.message.contains("mode=hybrid"),
                "the error must name the mode that would have used it, got {:?}",
                err.message
            );
        }
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
    async fn get_memory_absent_or_invisible_is_not_found() {
        let owner = owner();
        let req = GetMemoryReadRequest {
            memory_id: MemoryId::new(uuid::Uuid::now_v7()),
            include_neighbor_edges: false,
        };
        let err = engine()
            .get_memory(&granted_authz(&owner), &req)
            .await
            .expect_err("missing inspect is not-found, not an entry Forbidden");
        assert_eq!(err.code, ErrorCode::NotFound);
        assert!(err.message.contains("memory not found"));
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
    async fn facts_citing_object_denies_denied_context() {
        let owner = owner();
        let req = FactsCitingObjectReadRequest {
            cited_object_id: uuid::Uuid::now_v7(),
            limit: 50,
            after: None,
        };
        let err = engine()
            .facts_citing_object(&AuthzContext::denied_for_owner(&owner), &req)
            .await
            .expect_err("denied context must fail");
        assert_forbidden(&err);
    }
}

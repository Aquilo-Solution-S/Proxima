use crate::SearchProjectionColumnKind;
use crate::access::AccessKind;
use crate::authz::AuthzContext;
use crate::edge::Edge;
use crate::error::ProtocolError;
use crate::read_models::{MemoryCounts, MemorySchemaSpec, MemorySnapshot};
use crate::storage::{MemoryGraphIdentity, MemoryGraphPayloadRow, StorageError};
use crate::storage_ports::ReadVerbStoragePorts;
use crate::verbs::query::{
    FactCitationReadback, MAX_RELEVANCE_SEARCH_DEPTH, MemorySearchRequest, MemorySearchResult,
    SearchCursor, SearchMode,
};
use crate::verbs::schema::{MemorySearchProjection, payload_entity_kind};
use crate::{MemoryId, OwnerId, SchemaId, SchemaVersion};

use super::Engine;
use super::pipeline::ENTRY_NOT_FOUND_MESSAGE;

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
    pub owner: OwnerId,
}

#[derive(Debug, Clone)]
pub struct GetGraphReadResponse {
    pub pending_embedding_jobs: u64,
    pub failed_embedding_jobs: u64,
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
    /// `req.search.owner` or reads nothing on it; `Internal` when
    /// storage reads fail.
    pub async fn search(
        &self,
        authz: &AuthzContext,
        req: &SearchReadRequest,
    ) -> Result<SearchReadResponse, ProtocolError> {
        validate_search_request(&req.search)?;
        let owner = self
            .authorize_owner_read(authz, &req.search.owner, AccessKind::Fact)
            .await?;
        search_authorized(
            &self.storage.read_verb,
            self.registry.search_projections(),
            &self.memory_schema_specs(),
            authz.owner_scope(),
            std::slice::from_ref(&owner),
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
        let schemas = self.memory_schema_specs();
        get_memory_authorized(
            &self.storage.read_verb,
            authz.owner_scope(),
            &read_owners,
            &schemas,
            req,
        )
        .await
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
        if read_owners.is_empty() {
            return Ok(GetMemoriesReadResponse {
                memories: Vec::new(),
            });
        }
        let schemas = self.memory_schema_specs();
        let memories = self
            .storage
            .read_verb
            .memory_inspect
            .load_memories_by_ids(authz.owner_scope(), &read_owners, &req.memory_ids, &schemas)
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
        if read_owners.is_empty() {
            return Ok(Vec::new());
        }
        self.storage
            .read_verb
            .memory_read
            .load_sketches(authz.owner_scope(), &read_owners, memory_ids)
            .await
            .map_err(|err| storage_error("load_sketches", &err))
    }

    /// How many current memories the caller can read, by kind: one per
    /// series head and one per Goal. Scoped to the same read set as
    /// [`Engine::query`]; storage narrows it per kind.
    ///
    /// # Errors
    ///
    /// `Forbidden` when the context authorizes no reads (a denied context
    /// authorizes nothing); `Internal` on storage.
    pub async fn memory_counts(&self, authz: &AuthzContext) -> Result<MemoryCounts, ProtocolError> {
        let read_owners = self.authorize_read(authz).await?;
        if read_owners.is_empty() {
            return Ok(MemoryCounts::default());
        }
        self.storage
            .read_verb
            .memory_read
            .count_memories(authz.owner_scope(), &read_owners)
            .await
            .map_err(|err| storage_error("count_memories", &err))
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
        if read_owners.is_empty() {
            return Ok(Vec::new());
        }
        self.storage
            .read_verb
            .memory_read
            .load_pin_nodes(authz.owner_scope(), &read_owners, memory_ids)
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
        if read_owners.is_empty() {
            return Ok(Vec::new());
        }
        self.storage
            .read_verb
            .memory_read
            .load_inbound_pin_nodes(authz.owner_scope(), &read_owners, query)
            .await
            .map_err(|err| storage_error("load_inbound_pin_nodes", &err))
    }

    /// Owner-scoped graph overview domain read.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context cannot access `req.owner` or
    /// is not its admin ([`crate::Role::administers`]), and `Internal` when
    /// storage reads fail.
    pub async fn get_graph(
        &self,
        authz: &AuthzContext,
        req: &GetGraphReadRequest,
    ) -> Result<GetGraphReadResponse, ProtocolError> {
        let permit = self.authorize_owner_admin(authz, &req.owner).await?;
        get_graph_authorized(&self.storage.read_verb, authz.owner_scope(), permit.owner()).await
    }

    /// One row per embedding job of `entities` for `owner`.
    ///
    /// Each row is one job. A processing job's state carries how long its
    /// claim has been held, from the database clock. Pending and failed
    /// jobs carry no claim age. This is not [`Self::embedding_coverage`],
    /// which returns counters for an operator.
    ///
    /// An empty `entities` reads nothing. More than
    /// [`crate::MAX_EMBEDDING_DIAGNOSTIC_UNITS`] memories is refused.
    ///
    /// # Errors
    ///
    /// `InvalidArgument` when `entities` is over the bound, `Forbidden`
    /// when `authz` cannot read `owner`, and `Internal` on storage faults.
    pub async fn embedding_job_diagnostics(
        &self,
        authz: &AuthzContext,
        owner: &OwnerId,
        entities: &[MemoryId],
    ) -> Result<Vec<crate::EmbeddingJobDiagnostic>, ProtocolError> {
        if let Err(crate::StorageError::ConstraintViolation(message)) =
            crate::check_diagnostic_unit_count(entities.len())
        {
            return Err(ProtocolError::invalid_argument("entities", message));
        }
        let owner = self
            .authorize_owner_read(authz, owner, AccessKind::Fact)
            .await?;
        if entities.is_empty() {
            return Ok(Vec::new());
        }
        self.storage
            .read_verb
            .embedding_job
            .embedding_job_diagnostics(authz.owner_scope(), &owner, entities)
            .await
            .map_err(|err| storage_error("embedding_job_diagnostics", &err))
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
        if read_owners.is_empty() {
            return Err(ProtocolError::forbidden(ENTRY_NOT_FOUND_MESSAGE));
        }
        let found = self
            .storage
            .read_verb
            .memory_inspect
            .load_memories_by_ids(
                authz.owner_scope(),
                &read_owners,
                &[req.fact_memory_id],
                &self.memory_schema_specs(),
            )
            .await
            .map_err(|err| storage_error("load_memories_by_ids", &err))?;
        if found.is_empty() {
            return Err(ProtocolError::forbidden(ENTRY_NOT_FOUND_MESSAGE));
        }
        read_fact_citation_authorized(
            &self.storage.read_verb,
            authz.owner_scope(),
            &read_owners,
            req.fact_memory_id,
        )
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
        if read_owners.is_empty() {
            return Ok(crate::verbs::query::FactCitationPage {
                facts: Vec::new(),
                next_cursor: None,
                has_more: false,
            });
        }
        let schemas = self.memory_schema_specs();
        facts_citing_object_authorized(
            &self.storage.read_verb,
            authz.owner_scope(),
            &read_owners,
            req,
            &schemas,
        )
        .await
    }

    /// The registry's memory-bearing schemas, as the read verbs want them.
    ///
    /// `payload_entity_kind` is both the filter and the projection: a kind
    /// with no `EntityKind` is not a memory, so the `None` it returns is the
    /// same answer a separate `filter` would have given. Selecting and
    /// converting in one step is what keeps an "impossible" arm off the
    /// `match` — there is no second place for the two lists to disagree.
    ///
    /// Each spec also carries the schema's display fields, derived once from
    /// its search declaration: the body is the first declared `Text` field
    /// that is not `title`, the tags are the declared `tag_column`. Search
    /// hits and by-id reads both display through these, so one memory reads
    /// the same wherever it is reached from.
    pub(in crate::engine) fn memory_schema_specs(&self) -> Vec<MemorySchemaSpec> {
        let projections = self.registry.search_projections();
        self.registry
            .list()
            .into_iter()
            .filter_map(|schema| {
                let kind = payload_entity_kind(schema.kind)?;
                let projection = projections.iter().find(|projection| {
                    projection.kind == schema.kind
                        && projection.schema_id == schema.schema_id
                        && projection.schema_version == schema.schema_version
                });
                let body_field = projection.and_then(|projection| {
                    projection
                        .fields
                        .iter()
                        .find(|field| {
                            field.kind == SearchProjectionColumnKind::Text
                                && field.column != "title"
                        })
                        .map(|field| field.column.clone())
                });
                let tag_field = projection.and_then(|projection| projection.tag_column.clone());
                Some(
                    MemorySchemaSpec::new(
                        kind,
                        SchemaId::new(schema.schema_id.as_str().to_string()),
                        SchemaVersion::new(schema.schema_version.into_inner()),
                        schema.sidecar_table,
                    )
                    .with_display_fields(body_field, tag_field),
                )
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
    schemas: &[MemorySchemaSpec],
    owner_scope: Option<&crate::OwnerScope>,
    read_owners: &[OwnerId],
    req: &SearchReadRequest,
) -> Result<SearchReadResponse, ProtocolError> {
    let mut effective = req.search.clone();
    effective.read_owners = read_owners.to_vec();
    let page = ports
        .memory_read
        .search_memories(owner_scope, &effective, search_projections)
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
            .load_memory_graph_payloads(owner_scope, &identities, schemas, req.include_body)
            .await
            .map_err(|err| storage_error("load_memory_graph_payloads", &err))?
    };
    let neighbor_edges = if req.include_neighbor_edges {
        super::pin_read::neighbor_edges_from_nodes(
            &ports.memory_read,
            owner_scope,
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
    owner_scope: Option<&crate::OwnerScope>,
    read_owners: &[OwnerId],
    schemas: &[MemorySchemaSpec],
    req: &GetMemoryReadRequest,
) -> Result<GetMemoryReadResponse, ProtocolError> {
    if read_owners.is_empty() {
        return Err(ProtocolError::not_found("memory not found"));
    }
    let mut found = ports
        .memory_inspect
        .load_memories_by_ids(owner_scope, read_owners, &[req.memory_id], schemas)
        .await
        .map_err(|err| storage_error("load_memories_by_ids", &err))?;
    let Some(memory) = found.pop() else {
        return Err(ProtocolError::not_found("memory not found"));
    };
    let neighbor_edges = if req.include_neighbor_edges {
        super::pin_read::neighbor_edges_from_nodes(
            &ports.memory_read,
            owner_scope,
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
    owner_scope: Option<&crate::OwnerScope>,
    owner: &OwnerId,
) -> Result<GetGraphReadResponse, ProtocolError> {
    let job_status = ports
        .embedding_job
        .count_embedding_job_status(owner_scope, owner)
        .await
        .map_err(|err| storage_error("count_embedding_job_status", &err))?;
    Ok(GetGraphReadResponse {
        pending_embedding_jobs: job_status.pending,
        failed_embedding_jobs: job_status.failed,
    })
}

pub(in crate::engine) async fn read_fact_citation_authorized(
    ports: &ReadVerbStoragePorts,
    owner_scope: Option<&crate::OwnerScope>,
    read_owners: &[OwnerId],
    fact_memory_id: MemoryId,
) -> Result<Option<FactCitationReadback>, ProtocolError> {
    ports
        .citation
        .citation_of_fact(owner_scope, read_owners, fact_memory_id)
        .await
        .map_err(|err| storage_error("citation_of_fact", &err))
}

pub(in crate::engine) async fn facts_citing_object_authorized(
    ports: &ReadVerbStoragePorts,
    owner_scope: Option<&crate::OwnerScope>,
    read_owners: &[OwnerId],
    req: &FactsCitingObjectReadRequest,
    schemas: &[MemorySchemaSpec],
) -> Result<crate::verbs::query::FactCitationPage, ProtocolError> {
    ports
        .citation
        .facts_citing_object(
            owner_scope,
            read_owners,
            req.cited_object_id,
            schemas,
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
    use crate::{Engine, FlavorRegistry, GroupId, MemoryId, OwnerId, UserId};

    type ResolvedAuthz = AuthzContext;

    use super::{
        FactCitationReadRequest, FactsCitingObjectReadRequest, GetGraphReadRequest,
        GetMemoryReadRequest, SearchReadRequest,
    };

    fn engine() -> Engine {
        Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests())
    }

    /// #479 item 3: every core memory schema displays the field its search
    /// declaration indexes as its body, and its declared `tag_column` as its
    /// tags.
    #[test]
    fn core_schemas_display_their_declared_search_fields() {
        let specs = engine().memory_schema_specs();
        let fields = |id: &str| {
            let spec = specs
                .iter()
                .find(|spec| spec.schema_id.as_str() == id)
                .unwrap_or_else(|| panic!("{id} is a registered memory schema"));
            (spec.body_field.as_deref(), spec.tag_field.as_deref())
        };
        assert_eq!(fields("core/agent-note-v1"), (Some("body"), Some("tags")));
        assert_eq!(fields("core/utterance-v1"), (Some("text"), None));
        assert_eq!(fields("core/interpretation-v1"), (Some("claim"), None));
        assert_eq!(
            fields("core/agent-derivation-v1"),
            (Some("body"), Some("tags"))
        );
    }

    fn owner() -> OwnerId {
        OwnerId::from(UserId::new(uuid::Uuid::now_v7()))
    }

    fn group_owner() -> OwnerId {
        OwnerId::from(GroupId::new(uuid::Uuid::now_v7()))
    }

    fn search_req(owner: &OwnerId) -> SearchReadRequest {
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
                semantic: None,
            },
            include_body: false,
            include_neighbor_edges: false,
        }
    }

    fn assert_forbidden(err: &crate::ProtocolError) {
        assert_eq!(err.code, ErrorCode::Forbidden);
    }

    fn granted_authz(owner: &OwnerId) -> ResolvedAuthz {
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
    async fn embedding_job_diagnostics_refuses_too_many_units_before_storage() {
        let owner = owner();
        let entities = (0..=crate::MAX_EMBEDDING_DIAGNOSTIC_UNITS)
            .map(|_| MemoryId::new(uuid::Uuid::now_v7()))
            .collect::<Vec<_>>();
        let err = engine()
            .embedding_job_diagnostics(&granted_authz(&owner), &owner, &entities)
            .await
            .expect_err("over the bound");
        assert_eq!(err.code, ErrorCode::InvalidArgument);
        assert!(err.message.contains("256"), "{}", err.message);
    }

    #[tokio::test]
    async fn embedding_job_diagnostics_of_no_units_is_empty_after_authorization() {
        let owner = owner();
        let rows = engine()
            .embedding_job_diagnostics(&granted_authz(&owner), &owner, &[])
            .await
            .expect("no units");
        assert!(rows.is_empty());
        let err = engine()
            .embedding_job_diagnostics(&AuthzContext::denied_for_owner(&owner), &owner, &[])
            .await
            .expect_err("denied");
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

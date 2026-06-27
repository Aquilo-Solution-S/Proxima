use super::{Engine, MemoryPermit};
use crate::access::Relation;
use crate::authz::AuthzContext;
use crate::error::ProtocolError;
use crate::verbs::event_history::{
    EventHistoryRequest, EventHistoryResponse, MAX_EVENT_HISTORY_LIMIT,
};
use crate::verbs::mcp_call_history::{
    MAX_MCP_CALL_HISTORY_LIMIT, McpCallHistoryRequest, McpCallHistoryResponse,
};
use crate::verbs::query::{
    EdgeExistsRequest, EdgeExistsResponse, EdgeReadRequest, EdgeReadResponse, MemoryLineageRequest,
    MemoryLineageResponse, QueryRequest, QueryResponse,
};
use crate::verbs::schema::{SchemaRequest, SchemaResponse};

impl Engine {
    /// docs/14 §"Schema" — binary-scoped, unauthenticated by
    /// default. Owner is not consulted.
    #[must_use]
    pub fn schema(&self, req: &SchemaRequest) -> SchemaResponse {
        self.registry.handle(req)
    }

    /// docs/14 §"Query" — scoped to the authorization context's read access
    /// set (`S_read`). Caller passes the transport-extracted authorization
    /// context; the engine resolves the readable owners from it and filters
    /// results to `entity_owner ∈ S_read`. A client-supplied
    /// [`QueryRequest::principal`] is NOT an access vector — it can never widen
    /// what the caller sees (writes/admin reject a foreign owner; reads simply
    /// return the caller's accessible subset).
    ///
    /// For heads-only requests targeting a stateful Fact schema (one
    /// whose `FactPayload::natural_key_columns()` is non-empty), the
    /// engine populates `QueryRequest::stateful_heads` from the
    /// registry before dispatch. Storage emits the per-NK head SQL
    /// when the field is `Some`; otherwise the existing
    /// `supersedes`-based head scan applies (A/P).
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the authorization context resolves to an empty
    /// read set (a denied context authorizes nothing), `InvalidArgument` when
    /// `req.limit == 0`, or `Internal` when the storage query fails.
    pub async fn query(
        &self,
        authz: &AuthzContext,
        req: &QueryRequest,
    ) -> Result<QueryResponse, ProtocolError> {
        let read_owners = self.authorize_read(authz).await?;
        self.query_authorized(&read_owners, req).await
    }

    async fn query_authorized(
        &self,
        read_owners: &[crate::Principal],
        req: &QueryRequest,
    ) -> Result<QueryResponse, ProtocolError> {
        if req.limit == 0 {
            return Err(ProtocolError::invalid_argument("limit", "must be > 0"));
        }
        let mut effective = req.clone();
        effective.read_owners = read_owners.to_vec();
        if effective.stateful_heads.is_empty() {
            effective.stateful_heads = match effective.schema_id.as_ref() {
                Some(sid) => self.registry.stateful_filters_for_schema(sid),
                None => self.registry.stateful_filters(),
            };
        }
        self.storage
            .query_memories(&effective, self.registry.list().as_slice())
            .await
            .map_err(|e| ProtocolError::internal(e.to_string()))
    }

    /// Edge read scoped to the context's read access set (`S_read`). Same auth
    /// shape as `Query`; callers can hydrate by edge id or by
    /// relation/source/target filter. Edges are source-owned: an edge is
    /// visible iff its source is readable; an unreadable target is stubbed (its
    /// id is retained with `target_readable = false`, never dereferenced), and
    /// a World-readable source with an unreadable target is omitted entirely.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the authorization context resolves to an empty
    /// read set, `InvalidArgument` when `req.limit == 0`, or `Internal` when
    /// storage fails.
    pub async fn read_edges(
        &self,
        authz: &AuthzContext,
        req: &EdgeReadRequest,
    ) -> Result<EdgeReadResponse, ProtocolError> {
        let read_owners = self.authorize_read(authz).await?;
        self.read_edges_authorized(&read_owners, req).await
    }

    async fn read_edges_authorized(
        &self,
        read_owners: &[crate::Principal],
        req: &EdgeReadRequest,
    ) -> Result<EdgeReadResponse, ProtocolError> {
        if req.limit == 0 {
            return Err(ProtocolError::invalid_argument("limit", "must be > 0"));
        }
        self.storage
            .read_edges(read_owners, req)
            .await
            .map_err(|e| ProtocolError::internal(e.to_string()))
    }

    /// Edge existence probe scoped to the context's read set (`S_read`), same
    /// source-owned visibility as `read_edges`: existence is disclosed only for
    /// edges whose source is readable (a client `req.principal` is not an access
    /// vector).
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the authorization context resolves to an empty
    /// read set, or `Internal` when storage fails.
    pub async fn edge_exists(
        &self,
        authz: &AuthzContext,
        req: &EdgeExistsRequest,
    ) -> Result<EdgeExistsResponse, ProtocolError> {
        let read_owners = self.authorize_read(authz).await?;
        self.edge_exists_authorized(&read_owners, req).await
    }

    async fn edge_exists_authorized(
        &self,
        read_owners: &[crate::Principal],
        req: &EdgeExistsRequest,
    ) -> Result<EdgeExistsResponse, ProtocolError> {
        self.storage
            .edge_exists(read_owners, req)
            .await
            .map_err(|e| ProtocolError::internal(e.to_string()))
    }

    /// Owner-scoped Provenance/Supersession lineage walk from one memory.
    /// Same auth shape as `Query`.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context cannot access `req.principal` or
    /// lacks [`Relation::Viewer`], or `Internal` when storage fails.
    pub async fn walk_memory_lineage(
        &self,
        authz: &AuthzContext,
        req: &MemoryLineageRequest,
    ) -> Result<MemoryLineageResponse, ProtocolError> {
        let read_owners = self.authorize_read(authz).await?;
        self.walk_memory_lineage_authorized(&read_owners, req).await
    }

    async fn walk_memory_lineage_authorized(
        &self,
        read_owners: &[crate::Principal],
        req: &MemoryLineageRequest,
    ) -> Result<MemoryLineageResponse, ProtocolError> {
        self.storage
            .walk_memory_lineage(read_owners, req)
            .await
            .map_err(|e| ProtocolError::internal(e.to_string()))
    }

    /// docs/14 §"`EventHistory`" — bounded change-event read for ONE owner the
    /// caller selects via `req.principal` and the context can access (gated by
    /// `authorize_request`, unlike `Query`/`read_edges` which span `S_read`).
    /// Multi-owner event polling across `S_read` is the deferred spec-Q4 path
    /// (`list_events`); this verb stays single-owner. Server clamps `limit` to
    /// `MAX_EVENT_HISTORY_LIMIT`.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context cannot access `req.principal` or
    /// lacks [`Relation::Viewer`], `InvalidArgument` when `req.limit == 0`, or
    /// `Internal` when the storage read fails.
    pub async fn event_history(
        &self,
        authz: &AuthzContext,
        req: &EventHistoryRequest,
    ) -> Result<EventHistoryResponse, ProtocolError> {
        let permit = self
            .authorize_request(authz, &req.principal, Relation::Viewer)
            .await?;
        self.event_history_authorized(&permit, req).await
    }

    async fn event_history_authorized(
        &self,
        permit: &MemoryPermit,
        req: &EventHistoryRequest,
    ) -> Result<EventHistoryResponse, ProtocolError> {
        if req.limit == 0 {
            return Err(ProtocolError::invalid_argument("limit", "must be > 0"));
        }
        let mut effective = req.clone();
        effective.principal = permit.owner().clone();
        if effective.limit > MAX_EVENT_HISTORY_LIMIT {
            effective.limit = MAX_EVENT_HISTORY_LIMIT;
        }
        self.storage
            .event_history(&effective)
            .await
            .map_err(|e| ProtocolError::internal(e.to_string()))
    }

    /// docs/14 §protocol surface — bounded MCP-call activity read for ONE owner
    /// the caller selects via `req.principal` and the context can access (gated
    /// by `authorize_request`; single-owner, not `S_read`-spanning like
    /// `Query`). Server clamps `limit` to `MAX_MCP_CALL_HISTORY_LIMIT`.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context cannot access `req.principal` or
    /// lacks [`Relation::Viewer`], `InvalidArgument` when `req.limit == 0`, or
    /// `Internal` when the storage read fails.
    pub async fn read_mcp_call_history(
        &self,
        authz: &AuthzContext,
        req: &McpCallHistoryRequest,
    ) -> Result<McpCallHistoryResponse, ProtocolError> {
        let permit = self
            .authorize_request(authz, &req.principal, Relation::Viewer)
            .await?;
        self.read_mcp_call_history_authorized(&permit, req).await
    }

    async fn read_mcp_call_history_authorized(
        &self,
        permit: &MemoryPermit,
        req: &McpCallHistoryRequest,
    ) -> Result<McpCallHistoryResponse, ProtocolError> {
        if req.limit == 0 {
            return Err(ProtocolError::invalid_argument("limit", "must be > 0"));
        }
        let mut effective = req.clone();
        effective.principal = permit.owner().clone();
        if effective.limit > MAX_MCP_CALL_HISTORY_LIMIT {
            effective.limit = MAX_MCP_CALL_HISTORY_LIMIT;
        }
        self.storage
            .read_mcp_call_history(&effective)
            .await
            .map_err(|e| ProtocolError::internal(e.to_string()))
    }
}

use super::{Engine, MemoryPermit};
use crate::OwnerRef;
use crate::access::Relation;
use crate::authz::AuthzContext;
use crate::error::ProtocolError;
use crate::storage_ports::QueryStoragePorts;
use crate::verbs::change_history::{
    ChangeHistoryRequest, ChangeHistoryResponse, MAX_CHANGE_HISTORY_LIMIT,
};
use crate::verbs::mcp_call_history::{
    MAX_MCP_CALL_HISTORY_LIMIT, McpCallHistoryRequest, McpCallHistoryResponse,
};
use crate::verbs::query::{
    EdgeExistsRequest, EdgeExistsResponse, EdgeReadRequest, EdgeReadResponse, MemoryLineageRequest,
    MemoryLineageResponse, QueryCursor, QueryRequest, QueryResponse, SidecarAtom,
};
use crate::verbs::schema::{FlavorRegistryFrozen, SchemaRequest, SchemaResponse};
use crate::{Owner, SchemaId};

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
    /// results to `row owner ∈ S_read`. A client-supplied
    /// [`QueryRequest::owner`] is NOT an access vector — it can never widen
    /// what the caller sees (writes/admin reject a foreign owner; reads simply
    /// return the caller's accessible subset).
    ///
    /// Heads-only requests need no per-schema natural-key filter: a head
    /// is the latest `t` on a `handle`, and `FactPayload`
    /// natural-key columns already decide which `handle` an ingest lands
    /// on (docs/03 §Stateful Fact schemas).
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
        query_authorized(&self.storage.query, &self.registry, &read_owners, req).await
    }

    /// Edge read scoped to the context's read access set (`S_read`). Same
    /// auth shape as `Query`; callers narrow by kind and/or endpoint.
    /// Edges are source-owned: an edge is visible iff its source is
    /// readable; an unreadable target is rendered as
    /// `EdgeTargetProjection::Redacted` without id/kind leakage.
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
        read_edges_authorized(&self.storage.query, &read_owners, req).await
    }

    /// Edge existence probe scoped to the context's read set (`S_read`), same
    /// source-owned visibility as `read_edges`: existence is disclosed only for
    /// edges whose source is readable (a client `req.owner` is not an access
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
        edge_exists_authorized(&self.storage.query, &read_owners, req).await
    }

    /// Owner-scoped Provenance/Supersession lineage walk from one memory.
    /// Same auth shape as `Query`.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context cannot access `req.owner` or
    /// lacks [`Relation::Viewer`], or `Internal` when storage fails.
    pub async fn walk_memory_lineage(
        &self,
        authz: &AuthzContext,
        req: &MemoryLineageRequest,
    ) -> Result<MemoryLineageResponse, ProtocolError> {
        let read_owners = self.authorize_read(authz).await?;
        walk_memory_lineage_authorized(&self.storage.query, &read_owners, req).await
    }

    /// docs/14 §"`ChangeHistory`" — bounded change-event read scoped to the
    /// authorization context's read access set (`S_read`), matching `Query` and
    /// `read_edges`. A client-supplied [`ChangeHistoryRequest::owner`] is not
    /// an access vector and cannot widen what the caller sees. Server clamps
    /// `limit` to `MAX_CHANGE_HISTORY_LIMIT`.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the authorization context resolves to an empty
    /// read set, `InvalidArgument` when `req.limit == 0`, or `Internal` when the
    /// storage read fails.
    pub async fn change_history(
        &self,
        authz: &AuthzContext,
        req: &ChangeHistoryRequest,
    ) -> Result<ChangeHistoryResponse, ProtocolError> {
        let read_owners = self.authorize_read(authz).await?;
        change_history_authorized(&self.storage.query, &read_owners, req).await
    }

    /// docs/14 §protocol surface — bounded MCP-call activity read for ONE owner
    /// the caller selects via `req.owner` and the context can access (gated
    /// by `authorize_request`; single-owner, not `S_read`-spanning like
    /// `Query`). Server clamps `limit` to `MAX_MCP_CALL_HISTORY_LIMIT`.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context cannot access `req.owner` or
    /// lacks [`Relation::Viewer`], `InvalidArgument` when `req.limit == 0`, or
    /// `Internal` when the storage read fails.
    pub async fn read_mcp_call_history(
        &self,
        authz: &AuthzContext,
        req: &McpCallHistoryRequest,
    ) -> Result<McpCallHistoryResponse, ProtocolError> {
        let permit = self
            .authorize_request(authz, &req.owner, Relation::Viewer)
            .await?;
        read_mcp_call_history_authorized(&self.storage.query, &permit, req).await
    }

    /// Current owned series handle whose sidecar matches `columns`.
    ///
    /// Owner-only. A World-transferred series is a miss for the prior
    /// owner. Flavor code must not JOIN `memory_head` to answer this.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context cannot view `owner`,
    /// `InvalidArgument` when the column list is empty or an identifier
    /// is invalid, and `Internal` on storage failure.
    pub async fn owned_series_handle(
        &self,
        authz: &AuthzContext,
        owner: Owner,
        schema_id: &SchemaId,
        sidecar_table: &str,
        columns: &[(&str, SidecarAtom)],
    ) -> Result<Option<uuid::Uuid>, ProtocolError> {
        let _permit = self
            .authorize_request(authz, &owner, Relation::Viewer)
            .await?;
        self.storage
            .query
            .memory_read
            .owned_series_handle(owner, schema_id, sidecar_table, columns)
            .await
            .map_err(|err| match err {
                crate::StorageError::ConstraintViolation(message) => {
                    crate::error::ProtocolError::invalid_argument("columns", message)
                }
                other => super::errors::internal_storage_error("owned_series_handle", &other),
            })
    }
}

pub(in crate::engine) async fn query_authorized(
    ports: &QueryStoragePorts,
    registry: &FlavorRegistryFrozen,
    read_owners: &[OwnerRef],
    req: &QueryRequest,
) -> Result<QueryResponse, ProtocolError> {
    if req.limit == 0 {
        return Err(ProtocolError::invalid_argument("limit", "must be > 0"));
    }
    validate_query_cursor(req)?;
    let mut effective = req.clone();
    effective.read_owners = read_owners.to_vec();
    ports
        .memory_read
        .query_memories(&effective, registry.list().as_slice())
        .await
        .map_err(|e| ProtocolError::internal(e.to_string()))
}

fn validate_query_cursor(req: &QueryRequest) -> Result<(), ProtocolError> {
    let Some(cursor) = &req.page.after else {
        return Ok(());
    };

    match req.entity_kind {
        None => Err(ProtocolError::invalid_argument(
            "page.after",
            "cursor requires a single entity_kind",
        )),
        Some(crate::EntityKind::Goal) if matches!(cursor, QueryCursor::Goal { .. }) => Ok(()),
        Some(
            crate::EntityKind::Fact
            | crate::EntityKind::Abstraction
            | crate::EntityKind::Perspective,
        ) if matches!(cursor, QueryCursor::Memory { .. }) => Ok(()),
        Some(_) => Err(ProtocolError::invalid_argument(
            "page.after",
            "cursor kind does not match entity_kind",
        )),
    }
}

pub(in crate::engine) async fn read_edges_authorized(
    ports: &QueryStoragePorts,
    read_owners: &[OwnerRef],
    req: &EdgeReadRequest,
) -> Result<EdgeReadResponse, ProtocolError> {
    if req.limit == 0 {
        return Err(ProtocolError::invalid_argument("limit", "must be > 0"));
    }
    super::pin_read::read_edges_from_nodes(&ports.memory_read, read_owners, req).await
}

pub(in crate::engine) async fn edge_exists_authorized(
    ports: &QueryStoragePorts,
    read_owners: &[OwnerRef],
    req: &EdgeExistsRequest,
) -> Result<EdgeExistsResponse, ProtocolError> {
    super::pin_read::edge_exists_from_nodes(&ports.memory_read, read_owners, req).await
}

pub(in crate::engine) async fn walk_memory_lineage_authorized(
    ports: &QueryStoragePorts,
    read_owners: &[OwnerRef],
    req: &MemoryLineageRequest,
) -> Result<MemoryLineageResponse, ProtocolError> {
    ports
        .memory_read
        .walk_memory_lineage(read_owners, req)
        .await
        .map_err(|e| ProtocolError::internal(e.to_string()))
}

pub(in crate::engine) async fn change_history_authorized(
    ports: &QueryStoragePorts,
    read_owners: &[OwnerRef],
    req: &ChangeHistoryRequest,
) -> Result<ChangeHistoryResponse, ProtocolError> {
    if req.limit == 0 {
        return Err(ProtocolError::invalid_argument("limit", "must be > 0"));
    }
    let mut effective = req.clone();
    if effective.limit > MAX_CHANGE_HISTORY_LIMIT {
        effective.limit = MAX_CHANGE_HISTORY_LIMIT;
    }
    ports
        .change_event
        .change_history(read_owners, &effective)
        .await
        .map_err(|e| ProtocolError::internal(e.to_string()))
}

pub(in crate::engine) async fn read_mcp_call_history_authorized(
    ports: &QueryStoragePorts,
    permit: &MemoryPermit,
    req: &McpCallHistoryRequest,
) -> Result<McpCallHistoryResponse, ProtocolError> {
    if req.limit == 0 {
        return Err(ProtocolError::invalid_argument("limit", "must be > 0"));
    }
    let mut effective = req.clone();
    effective.owner = *permit.owner();
    if effective.limit > MAX_MCP_CALL_HISTORY_LIMIT {
        effective.limit = MAX_MCP_CALL_HISTORY_LIMIT;
    }
    ports
        .mcp_call_read
        .read_mcp_call_history(&effective)
        .await
        .map_err(|e| ProtocolError::internal(e.to_string()))
}

use super::Engine;
use crate::authz::{AuthzContext, MemoryAction, Role};
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

    /// docs/14 §"Query" — Owner-scoped. Caller passes the
    /// transport-extracted authorization context; engine gates owner
    /// access and graph-read capability.
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
    /// Returns `Forbidden` when the context cannot access `req.principal` or
    /// lacks the graph-read role, `InvalidArgument` when `req.limit == 0`, or
    /// `Internal` when the storage query fails.
    pub async fn query(
        &self,
        authz: &AuthzContext,
        req: &QueryRequest,
    ) -> Result<QueryResponse, ProtocolError> {
        super::authorize_action(authz, &req.principal, Role::GraphRead, MemoryAction::Read)?;
        if req.limit == 0 {
            return Err(ProtocolError::invalid_argument("limit", "must be > 0"));
        }
        let mut effective = req.clone();
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

    /// Owner-scoped edge read. Same auth shape as `Query`; callers can
    /// hydrate by edge id or by relation/source/target filter.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context cannot access `req.principal` or
    /// lacks graph-read, `InvalidArgument` when `req.limit == 0`, or
    /// `Internal` when storage fails.
    pub async fn read_edges(
        &self,
        authz: &AuthzContext,
        req: &EdgeReadRequest,
    ) -> Result<EdgeReadResponse, ProtocolError> {
        super::authorize_action(authz, &req.principal, Role::GraphRead, MemoryAction::Read)?;
        if req.limit == 0 {
            return Err(ProtocolError::invalid_argument("limit", "must be > 0"));
        }
        self.storage
            .read_edges(req)
            .await
            .map_err(|e| ProtocolError::internal(e.to_string()))
    }

    /// Owner-scoped edge existence probe. Same auth shape as `Query`.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context cannot access `req.principal` or
    /// lacks graph-read, or `Internal` when storage fails.
    pub async fn edge_exists(
        &self,
        authz: &AuthzContext,
        req: &EdgeExistsRequest,
    ) -> Result<EdgeExistsResponse, ProtocolError> {
        super::authorize_action(authz, &req.principal, Role::GraphRead, MemoryAction::Read)?;
        self.storage
            .edge_exists(req)
            .await
            .map_err(|e| ProtocolError::internal(e.to_string()))
    }

    /// Owner-scoped Provenance/Supersession lineage walk from one memory.
    /// Same auth shape as `Query`.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context cannot access `req.principal` or
    /// lacks graph-read, or `Internal` when storage fails.
    pub async fn walk_memory_lineage(
        &self,
        authz: &AuthzContext,
        req: &MemoryLineageRequest,
    ) -> Result<MemoryLineageResponse, ProtocolError> {
        super::authorize_action(authz, &req.principal, Role::GraphRead, MemoryAction::Read)?;
        self.storage
            .walk_memory_lineage(req)
            .await
            .map_err(|e| ProtocolError::internal(e.to_string()))
    }

    /// docs/14 §"`EventHistory`" — Owner-scoped bounded change-event
    /// read. Same auth shape as `Query` / `Subscribe`. Server clamps
    /// `limit` to `MAX_EVENT_HISTORY_LIMIT`.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context cannot access `req.principal` or
    /// lacks the graph-read role, `InvalidArgument` when `req.limit == 0`, or
    /// `Internal` when the storage read fails.
    pub async fn event_history(
        &self,
        authz: &AuthzContext,
        req: &EventHistoryRequest,
    ) -> Result<EventHistoryResponse, ProtocolError> {
        super::authorize_action(authz, &req.principal, Role::GraphRead, MemoryAction::Read)?;
        if req.limit == 0 {
            return Err(ProtocolError::invalid_argument("limit", "must be > 0"));
        }
        let mut effective = req.clone();
        if effective.limit > MAX_EVENT_HISTORY_LIMIT {
            effective.limit = MAX_EVENT_HISTORY_LIMIT;
        }
        self.storage
            .event_history(&effective)
            .await
            .map_err(|e| ProtocolError::internal(e.to_string()))
    }

    /// docs/14 §protocol surface — Owner-scoped bounded MCP-call
    /// activity read. Same auth shape as `Query`; server clamps
    /// `limit` to `MAX_MCP_CALL_HISTORY_LIMIT`.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context cannot access `req.principal` or
    /// lacks the graph-read role, `InvalidArgument` when `req.limit == 0`, or
    /// `Internal` when the storage read fails.
    pub async fn read_mcp_call_history(
        &self,
        authz: &AuthzContext,
        req: &McpCallHistoryRequest,
    ) -> Result<McpCallHistoryResponse, ProtocolError> {
        super::authorize_action(authz, &req.principal, Role::GraphRead, MemoryAction::Read)?;
        if req.limit == 0 {
            return Err(ProtocolError::invalid_argument("limit", "must be > 0"));
        }
        let mut effective = req.clone();
        if effective.limit > MAX_MCP_CALL_HISTORY_LIMIT {
            effective.limit = MAX_MCP_CALL_HISTORY_LIMIT;
        }
        self.storage
            .read_mcp_call_history(&effective)
            .await
            .map_err(|e| ProtocolError::internal(e.to_string()))
    }
}

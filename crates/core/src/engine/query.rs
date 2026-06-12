use super::Engine;
use crate::authz::{AuthzContext, Role};
use crate::error::ProtocolError;
use crate::verbs::event_history::{
    EventHistoryRequest, EventHistoryResponse, MAX_EVENT_HISTORY_LIMIT,
};
use crate::verbs::query::{QueryRequest, QueryResponse};
use crate::verbs::schema::{SchemaRequest, SchemaResponse};
use crate::verbs::subscribe::{ChangeEventStream, SubscribeRequest};

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
    /// Returns `Forbidden` when the context cannot access `req.owner` or
    /// lacks the graph-read role, or `Internal` when the storage query fails.
    pub async fn query(
        &self,
        authz: &AuthzContext,
        req: &QueryRequest,
    ) -> Result<QueryResponse, ProtocolError> {
        super::authorize(authz, &req.owner, Role::GraphRead)?;
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

    /// docs/14 §"Subscribe" — Owner-scoped stream with optional
    /// `since` cursor for resume.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context cannot access `req.owner` or
    /// lacks the graph-read role, or `Internal` when storage fails to open
    /// the change stream.
    pub async fn subscribe(
        &self,
        authz: &AuthzContext,
        req: SubscribeRequest,
    ) -> Result<ChangeEventStream, ProtocolError> {
        super::authorize(authz, &req.owner, Role::GraphRead)?;
        self.storage
            .subscribe_changes(&req.owner, req.since)
            .await
            .map_err(|e| ProtocolError::internal(e.to_string()))
    }

    /// docs/14 §"`EventHistory`" — Owner-scoped bounded change-event
    /// read. Same auth shape as `Query` / `Subscribe`. Server clamps
    /// `limit` to `MAX_EVENT_HISTORY_LIMIT`.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context cannot access `req.owner` or
    /// lacks the graph-read role, or `Internal` when `req.limit == 0` or
    /// the storage read fails.
    pub async fn event_history(
        &self,
        authz: &AuthzContext,
        req: &EventHistoryRequest,
    ) -> Result<EventHistoryResponse, ProtocolError> {
        super::authorize(authz, &req.owner, Role::GraphRead)?;
        if req.limit == 0 {
            return Err(ProtocolError::internal("EventHistory.limit must be > 0"));
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
}

use super::Engine;
use crate::auth::Credentials;
use crate::error::ProtocolError;
use crate::verbs::query::{QueryRequest, QueryResponse, SupersessionStatus};
use crate::verbs::schema::{SchemaRequest, SchemaResponse};
use crate::verbs::subscribe::{ChangeEventStream, SubscribeRequest};

impl Engine {
    /// docs/14 §"Schema" — binary-scoped, unauthenticated by
    /// default. Owner is not consulted.
    pub fn schema(&self, req: &SchemaRequest) -> SchemaResponse {
        self.registry.handle(req)
    }

    /// docs/14 §"Query" — Owner-scoped. Caller passes the
    /// transport-extracted credentials; engine resolves and
    /// gates `req.owner.principal ∈ resolved.accessible_principals`.
    ///
    /// For heads-only requests targeting a stateful Fact schema (one
    /// whose `FactPayload::natural_key_columns()` is non-empty), the
    /// engine populates `QueryRequest::stateful_heads` from the
    /// registry before dispatch. Storage emits the per-NK head SQL
    /// when the field is `Some`; otherwise the existing
    /// `supersedes`-based head scan applies (A/P).
    pub async fn query(
        &self,
        creds: &Credentials,
        req: &QueryRequest,
    ) -> Result<QueryResponse, ProtocolError> {
        let resolved = self
            .auth
            .resolve(creds)
            .map_err(|_| ProtocolError::auth_required())?;
        if !resolved.can_access_owner(&req.owner) {
            return Err(ProtocolError::forbidden(
                "principal cannot access requested owner",
            ));
        }
        let mut effective = req.clone();
        if matches!(effective.supersession, SupersessionStatus::HeadsOnly)
            && effective.stateful_heads.is_none()
            && let Some(sid) = effective.schema_id.as_ref()
        {
            effective.stateful_heads = self.registry.stateful_filter_for(sid);
        }
        self.storage
            .query_memories(&effective, self.registry.list().as_slice())
            .await
            .map_err(|e| ProtocolError::internal(e.to_string()))
    }

    /// docs/14 §"Subscribe" — Owner-scoped stream with optional
    /// `since` cursor for resume.
    pub async fn subscribe(
        &self,
        creds: &Credentials,
        req: SubscribeRequest,
    ) -> Result<ChangeEventStream, ProtocolError> {
        let resolved = self
            .auth
            .resolve(creds)
            .map_err(|_| ProtocolError::auth_required())?;
        if !resolved.can_access_owner(&req.owner) {
            return Err(ProtocolError::forbidden(
                "principal cannot access requested owner",
            ));
        }
        self.storage
            .subscribe_changes(&req.owner, req.since)
            .await
            .map_err(|e| ProtocolError::internal(e.to_string()))
    }
}

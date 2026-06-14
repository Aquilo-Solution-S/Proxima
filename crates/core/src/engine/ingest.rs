use super::Engine;
use crate::SchemaVersion;
use crate::authz::{AuthzContext, Role};
use crate::error::ProtocolError;
use crate::storage::StorageError;
use crate::verbs::close_batch::CloseBatchOutcome;
use crate::verbs::event_ingest::{AuthorizedEventIngest, EventDraft, EventIngestOutcome};
use crate::verbs::persist_mcp_call::{McpCallLogInput, McpCallLogOutcome};
use crate::{Principal, SourceBatchId};

impl Engine {
    /// docs/14 §"`EventIngest`" — Owner-scoped write. Validates
    /// schemas and delegates to storage.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context cannot access `draft.principal` or
    /// lacks the source-ingest role, `UnknownSchema` when the Fact schema or
    /// provided citation schemas are not registered, or `Internal` when the
    /// atomic ingest fails.
    pub async fn event_ingest(
        &self,
        authz: &AuthzContext,
        draft: EventDraft,
    ) -> Result<EventIngestOutcome, ProtocolError> {
        let authorized = self.authorize_event_ingest(authz, Role::SourceIngest, draft)?;
        self.storage
            .ingest_event_atomic(authorized.draft())
            .await
            .map_err(|e| ProtocolError::internal(e.to_string()))
    }

    /// Authorize + schema-validate + owner-stamp an event ingest,
    /// returning a witness required by the sidecar-ingest primitive.
    /// Does NOT write. `role` is the role the caller's operation
    /// requires.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context cannot access `draft.principal`
    /// or lacks `role`, or `UnknownSchema` when the Fact schema or provided
    /// citation schemas are not registered.
    pub fn authorize_event_ingest(
        &self,
        authz: &AuthzContext,
        role: Role,
        mut draft: EventDraft,
    ) -> Result<AuthorizedEventIngest, ProtocolError> {
        super::authorize(authz, &draft.principal, role)?;
        let owner = authz.scoped_owner(draft.principal.clone());
        draft.stamp_owner(owner);
        self.ensure_event_ingest_schema(&draft.schema_id, draft.schema_version)?;
        if let Some(citation) = &draft.citation {
            self.ensure_event_ingest_schema(
                &citation.object.schema_id,
                citation.object.schema_version,
            )?;
            self.ensure_event_ingest_schema(
                &citation.mapping.schema_id,
                citation.mapping.schema_version,
            )?;
        }
        Ok(AuthorizedEventIngest::new(draft))
    }

    fn ensure_event_ingest_schema(
        &self,
        schema_id: &crate::SchemaId,
        schema_version: SchemaVersion,
    ) -> Result<(), ProtocolError> {
        if self.registry.lookup(schema_id, schema_version).is_none() {
            return Err(ProtocolError::unknown_schema(
                schema_id.as_str(),
                schema_version.into_inner(),
            ));
        }
        Ok(())
    }

    /// Owner-scoped write of a host-observed MCP activity log.
    ///
    /// Authorizes the caller against the log's Owner and derives the
    /// stamped org from the authenticated identity — never trusting a
    /// caller-supplied org — mirroring [`Self::event_ingest`]. The
    /// per-user actor (`actor_oid` / `actor_upn`) is recorded as Fact
    /// data; the graph Owner is what gets authorized here.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when `authz` cannot access the log Owner or
    /// lacks the source-ingest role, or `Internal` when the atomic write
    /// fails.
    pub async fn persist_mcp_call(
        &self,
        authz: &AuthzContext,
        mut input: McpCallLogInput,
    ) -> Result<McpCallLogOutcome, ProtocolError> {
        let owner = authz.scoped_owner(input.owner.principal.clone());
        super::authorize(authz, &owner.principal, Role::SourceIngest)?;
        input.owner = owner;
        self.storage
            .persist_mcp_call_atomic(&input)
            .await
            .map_err(|e| ProtocolError::internal(e.to_string()))
    }

    /// docs/01 §"The contract" — Owner-scoped, idempotent batch close.
    /// Sources call this after a successful poll once they consider the
    /// batch complete. F→A consolidation (M5+) gates on
    /// `closed_at IS NOT NULL`.
    ///
    /// # Errors
    ///
    /// Returns `NotFound` when the batch doesn't exist or belongs to a
    /// different owner; `Forbidden` when the context cannot access `owner`
    /// or lacks the source-ingest role.
    pub async fn close_batch(
        &self,
        authz: &AuthzContext,
        principal: Principal,
        source_batch_id: SourceBatchId,
    ) -> Result<CloseBatchOutcome, ProtocolError> {
        super::authorize(authz, &principal, Role::SourceIngest)?;
        let outcome = self
            .storage
            .close_batch(&principal, source_batch_id)
            .await
            .map_err(|e| match e {
                StorageError::NotFound => ProtocolError::not_found("source batch not found"),
                other => ProtocolError::internal(other.to_string()),
            })?;

        Ok(outcome)
    }
}

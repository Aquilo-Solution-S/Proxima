use super::Engine;
use crate::SchemaVersion;
use crate::authz::{AuthzContext, Role};
use crate::error::ProtocolError;
use crate::storage::StorageError;
use crate::verbs::close_batch::CloseBatchOutcome;
use crate::verbs::event_ingest::{AuthorizedEventIngest, EventDraft, EventIngestOutcome};
use crate::verbs::persist_wake_trace::{WakeTracePersistInput, WakeTracePersistOutcome};
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

    /// Atomic wake-trace persistence. The storage layer writes the
    /// Fact, JSONL citation artifact, sidecars, and provenance edges
    /// in one transaction.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context cannot access `input.owner` or
    /// lacks the graph-write role, or `Internal` when the atomic persist
    /// fails.
    pub async fn persist_wake_trace(
        &self,
        authz: &AuthzContext,
        input: WakeTracePersistInput,
    ) -> Result<WakeTracePersistOutcome, ProtocolError> {
        super::authorize(authz, &input.owner.principal, Role::GraphWrite)?;
        self.persist_wake_trace_internal(input)
            .await
            .map_err(|e| ProtocolError::internal(e.to_string()))
    }

    /// Internal wake path. Callers have already resolved wake-token
    /// authorization.
    pub(crate) async fn persist_wake_trace_internal(
        &self,
        input: WakeTracePersistInput,
    ) -> Result<WakeTracePersistOutcome, StorageError> {
        self.storage
            .persist_wake_trace_atomic(&self.registry, &input)
            .await
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

        // Wake personalities after a new batch closes. Dispatcher
        // cursors and invocation rows provide idempotency.
        if !outcome.already_closed {
            let _ = self.run_dispatcher_tick().await?;
        }
        Ok(outcome)
    }
}

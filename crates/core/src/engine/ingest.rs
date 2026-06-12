use super::Engine;
use crate::SourceBatchId;
use crate::authz::{AuthzContext, Role};
use crate::error::ProtocolError;
use crate::storage::StorageError;
use crate::verbs::close_batch::CloseBatchOutcome;
use crate::verbs::event_ingest::{EventDraft, EventIngestOutcome};
use crate::verbs::persist_wake_trace::{WakeTracePersistInput, WakeTracePersistOutcome};

impl Engine {
    /// docs/14 §"`EventIngest`" — Owner-scoped write. Validates
    /// schemas and delegates to storage.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context cannot access `draft.owner` or
    /// lacks the source-ingest role, `UnknownSchema` when any of the three
    /// draft schemas isn't registered, or `Internal` when the atomic ingest
    /// fails.
    pub async fn event_ingest(
        &self,
        authz: &AuthzContext,
        draft: EventDraft,
    ) -> Result<EventIngestOutcome, ProtocolError> {
        super::authorize(authz, &draft.owner, Role::SourceIngest)?;
        // Three schema validations: fact, cited_object, citation_mapping.
        for (sid, ver) in [
            (&draft.schema_id, draft.schema_version),
            (
                &draft.cited_object.schema_id,
                draft.cited_object.schema_version,
            ),
            (
                &draft.citation_mapping.schema_id,
                draft.citation_mapping.schema_version,
            ),
        ] {
            if self.registry.lookup(sid, ver).is_none() {
                return Err(ProtocolError::unknown_schema(
                    sid.as_str(),
                    ver.into_inner(),
                ));
            }
        }
        self.storage
            .ingest_event_atomic(&draft)
            .await
            .map_err(|e| ProtocolError::internal(e.to_string()))
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
        super::authorize(authz, &input.owner, Role::GraphWrite)?;
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
        owner: crate::Owner,
        source_batch_id: SourceBatchId,
    ) -> Result<CloseBatchOutcome, ProtocolError> {
        super::authorize(authz, &owner, Role::SourceIngest)?;
        let outcome = self
            .storage
            .close_batch(&owner, source_batch_id)
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

use super::Engine;
use crate::SourceBatchId;
use crate::auth::Credentials;
use crate::error::ProtocolError;
use crate::storage::StorageError;
use crate::verbs::close_batch::CloseBatchOutcome;
use crate::verbs::event_ingest::{EventDraft, EventIngestOutcome};

impl Engine {
    /// docs/14 §"EventIngest" — Owner-scoped write. Validates
    /// schemas and delegates to storage.
    pub async fn event_ingest(
        &self,
        creds: &Credentials,
        draft: EventDraft,
    ) -> Result<EventIngestOutcome, ProtocolError> {
        let resolved = self
            .auth
            .resolve(creds)
            .map_err(|_| ProtocolError::auth_required())?;
        if !resolved.can_access_owner(&draft.owner) {
            return Err(ProtocolError::forbidden(
                "principal cannot access requested owner",
            ));
        }
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

    /// docs/01 §"The contract" — Owner-scoped, idempotent batch close.
    /// Sources call this after a successful poll once they consider the
    /// batch complete. F→A consolidation (M5+) gates on
    /// `closed_at IS NOT NULL`.
    ///
    /// # Errors
    ///
    /// Returns `NotFound` when the batch doesn't exist or belongs to a
    /// different owner; `Forbidden` when the principal cannot access
    /// `owner`; `AuthRequired` on resolver failure.
    pub async fn close_batch(
        &self,
        creds: &Credentials,
        owner: crate::Owner,
        _source_batch_id: SourceBatchId,
    ) -> Result<CloseBatchOutcome, ProtocolError> {
        let resolved = self
            .auth
            .resolve(creds)
            .map_err(|_| ProtocolError::auth_required())?;
        if !resolved.can_access_owner(&owner) {
            return Err(ProtocolError::forbidden(
                "principal cannot access requested owner",
            ));
        }
        let outcome = self
            .storage
            .close_batch(&owner, _source_batch_id)
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

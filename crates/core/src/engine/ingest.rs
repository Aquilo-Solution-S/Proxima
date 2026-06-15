use super::Engine;
use crate::SchemaVersion;
use crate::authz::{AuthzContext, Role};
use crate::error::ProtocolError;
use crate::storage::StorageError;
use crate::verbs::close_batch::CloseBatchOutcome;
use crate::verbs::event_ingest::{
    AuthorizedEventIngest, AuthorizedFactWithCitation, AuthorizedInlineCitationMapping,
    AuthorizedInlineCitedObject, EventDraft, EventIngestOutcome, InlineCitationMappingDraft,
    InlineCitedObjectDraft,
};
use crate::verbs::persist_mcp_call::{McpCallLogInput, McpCallLogOutcome};
use crate::verbs::schema::{PayloadKind, SchemaInfo};
use crate::{MemoryId, Owner, Principal, SourceBatchId};

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
        let owner = authorized.draft().owner();
        let outcome = self
            .storage
            .ingest_event_atomic(authorized.draft())
            .await
            .map_err(|e| ProtocolError::internal(e.to_string()))?;
        if let Err(err) = self.ensure_fact_embedding(&owner, outcome.memory_id).await {
            tracing::warn!(
                memory_id = %outcome.memory_id.into_inner(),
                error = %err,
                "best-effort Fact embedding failed after event ingest",
            );
        }
        Ok(outcome)
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
        self.ensure_fact_schema(&draft.schema_id, draft.schema_version)?;
        draft.rendered_text = self.render_fact_text(&draft);
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

    /// Authorize + schema-validate + owner-stamp a Fact with typed
    /// inline citation payloads. Does NOT write.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context cannot access `draft.principal`,
    /// lacks `role`, or the citation mapping targets a different cited-object
    /// schema; `UnknownSchema` when any schema is absent for the required kind;
    /// `InvalidArgument` when JSON payload validation fails; or `Internal` when
    /// a registered citation schema has no sidecar inserter.
    pub fn authorize_fact_with_citation(
        &self,
        authz: &AuthzContext,
        role: Role,
        mut draft: EventDraft,
        cited_object: InlineCitedObjectDraft,
        mapping: InlineCitationMappingDraft,
    ) -> Result<AuthorizedFactWithCitation, ProtocolError> {
        super::authorize(authz, &draft.principal, role)?;
        let owner = authz.scoped_owner(draft.principal.clone());
        draft.stamp_owner(owner);

        // Validate the Fact only by schema-existence, matching
        // `authorize_event_ingest`. The Fact payload is built from a
        // trusted typed struct. The untrusted citation payloads are
        // agent-supplied JSON, so they stay fully validated below.
        self.ensure_fact_schema(&draft.schema_id, draft.schema_version)?;
        draft.rendered_text = self.render_fact_text(&draft);
        let cited_object_info = self.validate_json_payload(
            &cited_object.schema_id,
            cited_object.schema_version,
            PayloadKind::CitedObject,
            &cited_object.payload_bytes,
            "cited_object.payload_bytes",
        )?;
        let mapping_info = self.validate_json_payload(
            &mapping.schema_id,
            mapping.schema_version,
            PayloadKind::CitationMapping,
            &mapping.payload_bytes,
            "mapping.payload_bytes",
        )?;

        if mapping_info.cited_object_schema.as_ref() != Some(&cited_object.schema_id) {
            return Err(ProtocolError::forbidden(format!(
                "citation mapping schema {} v{} targets {:?}, not cited object schema {}",
                mapping.schema_id.as_str(),
                mapping.schema_version.into_inner(),
                mapping_info
                    .cited_object_schema
                    .as_ref()
                    .map(SchemaIdDisplay::new),
                cited_object.schema_id.as_str(),
            )));
        }

        let cited_object_sidecar_inserter =
            cited_object_info.sidecar_inserter.ok_or_else(|| {
                ProtocolError::internal(format!(
                    "cited object schema {} v{} has no sidecar inserter",
                    cited_object.schema_id.as_str(),
                    cited_object.schema_version.into_inner(),
                ))
            })?;
        // A pure-link mapping has no sidecar inserter — that's legal, so
        // pass the Option through rather than erroring on absence.
        let mapping_sidecar_inserter = mapping_info.sidecar_inserter;
        let content_hash = self
            .registry
            .content_hash_for(
                &cited_object.schema_id,
                cited_object.schema_version,
                &cited_object.payload_bytes,
            )
            .map_err(|e| ProtocolError::internal(e.to_string()))?;

        Ok(AuthorizedFactWithCitation::new(
            draft,
            AuthorizedInlineCitedObject::new(
                cited_object.schema_id,
                cited_object.schema_version,
                content_hash,
                cited_object.payload_bytes,
                cited_object_sidecar_inserter,
            ),
            AuthorizedInlineCitationMapping::new(
                mapping.schema_id,
                mapping.schema_version,
                mapping.payload_bytes,
                mapping_sidecar_inserter,
            ),
        ))
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

    fn ensure_fact_schema(
        &self,
        schema_id: &crate::SchemaId,
        schema_version: SchemaVersion,
    ) -> Result<(), ProtocolError> {
        if self
            .registry
            .lookup_payload(schema_id, schema_version, PayloadKind::Fact)
            .is_none()
        {
            return Err(ProtocolError::unknown_schema(
                schema_id.as_str(),
                schema_version.into_inner(),
            ));
        }
        Ok(())
    }

    fn render_fact_text(&self, draft: &EventDraft) -> Option<String> {
        match self.registry.try_render_fact_payload(
            &draft.schema_id,
            draft.schema_version,
            &draft.payload,
        ) {
            Ok(rendered_text) => rendered_text,
            Err(err) => {
                tracing::warn!(
                    schema_id = %draft.schema_id.as_str(),
                    schema_version = draft.schema_version.into_inner(),
                    error = %err,
                    "fact render failed; m.text left NULL",
                );
                None
            }
        }
    }

    /// Best-effort Fact embedding. Missing text or missing embedding
    /// client is a no-op; storage/LLM failures are returned to callers
    /// that explicitly requested embedding/backfill.
    ///
    /// # Errors
    ///
    /// Returns storage errors from text load/upsert, `Internal` for
    /// embedding client failures, and `ConstraintViolation` when the
    /// client returns a vector whose length differs from `dim()`.
    pub async fn ensure_fact_embedding(
        &self,
        owner: &Owner,
        memory_id: MemoryId,
    ) -> Result<(), StorageError> {
        let Some(text) = self.storage.load_fact_text(owner, memory_id).await? else {
            return Ok(());
        };
        let Some(client) = self.embed_client() else {
            return Ok(());
        };
        let embedding = client
            .embed(&text)
            .await
            .map_err(|e| StorageError::Internal(format!("embed Fact text: {e}")))?;
        if embedding.len() != client.dim() {
            return Err(StorageError::ConstraintViolation(format!(
                "embedding dim mismatch: client dim {} but vector len {}",
                client.dim(),
                embedding.len(),
            )));
        }
        self.storage
            .upsert_fact_embedding(
                owner,
                memory_id,
                client.model_id(),
                client.dim(),
                &embedding,
            )
            .await
    }

    /// Owner-scoped, idempotent backfill of missing Fact embeddings for
    /// the current embedding client's model id.
    ///
    /// # Errors
    ///
    /// Returns storage errors from listing/upserting or `Internal` for
    /// embedding client failures.
    pub async fn backfill_fact_embeddings(
        &self,
        owner: &Owner,
        limit: usize,
    ) -> Result<usize, StorageError> {
        let Some(client) = self.embed_client() else {
            return Ok(0);
        };
        let missing = self
            .storage
            .list_facts_missing_embedding(owner, client.model_id(), limit)
            .await?;
        let mut count = 0;
        for memory_id in missing {
            self.ensure_fact_embedding(owner, memory_id).await?;
            count += 1;
        }
        Ok(count)
    }

    pub(super) fn validate_json_payload<'a>(
        &'a self,
        schema_id: &crate::SchemaId,
        schema_version: SchemaVersion,
        kind: PayloadKind,
        payload_bytes: &[u8],
        field: &str,
    ) -> Result<&'a SchemaInfo, ProtocolError> {
        let info = self
            .registry
            .lookup_payload(schema_id, schema_version, kind)
            .ok_or_else(|| {
                ProtocolError::unknown_schema(schema_id.as_str(), schema_version.into_inner())
            })?;
        let payload: serde_json::Value = serde_json::from_slice(payload_bytes).map_err(|e| {
            ProtocolError::invalid_argument(field, format!("invalid JSON payload: {e}"))
        })?;
        self.registry
            .validate_payload(schema_id, schema_version, kind, &payload)
            .map_err(|e| ProtocolError::invalid_argument(field, e))?;
        Ok(info)
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

struct SchemaIdDisplay<'a>(&'a crate::SchemaId);

impl<'a> SchemaIdDisplay<'a> {
    const fn new(schema_id: &'a crate::SchemaId) -> Self {
        Self(schema_id)
    }
}

impl std::fmt::Debug for SchemaIdDisplay<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0.as_str())
    }
}

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

    /// Authorize + schema-validate + owner-stamp a Fact with typed
    /// inline citation payloads. Does NOT write.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context cannot access `draft.principal`,
    /// lacks `role`, or the citation mapping targets a different cited-object
    /// schema; `UnknownSchema` when any schema is absent for the required kind;
    /// `InvalidArgument` when CBOR payload validation fails; or `Internal` when
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
        // trusted typed struct, and CBOR->JSON revalidation cannot
        // represent binary fields (e.g. a Uuid serializes to a CBOR
        // byte string). The untrusted inputs are the citation payloads
        // (agent-supplied JSON), which stay fully validated below.
        self.ensure_event_ingest_schema(&draft.schema_id, draft.schema_version)?;
        let cited_object_info = self.validate_cbor_payload(
            &cited_object.schema_id,
            cited_object.schema_version,
            PayloadKind::CitedObject,
            &cited_object.payload_bytes,
            "cited_object.payload_bytes",
        )?;
        let mapping_info = self.validate_cbor_payload(
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
        let mapping_sidecar_inserter = mapping_info.sidecar_inserter.ok_or_else(|| {
            ProtocolError::internal(format!(
                "citation mapping schema {} v{} has no sidecar inserter",
                mapping.schema_id.as_str(),
                mapping.schema_version.into_inner(),
            ))
        })?;
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

    fn validate_cbor_payload<'a>(
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
        let payload: serde_json::Value =
            ciborium::de::from_reader(std::io::Cursor::new(payload_bytes)).map_err(|e| {
                ProtocolError::invalid_argument(field, format!("invalid CBOR payload: {e}"))
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

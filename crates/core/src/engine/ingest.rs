use std::sync::Arc;

use super::Engine;
use crate::SchemaVersion;
use crate::access::Relation;
use crate::authz::AuthzContext;
use crate::error::ProtocolError;
use crate::llm::EmbeddingClient;
use crate::storage::StorageError;
use crate::verbs::close_batch::CloseBatchOutcome;
use crate::verbs::fact_ingest::{
    AuthorizedCitationAttachment, AuthorizedFactWithCitation, AuthorizedFactWrite,
    AuthorizedInlineCitationMapping, AuthorizedInlineCitedObject, FactIngestOutcome,
    FactWriteCommand, InlineCitationMappingDraft, InlineCitedObjectDraft,
};
use crate::verbs::persist_mcp_call::{McpCallLogInput, McpCallLogOutcome};
use crate::verbs::schema::{PayloadKind, ProtocolPayload, SchemaInfo};
use crate::{EntityKind, MemoryId, Owner, OwnerRef, SidecarPayload, SourceBatchId};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EmbeddingDrainOutcome {
    pub processed: usize,
    pub failed: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EmbedStep {
    Embedded,
    NothingToEmbed,
}

impl Engine {
    /// docs/14 §"`FactIngest`" — Owner-scoped write. Validates
    /// schemas and delegates to storage.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context cannot resolve exactly one writable owner or
    /// lacks [`Relation::Ingest`] on that owner space; `UnknownSchema` when the
    /// Fact schema or provided citation schemas are not registered; or
    /// `Internal` when the atomic ingest fails.
    pub async fn fact_ingest(
        &self,
        authz: &AuthzContext,
        draft: FactWriteCommand,
    ) -> Result<FactIngestOutcome, ProtocolError> {
        let authorized = self
            .authorize_fact_ingest(authz, Relation::Ingest, draft)
            .await?;
        let embedding_client = self.embed_client();
        let embedding_model_id = embedding_client.as_ref().map(|client| client.model_id());
        let outcome = self
            .storage
            .ingest
            .fact_ingest
            .ingest_fact_atomic(
                authorized.permit().owner(),
                authorized.draft(),
                embedding_model_id,
            )
            .await
            .map_err(|e| ProtocolError::internal(e.to_string()))?;
        Ok(outcome)
    }

    /// Authorize + schema-validate + owner-stamp a Fact write,
    /// returning a witness required by the sidecar-ingest primitive.
    /// Does NOT write. `relation` is the relation the caller's operation
    /// requires.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context cannot resolve exactly one writable owner
    /// for `relation`; `UnknownSchema` when the Fact schema or provided citation schemas
    /// are not registered.
    pub async fn authorize_fact_ingest(
        &self,
        authz: &AuthzContext,
        relation: Relation,
        draft: FactWriteCommand,
    ) -> Result<AuthorizedFactWrite, ProtocolError> {
        let owner = self.single_write_owner_for(authz, relation).await?;
        let permit = self.authorize_write(authz, &owner, relation).await?;
        let fact_info = self.fact_schema_info(&draft.schema_id, draft.schema_version)?;
        let fact_sidecar_table = fact_info.sidecar_table.clone();
        let fact_natural_key_columns = fact_info.natural_key_columns.clone();
        if let Some(citation) = &draft.citation {
            self.ensure_fact_ingest_schema(
                &citation.object.schema_id,
                citation.object.schema_version,
            )?;
            self.ensure_fact_ingest_schema(
                &citation.mapping.schema_id,
                citation.mapping.schema_version,
            )?;
        }
        Ok(AuthorizedFactWrite::new(
            permit.into(),
            draft,
            fact_sidecar_table,
            fact_natural_key_columns,
        ))
    }

    /// Authorize + schema-validate + owner-stamp a Fact with typed
    /// inline citation payloads. Does NOT write.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context cannot resolve exactly one writable owner,
    /// lacks `relation`, or the
    /// citation mapping targets a different cited-object
    /// schema; `UnknownSchema` when any schema is absent for the required kind;
    /// `InvalidArgument` when JSON payload validation fails; or `Internal` when
    /// a registered citation schema has no sidecar inserter.
    pub async fn authorize_fact_with_citation(
        &self,
        authz: &AuthzContext,
        relation: Relation,
        draft: FactWriteCommand,
        cited_object: InlineCitedObjectDraft,
        mapping: InlineCitationMappingDraft,
    ) -> Result<AuthorizedFactWithCitation, ProtocolError> {
        let owner = self.single_write_owner_for(authz, relation).await?;
        let permit = self.authorize_write(authz, &owner, relation).await?;

        // Validate the Fact only by schema-existence, matching
        // `authorize_fact_ingest`. The Fact payload is built from a
        // trusted typed struct. The untrusted citation payloads are
        // agent-supplied JSON, so they stay fully validated below.
        let fact_info = self.fact_schema_info(&draft.schema_id, draft.schema_version)?;
        let fact_sidecar_table = fact_info.sidecar_table.clone();
        let fact_natural_key_columns = fact_info.natural_key_columns.clone();
        let (cited_object, mapping) = self.authorize_inline_citation(cited_object, mapping)?;

        Ok(AuthorizedFactWithCitation::new(
            permit.into(),
            draft,
            cited_object,
            mapping,
            fact_sidecar_table,
            fact_natural_key_columns,
        ))
    }

    async fn single_write_owner_for(
        &self,
        authz: &AuthzContext,
        relation: Relation,
    ) -> Result<Owner, ProtocolError> {
        let access = self.resolve_access(authz).await?;
        let owners = access.write_owners_for(relation);
        match owners.as_slice() {
            [owner] => Ok(*owner),
            [] => Err(ProtocolError::forbidden(relation.denied_message())),
            _ => Err(ProtocolError::invalid_argument(
                "owner",
                "FactWriteCommand is ownerless; authorization must resolve exactly one writable owner",
            )),
        }
    }

    /// Persist an already-authorized typed-sidecar Fact ingest.
    ///
    /// # Errors
    ///
    /// Returns `Internal` when the atomic storage write fails.
    pub async fn ingest_fact_with_typed_sidecar(
        &self,
        authorized: &AuthorizedFactWrite,
        sidecar: &SidecarPayload,
        embedding_model_id: Option<&str>,
    ) -> Result<FactIngestOutcome, ProtocolError> {
        self.storage()
            .ingest
            .fact_ingest
            .ingest_fact_with_typed_sidecar(authorized, sidecar, embedding_model_id)
            .await
            .map_err(|err| ProtocolError::internal(err.to_string()))
    }

    /// Persist an already-authorized typed-sidecar Fact with inline citation.
    ///
    /// # Errors
    ///
    /// Returns `Internal` when the atomic storage write fails.
    pub async fn ingest_fact_with_citation_and_typed_sidecar(
        &self,
        authorized: &AuthorizedFactWithCitation,
        sidecar: &SidecarPayload,
        embedding_model_id: Option<&str>,
    ) -> Result<FactIngestOutcome, ProtocolError> {
        self.storage()
            .ingest
            .fact_ingest
            .ingest_fact_with_citation_and_typed_sidecar(authorized, sidecar, embedding_model_id)
            .await
            .map_err(|err| ProtocolError::internal(err.to_string()))
    }

    /// Authorize + schema-validate + owner-stamp a citation attachment
    /// for an existing Fact memory. Does NOT write.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context cannot access `principal`,
    /// lacks `relation`, or the
    /// citation mapping targets a different cited-object
    /// schema; `UnknownSchema` when a citation schema is absent for the
    /// required kind; `InvalidArgument` when JSON payload validation fails; or
    /// `Internal` when a registered cited-object schema has no sidecar inserter.
    pub async fn authorize_citation_attachment(
        &self,
        authz: &AuthzContext,
        relation: Relation,
        principal: OwnerRef,
        memory_id: MemoryId,
        cited_object: InlineCitedObjectDraft,
        mapping: InlineCitationMappingDraft,
    ) -> Result<AuthorizedCitationAttachment, ProtocolError> {
        let requested = principal;
        let permit = self.authorize_write(authz, &requested, relation).await?;
        let owner = *permit.owner();
        let (cited_object, mapping) = self.authorize_inline_citation(cited_object, mapping)?;
        Ok(AuthorizedCitationAttachment::new(
            permit.into(),
            memory_id,
            owner,
            cited_object,
            mapping,
        ))
    }

    fn authorize_inline_citation(
        &self,
        cited_object: InlineCitedObjectDraft,
        mapping: InlineCitationMappingDraft,
    ) -> Result<(AuthorizedInlineCitedObject, AuthorizedInlineCitationMapping), ProtocolError> {
        let (cited_object_info, cited_object_payload) = self.ingest_protocol_payload(
            &cited_object.schema_id,
            cited_object.schema_version,
            PayloadKind::CitedObject,
            &cited_object.payload_bytes,
            "cited_object.payload_bytes",
        )?;
        let (mapping_info, mapping_payload) = self.ingest_protocol_payload(
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

        if cited_object_info.sidecar_table.is_none() {
            return Err(ProtocolError::internal(format!(
                "cited object schema {} v{} has no sidecar table",
                cited_object.schema_id.as_str(),
                cited_object.schema_version.into_inner(),
            )));
        }
        let content_hash = cited_object_payload.content_hash.ok_or_else(|| {
            ProtocolError::internal(format!(
                "cited object schema {} v{} did not produce a content hash",
                cited_object.schema_id.as_str(),
                cited_object.schema_version.into_inner(),
            ))
        })?;
        let cited_object_sidecar = cited_object_payload.sidecar_payload;
        let mapping_sidecar = if mapping_info.sidecar_table.is_some() {
            Some(mapping_payload.sidecar_payload)
        } else {
            None
        };

        Ok((
            AuthorizedInlineCitedObject::new(
                cited_object.schema_id,
                cited_object.schema_version,
                content_hash,
                cited_object_sidecar,
            ),
            AuthorizedInlineCitationMapping::new(
                mapping.schema_id,
                mapping.schema_version,
                mapping_sidecar,
            ),
        ))
    }

    fn ensure_fact_ingest_schema(
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

    fn fact_schema_info(
        &self,
        schema_id: &crate::SchemaId,
        schema_version: SchemaVersion,
    ) -> Result<&SchemaInfo, ProtocolError> {
        self.registry
            .lookup_payload(schema_id, schema_version, PayloadKind::Fact)
            .ok_or_else(|| {
                ProtocolError::unknown_schema(schema_id.as_str(), schema_version.into_inner())
            })
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
        self.ensure_memory_embedding(owner, EntityKind::Fact, memory_id)
            .await?;
        Ok(())
    }

    async fn ensure_memory_embedding(
        &self,
        owner: &Owner,
        entity_kind: EntityKind,
        memory_id: MemoryId,
    ) -> Result<bool, StorageError> {
        let Some(client) = self.embed_client() else {
            return Ok(false);
        };
        let step = self
            .embed_claimed_memory(&client, owner, entity_kind, memory_id)
            .await?;
        Ok(matches!(step, EmbedStep::Embedded))
    }

    async fn embed_claimed_memory(
        &self,
        client: &Arc<dyn EmbeddingClient>,
        owner: &Owner,
        entity_kind: EntityKind,
        memory_id: MemoryId,
    ) -> Result<EmbedStep, StorageError> {
        let Some(text) = self
            .storage
            .ingest
            .embedding_text
            .load_embedding_text(owner, entity_kind, memory_id)
            .await?
        else {
            return Ok(EmbedStep::NothingToEmbed);
        };
        let embedding = client
            .embed(&text)
            .await
            .map_err(|e| StorageError::Internal(format!("embed memory text: {e}")))?;
        if embedding.len() != client.dim() {
            return Err(StorageError::ConstraintViolation(format!(
                "embedding dim mismatch: client dim {} but vector len {}",
                client.dim(),
                embedding.len(),
            )));
        }
        self.storage
            .ingest
            .embedding_write
            .upsert_memory_embedding(
                owner,
                entity_kind,
                memory_id,
                client.model_id(),
                client.dim(),
                &embedding,
            )
            .await?;
        Ok(EmbedStep::Embedded)
    }

    /// Owner-scoped, idempotent backfill enqueue for missing Fact
    /// embeddings under the current embedding client's model id.
    ///
    /// # Errors
    ///
    /// Returns storage errors from enqueueing missing jobs.
    pub async fn backfill_fact_embeddings(
        &self,
        owner: &Owner,
        limit: usize,
    ) -> Result<usize, StorageError> {
        let Some(client) = self.embed_client() else {
            return Ok(0);
        };
        let limit = i64::try_from(limit)
            .map_err(|_| StorageError::ConstraintViolation("limit too large".into()))?;
        let enqueued = self
            .storage
            .ingest
            .embedding_job
            .enqueue_missing_embedding_jobs(owner, client.model_id(), limit)
            .await?;
        usize::try_from(enqueued)
            .map_err(|_| StorageError::Internal("enqueued count does not fit usize".into()))
    }

    /// Host-invoked sweep that drains durable pending memory embedding jobs
    /// for the currently active embedding model. This method does not
    /// spawn a worker, timer, or model decision loop; the caller controls
    /// invocation and `limit`.
    ///
    /// # Errors
    ///
    /// Returns storage errors from claiming or final job-state writes.
    /// Per-job embedding failures are recorded on their job rows and
    /// counted in the returned outcome. A failed embed ends the current
    /// drain call so retry cadence stays one attempt per invocation.
    pub async fn drain_embedding_jobs(
        &self,
        limit: usize,
    ) -> Result<EmbeddingDrainOutcome, StorageError> {
        let Some(client) = self.embed_client() else {
            return Ok(EmbeddingDrainOutcome::default());
        };
        let limit = i64::try_from(limit)
            .map_err(|_| StorageError::ConstraintViolation("limit too large".into()))?;
        let mut outcome = EmbeddingDrainOutcome::default();
        for _ in 0..limit {
            let Some(claim) = self
                .storage
                .ingest
                .embedding_job
                .claim_pending_embedding_jobs(client.model_id(), 1)
                .await?
                .into_iter()
                .next()
            else {
                break;
            };
            outcome.processed += 1;
            match self
                .embed_claimed_memory(&client, &claim.owner, claim.entity_kind, claim.entity_id)
                .await
            {
                Ok(EmbedStep::Embedded | EmbedStep::NothingToEmbed) => {
                    self.storage
                        .ingest
                        .embedding_job
                        .complete_embedding_job(&claim)
                        .await?;
                }
                Err(err) => {
                    outcome.failed += 1;
                    self.storage
                        .ingest
                        .embedding_job
                        .fail_embedding_job(&claim, &err.to_string())
                        .await?;
                    break;
                }
            }
        }
        Ok(outcome)
    }

    pub(super) fn ingest_protocol_payload<'a>(
        &'a self,
        schema_id: &crate::SchemaId,
        schema_version: SchemaVersion,
        kind: PayloadKind,
        payload_bytes: &[u8],
        field: &str,
    ) -> Result<(&'a SchemaInfo, ProtocolPayload), ProtocolError> {
        let info = self
            .registry
            .lookup_payload(schema_id, schema_version, kind)
            .ok_or_else(|| {
                ProtocolError::unknown_schema(schema_id.as_str(), schema_version.into_inner())
            })?;
        let payload: serde_json::Value = serde_json::from_slice(payload_bytes).map_err(|e| {
            ProtocolError::invalid_argument(field, format!("invalid JSON payload: {e}"))
        })?;
        let payload = self
            .registry
            .ingest_protocol_payload(schema_id, schema_version, kind, &payload)
            .map_err(|e| ProtocolError::invalid_argument(field, e))?;
        Ok((info, payload))
    }

    /// Owner-scoped write of a host-observed MCP activity log.
    ///
    /// Authorizes the caller against the log's Owner and derives that
    /// Owner from the authenticated identity — never trusting a
    /// caller-supplied Owner — mirroring [`Self::fact_ingest`]. The
    /// per-user actor (`actor_oid` / `actor_upn`) is recorded as Fact
    /// data; the graph Owner is what gets authorized here.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when `authz` cannot access the log Owner or lacks
    /// [`Relation::Ingest`] on the owner space;
    /// or `Internal` when the atomic write fails.
    pub async fn persist_mcp_call(
        &self,
        authz: &AuthzContext,
        mut input: McpCallLogInput,
    ) -> Result<McpCallLogOutcome, ProtocolError> {
        let owner = authz.scoped_owner(input.owner);
        let permit = self
            .authorize_write(authz, &owner, Relation::Ingest)
            .await?;
        input.owner = *permit.owner();
        self.storage
            .ingest
            .mcp_call_write
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
    /// different owner; `Forbidden` when the context cannot access `owner` or
    /// lacks [`Relation::Ingest`] on the owner space.
    pub async fn close_batch(
        &self,
        authz: &AuthzContext,
        principal: OwnerRef,
        source_batch_id: SourceBatchId,
    ) -> Result<CloseBatchOutcome, ProtocolError> {
        let requested = principal;
        let permit = self
            .authorize_write(authz, &requested, Relation::Ingest)
            .await?;
        let outcome = self
            .storage
            .ingest
            .source_batch
            .close_batch(permit.owner(), source_batch_id)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;
    use crate::ids::UserId;
    use crate::llm::{EMBEDDING_DIM, LlmError};
    use crate::verbs::schema::FlavorRegistryFrozen;
    use crate::{AuthPath, FactPayload, FlavorRegistry, PayloadKeyBuilder, SchemaId};
    use serde::{Deserialize, Serialize};

    #[derive(Debug)]
    struct TestEmbedding;

    #[async_trait::async_trait]
    impl EmbeddingClient for TestEmbedding {
        async fn embed(&self, _text: &str) -> Result<Vec<f32>, LlmError> {
            Ok(vec![0.0; EMBEDDING_DIM])
        }

        fn model_id(&self) -> &'static str {
            "test-embed"
        }

        fn dim(&self) -> usize {
            EMBEDDING_DIM
        }
    }

    fn test_owner() -> Owner {
        OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()))
    }

    #[derive(Debug, Serialize, Deserialize)]
    struct TestFact {
        fact_id: String,
    }

    impl FactPayload for TestFact {
        const SCHEMA_ID: &'static str = "test/ingest-stamp-fact";
        const SCHEMA_VERSION: u32 = 1;

        fn receipt_key(&self) -> Vec<u8> {
            let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
            key.field_str("fact_id", &self.fact_id);
            key.finish()
        }

        fn render(&self) -> String {
            self.fact_id.clone()
        }
    }

    #[tokio::test]
    async fn authorize_fact_ingest_stamps_draft_owner_from_permit() {
        let owner = test_owner();
        let mut registry = FlavorRegistry::new();
        registry.add_fact_schema_or_panic_for_tests::<TestFact>();
        let engine = Engine::new(registry.freeze_or_panic_for_tests());
        let authz = AuthzContext::single_owner(&owner, AuthPath::System);
        let draft = FactWriteCommand::from_payload(
            "test/source",
            SourceBatchId::new(uuid::Uuid::now_v7()),
            &TestFact {
                fact_id: "fact-1".to_string(),
            },
            time::OffsetDateTime::now_utc(),
        );

        let authorized = engine
            .authorize_fact_ingest(&authz, Relation::Ingest, draft)
            .await
            .expect("single-owner System context should authorize ingest");

        assert_eq!(authorized.permit().owner(), &owner);
    }

    #[tokio::test]
    async fn authorize_fact_ingest_denies_denied_context() {
        let owner = test_owner();
        let mut registry = FlavorRegistry::new();
        registry.add_fact_schema_or_panic_for_tests::<TestFact>();
        let engine = Engine::new(registry.freeze_or_panic_for_tests());
        let draft = FactWriteCommand::from_payload(
            "test/source",
            SourceBatchId::new(uuid::Uuid::now_v7()),
            &TestFact {
                fact_id: "fact-1".to_string(),
            },
            time::OffsetDateTime::now_utc(),
        );

        let err = engine
            .authorize_fact_ingest(
                &AuthzContext::denied_for_owner(&owner),
                Relation::Editor,
                draft,
            )
            .await
            .expect_err("denied context must fail");

        assert_eq!(err.code, ErrorCode::Forbidden);
    }

    #[tokio::test]
    async fn authorize_fact_with_citation_denies_denied_context() {
        let owner = test_owner();
        let mut registry = FlavorRegistry::new();
        registry.add_fact_schema_or_panic_for_tests::<TestFact>();
        let engine = Engine::new(registry.freeze_or_panic_for_tests());
        let draft = FactWriteCommand::from_payload(
            "test/source",
            SourceBatchId::new(uuid::Uuid::now_v7()),
            &TestFact {
                fact_id: "fact-1".to_string(),
            },
            time::OffsetDateTime::now_utc(),
        );
        let cited_object = InlineCitedObjectDraft {
            schema_id: SchemaId::new("test/cited-object".into()),
            schema_version: SchemaVersion::new(1),
            payload_bytes: Vec::new(),
        };
        let mapping = InlineCitationMappingDraft {
            schema_id: SchemaId::new("test/citation-mapping".into()),
            schema_version: SchemaVersion::new(1),
            payload_bytes: Vec::new(),
        };

        let err = engine
            .authorize_fact_with_citation(
                &AuthzContext::denied_for_owner(&owner),
                Relation::Editor,
                draft,
                cited_object,
                mapping,
            )
            .await
            .expect_err("denied context must fail before schema validation");

        assert_eq!(err.code, ErrorCode::Forbidden);
    }

    #[tokio::test]
    async fn embed_claimed_memory_without_text_returns_nothing_to_embed() {
        let engine = Engine::new(FlavorRegistryFrozen::new());
        let client: Arc<dyn EmbeddingClient> = Arc::new(TestEmbedding);

        let step = engine
            .embed_claimed_memory(
                &client,
                &test_owner(),
                EntityKind::Fact,
                MemoryId::new(uuid::Uuid::now_v7()),
            )
            .await
            .expect("RejectingStorage returns no text without error");

        assert_eq!(step, EmbedStep::NothingToEmbed);
    }

    #[tokio::test]
    async fn ensure_memory_embedding_without_client_preserves_noop_result() {
        let engine = Engine::new(FlavorRegistryFrozen::new());

        let embedded = engine
            .ensure_memory_embedding(
                &test_owner(),
                EntityKind::Fact,
                MemoryId::new(uuid::Uuid::now_v7()),
            )
            .await
            .expect("missing embedding client is a no-op");

        assert!(!embedded);
    }
}

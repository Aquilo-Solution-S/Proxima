use std::sync::Arc;

use super::Engine;
use crate::SchemaVersion;
use crate::access::Relation;
use crate::authz::{AuthzContext, EngineAuthority};
use crate::edge::EdgeEndpoint;
use crate::error::ProtocolError;
use crate::llm::{EMBEDDING_BATCH_SIZE, EmbeddingClient, LlmError};
use crate::storage::{EmbeddingJobClaim, StorageError};

use crate::verbs::fact_ingest::{
    AuthorizedCitationAttachment, AuthorizedFactWithCitation, AuthorizedFactWithCitationRef,
    AuthorizedFactWrite, AuthorizedInlineCitationMapping, AuthorizedInlineCitedObject,
    AuthorizedNodeLinks, FactIngestOutcome, FactWriteCommand, InlineCitationMappingDraft,
    InlineCitedObjectDraft,
};
use crate::verbs::persist_mcp_call::{McpCallLogInput, McpCallLogOutcome};
use crate::verbs::schema::{PayloadKind, ProtocolPayload, SchemaInfo};
use crate::{EmbeddableEntityRef, EntityKind, MemoryId, Owner, OwnerRef, SidecarPayload};

#[cfg(test)]
use crate::SourceBatchId;

/// Liveness probe after a provider refuses a batch.
///
/// Trivial and constant: a failed probe means the provider is down; a
/// successful one means the refused batch's contents are at fault. Shared
/// with [`crate::llm::embed_failure_blames_the_input`] so drain and write
/// ask the same question.
const TRANSIENT_BATCH_PROBE: &str = crate::llm::EMBED_LIVENESS_PROBE;

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
    /// Fact schema or provided citation schemas are not registered.
    /// Caller-fixable storage rejections (closed batch, concurrent
    /// citation) surface as `InvalidArgument`; infrastructure faults as
    /// `Internal`.
    ///
    /// A schema that declared `EMBEDDABLE = false` is written without a
    /// vector even when the host has an embedder configured.
    pub async fn fact_ingest<A>(
        &self,
        authority: &A,
        draft: FactWriteCommand,
    ) -> Result<FactIngestOutcome, ProtocolError>
    where
        A: EngineAuthority + ?Sized,
    {
        let authorized = self
            .authorize_fact_ingest(authority, Relation::Ingest, draft, &[])
            .await?;
        self.validate_write_permit(authorized.owner_write_permit())?;
        let embedding_client = self.embed_client();
        let requested = embedding_client.as_ref().map(|client| client.model_id());
        // `embed_client().map(model_id)` asks "is there an embedder", which
        // the schema's own declaration overrides — see `vector_model_for`.
        let embedding_model_id =
            self.vector_model_for(authorized.draft().schema_id.as_str(), requested);
        let outcome = self
            .storage
            .ingest
            .fact_ingest
            .ingest_fact_atomic(
                authorized.owner_write_permit(),
                authorized.draft(),
                embedding_model_id,
            )
            .await
            .map_err(|err| {
                super::errors::map_write_storage_error(
                    err,
                    "fact",
                    "fact ingest referenced row not found",
                )
            })?;
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
    pub async fn authorize_fact_ingest<A>(
        &self,
        authority: &A,
        relation: Relation,
        draft: FactWriteCommand,
        sidecars: &[SidecarPayload],
    ) -> Result<AuthorizedFactWrite, ProtocolError>
    where
        A: EngineAuthority + ?Sized,
    {
        let owner = self.single_write_owner_for(authority, relation)?;
        let permit = self.authorize_write(authority, &owner, relation).await?;
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
        let links = self
            .authorize_fact_node_links(authority, &draft, sidecars)
            .await?;
        Ok(AuthorizedFactWrite::new(
            permit.into(),
            draft,
            fact_sidecar_table,
            fact_natural_key_columns,
            links,
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
    pub async fn authorize_fact_with_citation<A>(
        &self,
        authority: &A,
        relation: Relation,
        draft: FactWriteCommand,
        cited_object: InlineCitedObjectDraft,
        mapping: InlineCitationMappingDraft,
        sidecars: &[SidecarPayload],
    ) -> Result<AuthorizedFactWithCitation, ProtocolError>
    where
        A: EngineAuthority + ?Sized,
    {
        let owner = self.single_write_owner_for(authority, relation)?;
        let permit = self.authorize_write(authority, &owner, relation).await?;

        // Validate the Fact only by schema-existence, matching
        // `authorize_fact_ingest`. The Fact payload is built from a
        // trusted typed struct. The untrusted citation payloads are
        // agent-supplied JSON, so they stay fully validated below.
        let fact_info = self.fact_schema_info(&draft.schema_id, draft.schema_version)?;
        let fact_sidecar_table = fact_info.sidecar_table.clone();
        let fact_natural_key_columns = fact_info.natural_key_columns.clone();
        let (cited_object, mapping) = self.authorize_inline_citation(cited_object, mapping)?;
        let links = self
            .authorize_fact_node_links(authority, &draft, sidecars)
            .await?;

        Ok(AuthorizedFactWithCitation::new(
            permit.into(),
            draft,
            cited_object,
            mapping,
            fact_sidecar_table,
            fact_natural_key_columns,
            links,
        ))
    }

    /// Authorize + schema-validate + owner-stamp a Fact that cites an
    /// ALREADY-STORED cited object by id. Does NOT write, and does not
    /// resolve the referenced object — existence, owner, and schema of
    /// the stored row are storage's check, inside the same transaction
    /// that writes the mapping (no TOCTOU window).
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context cannot resolve exactly one
    /// writable owner or lacks `relation`; `UnknownSchema` when the Fact
    /// or mapping schema is absent for the required kind;
    /// `InvalidArgument` when the mapping payload fails validation; or
    /// `Internal` when the mapping schema declares no cited-object
    /// target.
    pub async fn authorize_fact_with_citation_by_ref<A>(
        &self,
        authority: &A,
        relation: Relation,
        draft: FactWriteCommand,
        cited_object_id: uuid::Uuid,
        mapping: InlineCitationMappingDraft,
        sidecars: &[SidecarPayload],
    ) -> Result<AuthorizedFactWithCitationRef, ProtocolError>
    where
        A: EngineAuthority + ?Sized,
    {
        let owner = self.single_write_owner_for(authority, relation)?;
        let permit = self.authorize_write(authority, &owner, relation).await?;
        let fact_info = self.fact_schema_info(&draft.schema_id, draft.schema_version)?;
        let fact_sidecar_table = fact_info.sidecar_table.clone();
        let fact_natural_key_columns = fact_info.natural_key_columns.clone();
        let (mapping, expected_object_schema) = self.authorize_citation_mapping_draft(mapping)?;
        let links = self
            .authorize_fact_node_links(authority, &draft, sidecars)
            .await?;

        Ok(AuthorizedFactWithCitationRef::new(
            permit.into(),
            draft,
            cited_object_id,
            expected_object_schema,
            mapping,
            fact_sidecar_table,
            fact_natural_key_columns,
            links,
        ))
    }

    /// Resolve the index rows a Fact write is admitted to assert: its
    /// declared origins, and the references its typed payloads carry.
    ///
    /// A Fact sits at the bottom of the F/A/P order, so the layering rule
    /// alone decides what it may point at — Facts, Fact-entity heads, and
    /// Goals. Nothing here reads a kind off the caller: the origins come
    /// from the derivation declaration and the references from payload
    /// content, which is what keeps the edge set a function of node
    /// content.
    async fn authorize_fact_node_links<A>(
        &self,
        authority: &A,
        draft: &FactWriteCommand,
        sidecars: &[SidecarPayload],
    ) -> Result<AuthorizedNodeLinks, ProtocolError>
    where
        A: EngineAuthority + ?Sized,
    {
        let declared: Vec<_> = sidecars
            .iter()
            .flat_map(SidecarPayload::references)
            .collect();
        for reference in &declared {
            reference
                .validate()
                .map_err(|err| ProtocolError::invalid_argument("references", err))?;
        }
        let references: Vec<EdgeEndpoint> = declared
            .into_iter()
            .map(|reference| reference.target)
            .collect();
        let origins = self
            .authorize_fact_link_targets(authority, &draft.derived_from, "derived_from")
            .await?;
        let references = self
            .authorize_fact_link_targets(authority, &references, "references")
            .await?;
        Ok(AuthorizedNodeLinks::new(origins, references))
    }

    async fn authorize_fact_link_targets<A>(
        &self,
        authority: &A,
        targets: &[EdgeEndpoint],
        field: &str,
    ) -> Result<Vec<EdgeEndpoint>, ProtocolError>
    where
        A: EngineAuthority + ?Sized,
    {
        let mut out: Vec<EdgeEndpoint> = Vec::with_capacity(targets.len());
        for target in targets {
            match target.kind {
                EntityKind::Fact | EntityKind::Goal => {}
                EntityKind::Abstraction | EntityKind::Perspective => {
                    return Err(ProtocolError::invalid_argument(
                        field,
                        format!(
                            "layering violation: a Fact cannot point at a {}",
                            target.kind.as_str()
                        ),
                    ));
                }
            }
            match target.entity {
                crate::EntityRef::Memory(memory_id) => {
                    self.authorize_entry_read(authority, crate::EntityId::Memory(memory_id))
                        .await?;
                }
                crate::EntityRef::Goal(goal_id) => {
                    self.authorize_entry_read(authority, crate::EntityId::Goal(goal_id))
                        .await?;
                }
            }
            if !out.contains(target) {
                out.push(*target);
            }
        }
        Ok(out)
    }

    fn single_write_owner_for<A>(
        &self,
        authority: &A,
        relation: Relation,
    ) -> Result<Owner, ProtocolError>
    where
        A: EngineAuthority + ?Sized,
    {
        let operation = self.operation_authority(authority)?;
        let access = self.resolve_access_inner(operation.authz(), operation.redeemed_phase())?;
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

    /// The model a Fact of `schema_id` should be embedded under, given
    /// what the caller asked for: the caller's answer, unless the schema
    /// declared `EMBEDDABLE = false`.
    ///
    /// APPLIED HERE, NOT AT THE CALL SITES, because every typed Fact
    /// write in the process funnels through one of the four verbs that
    /// persist one — `fact_ingest` above, plus the three below — and none
    /// of them should have to remember. Count them when adding a fifth:
    /// `fact_ingest` was missed for a release because it does not share
    /// the `ingest_fact_*` name. A caller that computes
    /// `embed_client().map(model_id)` — which is what the upload verb
    /// does, and the obvious thing to write — is asking "is there an
    /// embedder", a question the schema's own declaration overrides.
    ///
    /// Storage would be the lower boundary, and cannot host this: the
    /// answer lives in the flavor registry, which storage does not hold.
    pub(in crate::engine) fn vector_model_for<'a>(
        &self,
        schema_id: &str,
        requested: Option<&'a str>,
    ) -> Option<&'a str> {
        requested.filter(|_| self.registry().schema_is_embeddable(schema_id))
    }

    /// Persist an already-authorized typed-sidecar Fact ingest.
    ///
    /// `embedding_model_id` is a request, not an instruction: a schema
    /// that declared `EMBEDDABLE = false` is written without a vector
    /// whatever the caller passes.
    ///
    /// # Errors
    ///
    /// Returns caller-fixable storage rejections (closed batch, concurrent
    /// citation) as `InvalidArgument` and infrastructure faults as
    /// `Internal`.
    pub async fn ingest_fact_with_typed_sidecar(
        &self,
        authorized: &AuthorizedFactWrite,
        sidecars: &[SidecarPayload],
        embedding_model_id: Option<&str>,
    ) -> Result<FactIngestOutcome, ProtocolError> {
        self.validate_write_permit(authorized.owner_write_permit())?;
        let embedding_model_id =
            self.vector_model_for(authorized.draft().schema_id.as_str(), embedding_model_id);
        self.storage()
            .ingest
            .fact_ingest
            .ingest_fact_with_typed_sidecar(authorized, sidecars, embedding_model_id)
            .await
            .map_err(|err| {
                super::errors::map_write_storage_error(
                    err,
                    "fact",
                    "fact ingest referenced row not found",
                )
            })
    }

    /// Persist an already-authorized typed-sidecar Fact with inline citation.
    ///
    /// # Errors
    ///
    /// Returns caller-fixable storage rejections (closed batch, concurrent
    /// citation) as `InvalidArgument` and infrastructure faults as
    /// `Internal`.
    pub async fn ingest_fact_with_citation_and_typed_sidecar(
        &self,
        authorized: &AuthorizedFactWithCitation,
        sidecars: &[SidecarPayload],
        embedding_model_id: Option<&str>,
    ) -> Result<FactIngestOutcome, ProtocolError> {
        self.validate_write_permit(authorized.owner_write_permit())?;
        let embedding_model_id =
            self.vector_model_for(authorized.draft().schema_id.as_str(), embedding_model_id);
        self.storage()
            .ingest
            .fact_ingest
            .ingest_fact_with_citation_and_typed_sidecar(authorized, sidecars, embedding_model_id)
            .await
            .map_err(|err| {
                super::errors::map_write_storage_error(
                    err,
                    "fact",
                    "fact ingest referenced row not found",
                )
            })
    }

    /// Persist an already-authorized typed-sidecar Fact citing an
    /// existing object by reference.
    ///
    /// # Errors
    ///
    /// Returns caller-fixable storage rejections (closed batch, missing
    /// or foreign cited object, mapping-target mismatch) as
    /// `InvalidArgument` and infrastructure faults as `Internal`.
    pub async fn ingest_fact_with_citation_ref_and_typed_sidecar(
        &self,
        authorized: &AuthorizedFactWithCitationRef,
        sidecars: &[SidecarPayload],
        embedding_model_id: Option<&str>,
    ) -> Result<FactIngestOutcome, ProtocolError> {
        self.validate_write_permit(authorized.owner_write_permit())?;
        let embedding_model_id =
            self.vector_model_for(authorized.draft().schema_id.as_str(), embedding_model_id);
        self.storage()
            .ingest
            .fact_ingest
            .ingest_fact_with_citation_ref_and_typed_sidecar(
                authorized,
                sidecars,
                embedding_model_id,
            )
            .await
            .map_err(|err| {
                super::errors::map_write_storage_error(
                    err,
                    "fact",
                    "fact ingest referenced row not found",
                )
            })
    }

    /// Authorize + schema-validate + owner-stamp a citation attachment
    /// for an existing Fact memory. Does NOT write.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context cannot access `requested_owner`,
    /// lacks `relation`, or the citation mapping targets a different
    /// cited-object schema; `UnknownSchema` when a citation schema is
    /// absent for the required kind; `InvalidArgument` when JSON payload
    /// validation fails or `memory_kind` is not a kind that cites
    /// directly; or `Internal` when a registered cited-object schema has
    /// no sidecar inserter.
    #[allow(clippy::too_many_arguments)] // one parameter per authorized fact
    pub async fn authorize_citation_attachment(
        &self,
        authz: &AuthzContext,
        relation: Relation,
        requested_owner: OwnerRef,
        memory_id: MemoryId,
        memory_kind: EntityKind,
        cited_object: InlineCitedObjectDraft,
        mapping: InlineCitationMappingDraft,
    ) -> Result<AuthorizedCitationAttachment, ProtocolError> {
        let requested = requested_owner;
        let permit = self.authorize_write(authz, &requested, relation).await?;
        let owner = *permit.owner();
        // A citation is legal on a Fact or an Abstraction and on nothing
        // else. The rule is about what a memory kind MEANS — a
        // Perspective that cited directly would be grounding itself
        // twice, alongside the references it already grounds through —
        // so it is decided here, from the declared kind, and storage
        // rejects the write if the row disagrees.
        if !crate::citations::kind_may_cite_directly(memory_kind) {
            return Err(ProtocolError::invalid_argument(
                "memory_kind",
                format!(
                    "a {} cannot carry a citation; only Fact and Abstraction memories cite directly",
                    memory_kind.as_str()
                ),
            ));
        }
        let (cited_object, mapping) = self.authorize_inline_citation(cited_object, mapping)?;
        Ok(AuthorizedCitationAttachment::new(
            permit.into(),
            memory_id,
            memory_kind,
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
        let (_cited_object_info, cited_object_payload) = self.ingest_protocol_payload(
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

    /// Validate a citation-mapping draft alone (no inline cited object),
    /// returning the authorized mapping plus the cited-object schema it
    /// targets — the schema the referenced stored object must carry.
    fn authorize_citation_mapping_draft(
        &self,
        mapping: InlineCitationMappingDraft,
    ) -> Result<(AuthorizedInlineCitationMapping, crate::SchemaId), ProtocolError> {
        let (mapping_info, mapping_payload) = self.ingest_protocol_payload(
            &mapping.schema_id,
            mapping.schema_version,
            PayloadKind::CitationMapping,
            &mapping.payload_bytes,
            "mapping.payload_bytes",
        )?;
        let expected_object_schema = mapping_info.cited_object_schema.clone().ok_or_else(|| {
            ProtocolError::internal(format!(
                "citation mapping schema {} v{} declares no cited-object schema",
                mapping.schema_id.as_str(),
                mapping.schema_version.into_inner(),
            ))
        })?;
        let mapping_sidecar = if mapping_info.sidecar_table.is_some() {
            Some(mapping_payload.sidecar_payload)
        } else {
            None
        };
        Ok((
            AuthorizedInlineCitationMapping::new(
                mapping.schema_id,
                mapping.schema_version,
                mapping_sidecar,
            ),
            expected_object_schema,
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
    /// A Fact whose schema declared `EMBEDDABLE = false` is left alone:
    /// this is a no-op for it, not a forced vector.
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
        // This path holds a `MemoryId` and never passes through the job
        // queue, so the enqueue-side exclusions do not apply to it. The
        // schema's declaration is enforced here instead.
        let Some(text) = self
            .storage
            .ingest
            .embedding_text
            .load_embedding_text(
                owner,
                entity_kind,
                memory_id,
                self.registry().non_embeddable_schema_ids(),
            )
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
            .insert_embedding(
                owner,
                EmbeddableEntityRef::Memory {
                    kind: entity_kind,
                    memory_id,
                },
                client.model_id(),
                client.dim(),
                &embedding,
                crate::storage_ports::EmbeddingWriteProof::new(),
            )
            .await?;
        Ok(EmbedStep::Embedded)
    }

    /// Owner-scoped, idempotent backfill enqueue for memories missing an
    /// embedding under the current client's model id.
    ///
    /// Covers Facts *and* derived memories. Derived rows matter because a
    /// flavor can materialize Abstractions through its own sidecar path with
    /// no embedding client in scope — code-chunk ingest does — leaving them
    /// semantically invisible until someone ran a global reconcile.
    ///
    /// # Errors
    ///
    /// Returns authorization failures with their protocol category
    /// (e.g. `Forbidden`) rather than an internal-error string, and
    /// storage errors from enqueueing missing jobs as `Internal`.
    pub async fn backfill_missing_embeddings(
        &self,
        authz: &AuthzContext,
        owner: &Owner,
        limit: usize,
    ) -> Result<usize, ProtocolError> {
        self.operation_authority(authz)?;
        let Some(client) = self.embed_client() else {
            return Ok(0);
        };
        let limit = i64::try_from(limit)
            .map_err(|_| ProtocolError::invalid_argument("limit", "too large"))?;
        let permit = self.authorize_write(authz, owner, Relation::Ingest).await?;
        let enqueued = self
            .storage
            .ingest
            .embedding_job
            .enqueue_missing_embedding_jobs(
                permit.owner_write_permit(),
                client.model_id(),
                limit,
                self.registry().non_embeddable_schema_ids(),
            )
            .await
            .map_err(|err| ProtocolError::internal(err.to_string()))?;
        usize::try_from(enqueued)
            .map_err(|_| ProtocolError::internal("enqueued count does not fit usize"))
    }

    /// Host-invoked sweep that drains durable pending memory embedding jobs
    /// for the currently active embedding model. This method does not
    /// spawn a worker, timer, or model decision loop; the caller controls
    /// invocation and `limit`. Jobs are claimed and embedded in batches of
    /// up to [`EMBEDDING_BATCH_SIZE`] texts per provider call (`/embeddings`
    /// endpoints accept arrays), dividing request count — and request-rate-
    /// limit pressure — by the batch width.
    ///
    /// Failure semantics:
    /// - a *transient* batch failure (429/5xx/network) releases the claimed
    ///   jobs back to `pending` without burning retry attempts — a provider
    ///   outage is not evidence against any individual job — and ends the
    ///   drain call;
    /// - a *permanent* batch rejection ([`LlmError::EmbedPermanent`])
    ///   re-embeds the batch one text at a time to isolate the poison
    ///   input(s); an over-limit input is rescued by bisecting it into
    ///   chunked embeddings (full coverage), jobs rejected at every length go
    ///   terminal instead of cycling reject-retry forever, and their
    ///   batch-mates still embed.
    ///
    /// # Errors
    ///
    /// Returns storage errors from claiming or final job-state writes.
    /// Per-job embedding failures are recorded on their job rows and
    /// counted in the returned outcome; each job receives at most one
    /// attempt per invocation.
    pub async fn drain_embedding_jobs(
        &self,
        limit: usize,
    ) -> Result<EmbeddingDrainOutcome, StorageError> {
        let Some(client) = self.embed_client() else {
            return Ok(EmbeddingDrainOutcome::default());
        };
        let mut outcome = EmbeddingDrainOutcome::default();
        let mut remaining = limit;
        while remaining > 0 {
            let take = i64::try_from(remaining.min(EMBEDDING_BATCH_SIZE))
                .map_err(|_| StorageError::ConstraintViolation("limit too large".into()))?;
            let claims = self
                .storage
                .ingest
                .embedding_job
                .claim_pending_embedding_jobs(client.model_id(), take)
                .await?;
            if claims.is_empty() {
                break;
            }
            remaining = remaining.saturating_sub(claims.len());

            // Jobs whose memory no longer yields embeddable text are
            // complete as-is; only texted jobs go to the provider.
            let mut batch: Vec<(EmbeddingJobClaim, String)> = Vec::with_capacity(claims.len());
            for claim in claims {
                let text = self
                    .storage
                    .ingest
                    .embedding_text
                    .load_embedding_text(
                        &claim.owner,
                        claim.entity_kind,
                        claim.entity_id,
                        // Also excluded here, not just at enqueue: a job
                        // queued before its schema declared
                        // `EMBEDDABLE = false` completes as a no-op
                        // instead of embedding what now declines a vector.
                        self.registry().non_embeddable_schema_ids(),
                    )
                    .await?;
                if let Some(text) = text {
                    batch.push((claim, text));
                } else {
                    outcome.processed += 1;
                    self.storage
                        .ingest
                        .embedding_job
                        .complete_embedding_job(&claim)
                        .await?;
                }
            }
            if batch.is_empty() {
                continue;
            }

            let texts: Vec<String> = batch.iter().map(|(_, text)| text.clone()).collect();
            match client.embed_many(&texts).await {
                Ok(vectors) => {
                    for ((claim, _), vector) in batch.iter().zip(vectors) {
                        outcome.processed += 1;
                        if !self.store_claim_embedding(&client, claim, &vector).await? {
                            outcome.failed += 1;
                        }
                    }
                }
                Err(LlmError::EmbedPermanent(_)) => {
                    self.embed_claims_individually(&client, batch, &mut outcome)
                        .await?;
                }
                Err(err) => {
                    // A transient batch error is supposed to mean the
                    // provider failed rather than any input being bad — but
                    // the two are indistinguishable from the response when
                    // the provider fails *because of* an input. Observed
                    // against a local runner: one scanned page whose OCR
                    // hallucinated a 300-row CJK table killed the model
                    // process, which surfaces as `400 {"error": "… EOF"}`,
                    // correctly classified transient because nothing looked
                    // at the input. Released unburned, the whole claim of 32
                    // came back every drain and 31 innocent pages of the book
                    // stayed unembedded indefinitely.
                    //
                    // Probing separates the cases. If the provider answers a
                    // trivial input right after refusing the batch, it is up,
                    // and this batch's failure is attributable to its
                    // contents — so isolate them the same way a permanent
                    // rejection is isolated. If the probe also fails, the
                    // provider really is down: release without burning
                    // attempts, exactly as before, for one extra tiny call.
                    if client.embed(TRANSIENT_BATCH_PROBE).await.is_ok() {
                        tracing::warn!(
                            error = %err,
                            jobs = batch.len(),
                            "transient embedding batch failure but the provider answers; \
                             isolating inputs instead of holding the batch"
                        );
                        self.embed_claims_individually(&client, batch, &mut outcome)
                            .await?;
                        continue;
                    }
                    let claims: Vec<EmbeddingJobClaim> =
                        batch.into_iter().map(|(claim, _)| claim).collect();
                    tracing::warn!(
                        error = %err,
                        jobs = claims.len(),
                        "transient embedding batch failure; releasing claims without burning attempts"
                    );
                    self.storage
                        .ingest
                        .embedding_job
                        .release_embedding_jobs(&claims, &format!("embed memory text: {err}"))
                        .await?;
                    break;
                }
            }
        }
        Ok(outcome)
    }

    /// Store one produced vector for its claim and complete the job; a
    /// dimension mismatch records an ordinary retryable job failure
    /// instead. Returns whether the vector was stored.
    async fn store_claim_embedding(
        &self,
        client: &Arc<dyn EmbeddingClient>,
        claim: &EmbeddingJobClaim,
        vector: &[f32],
    ) -> Result<bool, StorageError> {
        if vector.len() != client.dim() {
            let error = format!(
                "embedding dim mismatch: client dim {} but vector len {}",
                client.dim(),
                vector.len(),
            );
            self.storage
                .ingest
                .embedding_job
                .fail_embedding_job(claim, &error)
                .await?;
            return Ok(false);
        }
        self.storage
            .ingest
            .embedding_write
            .insert_embedding(
                &claim.owner,
                EmbeddableEntityRef::Memory {
                    kind: claim.entity_kind,
                    memory_id: claim.entity_id,
                },
                client.model_id(),
                client.dim(),
                vector,
                crate::storage_ports::EmbeddingWriteProof::new(),
            )
            .await?;
        self.storage
            .ingest
            .embedding_job
            .complete_embedding_job(claim)
            .await?;
        Ok(true)
    }

    /// Per-item fallback after a permanent batch rejection: isolate which
    /// inputs the provider rejects. A rejected input is bisected into
    /// provider-acceptable chunks ([`crate::llm::embed_in_chunks`]) — over-limit
    /// inputs, the dominant permanent cause, stay fully semantically
    /// findable through chunked embeddings instead of going invisible.
    /// Inputs the provider rejects at every length go terminal; other
    /// errors record one ordinary attempt. Either way each job gets at
    /// most one attempt in this pass.
    async fn embed_claims_individually(
        &self,
        client: &Arc<dyn EmbeddingClient>,
        batch: Vec<(EmbeddingJobClaim, String)>,
        outcome: &mut EmbeddingDrainOutcome,
    ) -> Result<(), StorageError> {
        for (claim, text) in batch {
            outcome.processed += 1;
            match client.embed(&text).await {
                Ok(vector) => {
                    if !self.store_claim_embedding(client, &claim, &vector).await? {
                        outcome.failed += 1;
                    }
                }
                Err(LlmError::EmbedPermanent(message)) => {
                    match crate::llm::embed_in_chunks(client.as_ref(), &text).await {
                        Ok(Some(vectors)) => {
                            tracing::warn!(
                                entity_id = ?claim.entity_id,
                                chunks = vectors.len(),
                                total_bytes = text.len(),
                                "over-limit embedding input rescued as chunked embeddings"
                            );
                            if !self
                                .store_claim_embedding_chunks(client, &claim, &vectors)
                                .await?
                            {
                                outcome.failed += 1;
                            }
                        }
                        Ok(None) => {
                            outcome.failed += 1;
                            tracing::warn!(
                                entity_id = ?claim.entity_id,
                                "embedding input permanently rejected at every length; job going terminal"
                            );
                            self.storage
                                .ingest
                                .embedding_job
                                .fail_embedding_job_permanently(
                                    &claim,
                                    &format!("embed memory text: {message}"),
                                )
                                .await?;
                        }
                        Err(err) => {
                            outcome.failed += 1;
                            self.storage
                                .ingest
                                .embedding_job
                                .fail_embedding_job(
                                    &claim,
                                    &format!("embed truncated memory text: {err}"),
                                )
                                .await?;
                        }
                    }
                }
                Err(err) => {
                    outcome.failed += 1;
                    self.storage
                        .ingest
                        .embedding_job
                        .fail_embedding_job(&claim, &format!("embed memory text: {err}"))
                        .await?;
                }
            }
        }
        Ok(())
    }

    /// Store one chunked embedding version for its claim and complete the
    /// job; a dimension mismatch in any chunk records an ordinary
    /// retryable job failure instead. Returns whether the version was
    /// stored.
    async fn store_claim_embedding_chunks(
        &self,
        client: &Arc<dyn EmbeddingClient>,
        claim: &EmbeddingJobClaim,
        vectors: &[Vec<f32>],
    ) -> Result<bool, StorageError> {
        if vectors.is_empty() || vectors.iter().any(|vector| vector.len() != client.dim()) {
            let error = format!(
                "chunked embedding dim mismatch: client dim {} but got {} chunk(s) of lens {:?}",
                client.dim(),
                vectors.len(),
                vectors.iter().map(Vec::len).collect::<Vec<_>>(),
            );
            self.storage
                .ingest
                .embedding_job
                .fail_embedding_job(claim, &error)
                .await?;
            return Ok(false);
        }
        let chunks: Vec<&[f32]> = vectors.iter().map(Vec::as_slice).collect();
        self.storage
            .ingest
            .embedding_write
            .insert_embedding_chunks(
                &claim.owner,
                EmbeddableEntityRef::Memory {
                    kind: claim.entity_kind,
                    memory_id: claim.entity_id,
                },
                client.model_id(),
                client.dim(),
                &chunks,
                crate::storage_ports::EmbeddingWriteProof::new(),
            )
            .await?;
        self.storage
            .ingest
            .embedding_job
            .complete_embedding_job(claim)
            .await?;
        Ok(true)
    }

    /// Host-invoked global reconciliation: enqueue durable embedding jobs for
    /// every embeddable memory in `scope` that lacks coverage under the
    /// active embedding client's model. Complements [`Self::drain_embedding_jobs`]:
    /// drain heals the queue, reconcile heals the *absence* of queue entries
    /// (memories written while no embedding client was configured, model
    /// changes, `failed` jobs whose retries are exhausted). Idempotent; like
    /// drain, the caller controls invocation — no worker or timer is spawned.
    ///
    /// # Errors
    ///
    /// Returns storage errors from the reconciliation scan/enqueue.
    pub async fn reconcile_embeddings(
        &self,
        scope: crate::EmbeddingReconcileScope,
        limit: Option<i64>,
    ) -> Result<crate::EmbeddingReconcileOutcome, StorageError> {
        let Some(client) = self.embed_client() else {
            return Ok(crate::EmbeddingReconcileOutcome::default());
        };
        self.storage
            .compliance
            .embedding_maintenance
            .reconcile_embeddings(
                crate::EmbeddingReconcileOptions {
                    model_id: client.model_id(),
                    scope,
                    limit,
                    non_embeddable_schemas: self.registry().non_embeddable_schema_ids(),
                },
                crate::storage_ports::OperatorMaintenanceProof::new(),
            )
            .await
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
            .persist_mcp_call_atomic(permit.owner_write_permit(), &input)
            .await
            .map_err(|err| {
                super::errors::map_write_storage_error(
                    err,
                    "mcp_call",
                    "mcp call referenced row not found",
                )
            })
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
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        let draft = FactWriteCommand::from_payload(
            "test/source",
            SourceBatchId::new(uuid::Uuid::now_v7()),
            &TestFact {
                fact_id: "fact-1".to_string(),
            },
            time::OffsetDateTime::now_utc(),
        );

        let authorized = engine
            .authorize_fact_ingest(&authz, Relation::Ingest, draft, &[])
            .await
            .expect("single-owner host context should authorize ingest");

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
                &[],
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
                &[],
            )
            .await
            .expect_err("denied context must fail before schema validation");

        assert_eq!(err.code, ErrorCode::Forbidden);
    }

    #[tokio::test]
    async fn embed_claimed_memory_without_text_returns_nothing_to_embed() {
        let engine = Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests());
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
        let engine = Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests());

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

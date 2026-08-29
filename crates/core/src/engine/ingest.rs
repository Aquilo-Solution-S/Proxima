use std::sync::Arc;

use super::Engine;
use crate::SchemaVersion;
use crate::access::Relation;
use crate::authz::{AuthzContext, EngineAuthority};
use crate::edge::EdgeEndpoint;
use crate::error::ProtocolError;
use crate::llm::{EmbeddingClient, LlmError};
use crate::storage::{EmbeddingJobClaim, StorageError};
use crate::storage_ports::EmbeddingJobHandle;

use crate::verbs::fact_ingest::{
    AuthorizedCitationAttachment, AuthorizedFactWithCitation, AuthorizedFactWithCitationRef,
    AuthorizedFactWrite, AuthorizedInlineCitationMapping, AuthorizedInlineCitedObject,
    AuthorizedNodeLinks, CitationSpec, FactIngestOutcome, FactWriteCommand,
    InlineCitationMappingDraft, InlineCitedObjectDraft,
};
use crate::verbs::persist_mcp_call::{
    MCP_CALL_CITATION_SCHEMA, MCP_CALL_IO_SCHEMA, MCP_CALL_SOURCE_ID, McpCallLogInput,
    McpCallLogOutcome,
};
use crate::verbs::schema::{PayloadKind, ProtocolPayload, SchemaInfo};
use crate::{
    EmbeddableEntityRef, EntityKind, MemoryId, Owner, OwnerRef, SidecarPayload, SourceBatchId,
};

/// Liveness probe after a provider refuses a batch.
///
/// Trivial and constant: a failed probe means the provider is down; a
/// successful one means the refused batch's contents are at fault. Shared
/// with [`crate::llm::embed_failure_blames_the_input`] so drain and write
/// ask the same question.
const TRANSIENT_BATCH_PROBE: &str = crate::llm::EMBED_LIVENESS_PROBE;

fn normalize_fact_source_kind(draft: &mut FactWriteCommand) -> Result<(), ProtocolError> {
    if draft.kind.is_empty() {
        "fact".clone_into(&mut draft.kind);
    }
    if draft.kind != "fact" {
        return Err(ProtocolError::invalid_argument(
            "kind",
            "Fact ingest requires a Fact source",
        ));
    }
    Ok(())
}

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

struct EmbeddingClaimHeartbeat {
    handle: tokio::task::JoinHandle<()>,
}

impl Drop for EmbeddingClaimHeartbeat {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

fn spawn_embedding_claim_heartbeat(
    jobs: EmbeddingJobHandle,
    claims: Vec<EmbeddingJobClaim>,
    interval: std::time::Duration,
) -> EmbeddingClaimHeartbeat {
    let handle = tokio::spawn(async move {
        loop {
            tokio::time::sleep(interval).await;
            if let Err(err) = jobs.renew_embedding_jobs(&claims).await {
                tracing::warn!(error = %err, "embedding claim heartbeat failed");
            }
        }
    });
    EmbeddingClaimHeartbeat { handle }
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
    /// A schema whose recipe resolves to no embed unit is written without a
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
            .ingest_authorized_fact_atomic(&authorized, embedding_model_id)
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
        self.authorize_fact_ingest_visible(authority, relation, draft, sidecars, &[], &[])
            .await
    }

    /// [`Self::authorize_fact_ingest`] treating `session_visible` memory
    /// ids as already read-checked (written earlier in the same [`UnitOfWork`]).
    pub(in crate::engine) async fn authorize_fact_ingest_visible<A>(
        &self,
        authority: &A,
        relation: Relation,
        mut draft: FactWriteCommand,
        sidecars: &[SidecarPayload],
        session_visible: &[MemoryId],
        session_visible_kinds: &[(MemoryId, EntityKind)],
    ) -> Result<AuthorizedFactWrite, ProtocolError>
    where
        A: EngineAuthority + ?Sized,
    {
        let owner = self.single_write_owner_for(authority, relation)?;
        let permit = self.authorize_write(authority, &owner, relation).await?;
        normalize_fact_source_kind(&mut draft)?;
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
            .authorize_fact_node_links(
                authority,
                &draft,
                sidecars,
                session_visible,
                session_visible_kinds,
            )
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
        mut draft: FactWriteCommand,
        cited_object: InlineCitedObjectDraft,
        mapping: InlineCitationMappingDraft,
        sidecars: &[SidecarPayload],
    ) -> Result<AuthorizedFactWithCitation, ProtocolError>
    where
        A: EngineAuthority + ?Sized,
    {
        let owner = self.single_write_owner_for(authority, relation)?;
        let permit = self.authorize_write(authority, &owner, relation).await?;
        normalize_fact_source_kind(&mut draft)?;

        // Validate the Fact only by schema-existence, matching
        // `authorize_fact_ingest`. The Fact payload is built from a
        // trusted typed struct. The untrusted citation payloads are
        // agent-supplied JSON, so they stay fully validated below.
        let fact_info = self.fact_schema_info(&draft.schema_id, draft.schema_version)?;
        let fact_sidecar_table = fact_info.sidecar_table.clone();
        let fact_natural_key_columns = fact_info.natural_key_columns.clone();
        let (cited_object, mapping) = self.authorize_inline_citation(cited_object, mapping)?;
        let links = self
            .authorize_fact_node_links(authority, &draft, sidecars, &[], &[])
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
        mut draft: FactWriteCommand,
        cited_object_id: uuid::Uuid,
        mapping: InlineCitationMappingDraft,
        sidecars: &[SidecarPayload],
    ) -> Result<AuthorizedFactWithCitationRef, ProtocolError>
    where
        A: EngineAuthority + ?Sized,
    {
        let owner = self.single_write_owner_for(authority, relation)?;
        let permit = self.authorize_write(authority, &owner, relation).await?;
        normalize_fact_source_kind(&mut draft)?;
        let fact_info = self.fact_schema_info(&draft.schema_id, draft.schema_version)?;
        let fact_sidecar_table = fact_info.sidecar_table.clone();
        let fact_natural_key_columns = fact_info.natural_key_columns.clone();
        let (mapping, expected_object_schema) = self.authorize_citation_mapping_draft(mapping)?;
        let links = self
            .authorize_fact_node_links(authority, &draft, sidecars, &[], &[])
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
    /// A Fact sits at the bottom of the F/A/P order, so its pins may target
    /// Facts or Goals. Origins come from the derivation declaration and
    /// references from payload content; every declared endpoint kind is
    /// checked against the resolved target before the authorized carrier is
    /// minted.
    async fn authorize_fact_node_links<A>(
        &self,
        authority: &A,
        draft: &FactWriteCommand,
        sidecars: &[SidecarPayload],
        session_visible: &[MemoryId],
        session_visible_kinds: &[(MemoryId, EntityKind)],
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
        let typed_references: Vec<EdgeEndpoint> = declared
            .into_iter()
            .map(|reference| reference.target)
            .fold(Vec::new(), |mut references, target| {
                if !references.contains(&target) {
                    references.push(target);
                }
                references
            });
        let payload_references = typed_references.clone();
        let raw_references: Vec<EdgeEndpoint> = draft
            .refs
            .iter()
            .copied()
            .map(|id| EdgeEndpoint::memory(EntityKind::Fact, MemoryId::new(id)))
            .fold(Vec::new(), |mut references, target| {
                if !references.contains(&target) {
                    references.push(target);
                }
                references
            });
        if !draft.refs.is_empty() && !typed_references.is_empty() {
            let typed_ids: Vec<_> = typed_references
                .iter()
                .map(|reference| reference.entity_id())
                .fold(Vec::new(), |mut ids, id| {
                    if !ids.contains(&id) {
                        ids.push(id);
                    }
                    ids
                });
            let raw_ids: Vec<_> = raw_references
                .iter()
                .map(|reference| reference.entity_id())
                .collect();
            if typed_ids != raw_ids {
                return Err(ProtocolError::invalid_argument(
                    "refs",
                    "raw Fact references must equal the payload-declared references",
                ));
            }
        }
        let references = if typed_references.is_empty() {
            raw_references
        } else {
            typed_references
        };
        let origins = self
            .authorize_fact_link_targets(
                authority,
                &draft.derived_from,
                "derived_from",
                session_visible,
                session_visible_kinds,
            )
            .await?;
        let references = self
            .authorize_fact_link_targets(
                authority,
                &references,
                "references",
                session_visible,
                session_visible_kinds,
            )
            .await?;
        Ok(AuthorizedNodeLinks::new(
            origins,
            references,
            payload_references,
        ))
    }

    async fn authorize_fact_link_targets<A>(
        &self,
        authority: &A,
        targets: &[EdgeEndpoint],
        field: &str,
        session_visible: &[MemoryId],
        session_visible_kinds: &[(MemoryId, EntityKind)],
    ) -> Result<Vec<EdgeEndpoint>, ProtocolError>
    where
        A: EngineAuthority + ?Sized,
    {
        let mut out: Vec<EdgeEndpoint> = Vec::with_capacity(targets.len());
        // Kind comparison is deferred out of the admission loop and grouped
        // under the permit owner that admitted each target: one kind load per
        // owner, not one per target. A Vec and not a map because the order a
        // caller met the owners is the order their mismatches surface.
        let mut deferred_kinds: Vec<(Owner, Vec<(MemoryId, EntityKind)>)> = Vec::new();
        for target in targets {
            // A repeated declaration names the same endpoint, and the first
            // occurrence already validated and admitted it. Skipping here and
            // not at the push below is what keeps a duplicate off the read
            // path: shape, layering and `authorize_entry_read` all run once.
            if out.contains(target) {
                continue;
            }
            target
                .validate_shape()
                .map_err(|err| ProtocolError::invalid_argument(field, err))?;
            if field == "derived_from" && matches!(target.entity, crate::EntityRef::Goal(_)) {
                return Err(ProtocolError::invalid_argument(
                    field,
                    "Fact origins must target a Memory",
                ));
            }
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
                crate::EntityRef::Memory(memory_id) if session_visible.contains(&memory_id) => {
                    let actual = session_visible_kinds
                        .iter()
                        .find_map(|(id, kind)| (*id == memory_id).then_some(*kind))
                        .ok_or_else(|| {
                            ProtocolError::internal(
                                "session-visible Fact reference is missing its stored kind",
                            )
                        })?;
                    if actual != target.kind {
                        return Err(ProtocolError::invalid_argument(
                            field,
                            format!(
                                "declared target kind {} does not match stored kind {}",
                                target.kind.as_str(),
                                actual.as_str()
                            ),
                        ));
                    }
                }
                crate::EntityRef::Memory(memory_id) => {
                    let permit = self
                        .authorize_entry_read(authority, crate::EntityId::Memory(memory_id))
                        .await?;
                    let owner = *permit.owner();
                    match deferred_kinds
                        .iter_mut()
                        .find(|(admitted, _)| *admitted == owner)
                    {
                        Some((_, declared)) => declared.push((memory_id, target.kind)),
                        None => deferred_kinds.push((owner, vec![(memory_id, target.kind)])),
                    }
                }
                crate::EntityRef::Goal(goal_id) => {
                    self.authorize_entry_read(authority, crate::EntityId::Goal(goal_id))
                        .await?;
                }
            }
            out.push(*target);
        }
        for (owner, declared) in &deferred_kinds {
            let memory_ids: Vec<MemoryId> = declared.iter().map(|(id, _)| *id).collect();
            let stored = self.load_required_memory_kinds(owner, &memory_ids).await?;
            for ((_, target_kind), actual) in declared.iter().zip(stored) {
                if actual != *target_kind {
                    return Err(ProtocolError::invalid_argument(
                        field,
                        format!(
                            "declared target kind {} does not match stored kind {}",
                            target_kind.as_str(),
                            actual.as_str()
                        ),
                    ));
                }
            }
        }
        Ok(out)
    }

    pub(in crate::engine) fn single_write_owner_for<A>(
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
    /// what the caller asked for: the caller's answer, unless the schema's
    /// recipe resolves to no embed unit.
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
    /// whose recipe resolves to no embed unit is written without a vector
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
        authorized
            .links()
            .validate_sidecar_references(sidecars)
            .map_err(|err| ProtocolError::invalid_argument("sidecars", err))?;
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
        authorized
            .links()
            .validate_sidecar_references(sidecars)
            .map_err(|err| ProtocolError::invalid_argument("sidecars", err))?;
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
        authorized
            .links()
            .validate_sidecar_references(sidecars)
            .map_err(|err| ProtocolError::invalid_argument("sidecars", err))?;
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
    /// A Fact whose schema resolves to no embed unit is left alone:
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
    /// up to the host-configured [`crate::EmbeddingRuntimePolicy::batch_size`]
    /// texts per provider call. Direct core hosts can install the policy with
    /// [`Engine::with_embedding_runtime_policy`].
    ///
    /// Failure semantics:
    /// - a *transient* batch failure (429/5xx/network) releases the claimed
    ///   jobs back to `pending` without burning retry attempts — a provider
    ///   outage is not evidence against any individual job — and ends the
    ///   drain call;
    /// - a batch failure whose liveness probe succeeds re-embeds the batch one
    ///   text at a time to isolate the content-attributed input(s); an
    ///   over-limit input is rescued by bisecting it into chunked embeddings
    ///   (full coverage), jobs rejected at every length go terminal instead of
    ///   cycling reject-retry forever, and their batch-mates still embed;
    ///   when the probe fails, all jobs remain retryable.
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
        let policy = self.embedding_runtime_policy();
        let mut outcome = EmbeddingDrainOutcome::default();
        let mut remaining = limit;
        while remaining > 0 {
            let take = i64::try_from(remaining.min(policy.batch_size()))
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
            let _heartbeat = spawn_embedding_claim_heartbeat(
                self.storage.ingest.embedding_job.clone(),
                claims.clone(),
                policy.claim_heartbeat_interval(),
            );

            // Jobs whose memory no longer yields embeddable text are
            // complete as-is; only texted jobs go to the provider.
            // Also excluded here, not just at enqueue: a job queued
            // before its schema stopped resolving an embed unit completes
            // as a no-op instead of embedding what now declines a vector.
            let items: Vec<(Owner, EntityKind, MemoryId)> = claims
                .iter()
                .map(|claim| (claim.owner, claim.entity_kind, claim.entity_id))
                .collect();
            let texts = self
                .storage
                .ingest
                .embedding_text
                .load_embedding_texts(&items, self.registry().non_embeddable_schema_ids())
                .await?;
            let mut batch: Vec<(EmbeddingJobClaim, String)> = Vec::with_capacity(claims.len());
            for (claim, text) in claims.into_iter().zip(texts) {
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
                Ok(vectors) if vectors.len() != batch.len() => {
                    self.release_malformed_embedding_batch(batch, vectors.len())
                        .await?;
                    break;
                }
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

    async fn release_malformed_embedding_batch(
        &self,
        batch: Vec<(EmbeddingJobClaim, String)>,
        received: usize,
    ) -> Result<(), StorageError> {
        let error = format!(
            "embedding batch cardinality mismatch: sent {} texts but received {received} vectors",
            batch.len(),
        );
        tracing::warn!(
            sent = batch.len(),
            received,
            "embedding provider returned malformed batch cardinality"
        );
        let claims: Vec<EmbeddingJobClaim> = batch.into_iter().map(|(claim, _)| claim).collect();
        self.storage
            .ingest
            .embedding_job
            .release_embedding_jobs(&claims, &error)
            .await
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
                crate::storage_ports::EmbeddingWriteProof::for_claim(claim),
            )
            .await?;
        self.storage
            .ingest
            .embedding_job
            .complete_embedding_job(claim)
            .await?;
        Ok(true)
    }

    /// Per-item fallback after a live-provider batch rejection: isolate which
    /// inputs the provider rejects. A rejected input is bisected into
    /// provider-acceptable chunks ([`crate::llm::embed_in_chunks_after_failure`])
    /// — over-limit inputs stay fully semantically findable through chunked
    /// embeddings instead of going invisible. An ambiguous per-item failure
    /// is eligible only after its own liveness probe succeeds.
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
                Err(err) => {
                    let initial_error = err.to_string();
                    match crate::llm::embed_in_chunks_after_failure(client.as_ref(), &text, err)
                        .await
                    {
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
                                    &format!("embed memory text: {initial_error}"),
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
                crate::storage_ports::EmbeddingWriteProof::for_claim(claim),
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
    /// `limit: None` uses [`crate::EMBEDDING_RECONCILE_DEFAULT_LIMIT`].
    pub async fn reconcile_embeddings(
        &self,
        scope: crate::EmbeddingReconcileScope,
        limit: Option<i64>,
    ) -> Result<crate::EmbeddingReconcileOutcome, StorageError> {
        let Some(client) = self.embed_client() else {
            return Ok(crate::EmbeddingReconcileOutcome::default());
        };
        self.storage
            .owner_inverse
            .embedding_maintenance
            .reconcile_embeddings(
                crate::EmbeddingReconcileOptions {
                    model_id: client.model_id(),
                    scope,
                    limit: Some(limit.unwrap_or(crate::EMBEDDING_RECONCILE_DEFAULT_LIMIT)),
                    non_embeddable_schemas: self.registry().non_embeddable_schema_ids(),
                },
                self.embedding_runtime_policy(),
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
    /// The write itself is the ordinary governed typed-Fact path —
    /// [`Self::authorize_fact_ingest`] then
    /// [`Self::ingest_fact_with_typed_sidecar`] — and deliberately not a
    /// verb of its own. The admission row therefore declares
    /// `proxima_core.mcp_call_logged_v1` in `sidecar_tables` and the typed
    /// row lands through the frozen sidecar registry; that row is the ONLY
    /// thing [`Self::read_mcp_call_history`] reads, so a second write path
    /// that skipped it would log calls into an unreadable history.
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
        // The gate above is what rejects a foreign log Owner. The Fact path
        // below resolves the owner from the context instead of taking one,
        // so it is handed a context narrowed to the owner just authorized —
        // the same shape the core `record_utterance` tool writes through.
        let scoped = authz
            .clone()
            .narrowed_to_owner(input.owner)
            .ok_or_else(|| {
                ProtocolError::forbidden("mcp call log owner is not writable by this context")
            })?;

        let receipt_id = input.receipt_id();
        let payload = input.payload();
        let mut draft = FactWriteCommand::from_payload(
            MCP_CALL_SOURCE_ID,
            SourceBatchId::new(uuid::Uuid::now_v7()),
            &payload,
            input.observed_at,
        )
        .occurred_at(input.occurred_at)
        // Content-addressed I/O citation: the same request/response bytes
        // under one Owner share one cited object, whatever else differs.
        .with_citation(CitationSpec::v1(
            MCP_CALL_IO_SCHEMA,
            input.io_content_hash(),
            MCP_CALL_CITATION_SCHEMA,
        ));
        // Whole-verb replay key. `from_payload` digests the payload alone,
        // which would collapse two identical calls made at different times
        // into one Fact; `McpCallLogInput::receipt_id` folds the timestamps
        // as well, which is the documented idempotency of this verb.
        draft.ingest_key = Some(hex::encode(receipt_id.into_inner()));

        let sidecars = [SidecarPayload::fact(payload)];
        let authorized = self
            .authorize_fact_ingest(&scoped, Relation::Ingest, draft, &sidecars)
            .await?;
        let embed_client = self.embed_client();
        let requested = embed_client.as_ref().map(|client| client.model_id());
        let outcome = self
            .ingest_fact_with_typed_sidecar(&authorized, &sidecars, requested)
            .await?;
        Ok(McpCallLogOutcome {
            receipt_id,
            fact_memory_id: outcome.memory_id,
            cited_object_id: outcome.cited_object_id,
            change_event_seq: outcome.change_event_seq,
            idempotent_replay: outcome.idempotent_replay,
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::engine::access_sets::tests::MembershipStorage;
    use crate::error::ErrorCode;
    use crate::ids::UserId;
    use crate::llm::{EMBEDDING_DIM, LlmError};
    use crate::{
        AuthPath, FactPayload, FlavorRegistry, GroupId, PayloadKeyBuilder, PayloadReference,
        ReferenceBinding, SchemaId,
    };
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

    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct ReferencedTestFact {
        fact_id: String,
        targets: Vec<EdgeEndpoint>,
    }

    impl FactPayload for ReferencedTestFact {
        const SCHEMA_ID: &'static str = "test/ingest-referenced-fact";
        const SCHEMA_VERSION: u32 = 1;

        fn receipt_key(&self) -> Vec<u8> {
            let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
            key.field_str("fact_id", &self.fact_id);
            key.finish()
        }

        fn render(&self) -> String {
            self.fact_id.clone()
        }

        fn references(&self) -> Vec<PayloadReference> {
            self.targets
                .iter()
                .copied()
                .map(|target| PayloadReference {
                    field: "target",
                    binding: ReferenceBinding::Pin,
                    target,
                })
                .collect()
        }
    }

    fn referenced_draft(payload: &ReferencedTestFact) -> FactWriteCommand {
        FactWriteCommand::from_payload(
            "test/references",
            SourceBatchId::new(uuid::Uuid::now_v7()),
            payload,
            time::OffsetDateTime::now_utc(),
        )
    }

    fn reference_engine(
        owner: Owner,
        home_owner: Option<Owner>,
        entity_readable: bool,
        memory_kind: Option<EntityKind>,
        observed_fact_writes: Arc<AtomicUsize>,
    ) -> Engine {
        Engine::compose_or_panic_for_tests(
            MembershipStorage {
                member: owner,
                group: GroupId::new(uuid::Uuid::now_v7()),
                membership_relation: Relation::Viewer,
                home_owner,
                entity_readable,
                memory_kind,
                goal_evidence: None,
                observed_fact_writes,
                observed_modify_evidence: Arc::new(std::sync::Mutex::new(None)),
                observed_goal_authorship: Arc::new(std::sync::Mutex::new(Vec::new())),
            }
            .storage_ports(),
            FlavorRegistry::add_fact_schema_or_panic_for_tests::<ReferencedTestFact>,
        )
    }

    #[tokio::test]
    async fn fact_payload_refs_are_the_authorized_links() {
        let owner = test_owner();
        let fact = MemoryId::new(uuid::Uuid::now_v7());
        let goal = crate::GoalId::new(uuid::Uuid::now_v7());
        let payload = ReferencedTestFact {
            fact_id: "typed-links".to_owned(),
            targets: vec![
                EdgeEndpoint::memory(EntityKind::Fact, fact),
                EdgeEndpoint::goal(goal),
                EdgeEndpoint::memory(EntityKind::Fact, fact),
            ],
        };
        let sidecars = [SidecarPayload::fact(payload.clone())];
        let engine = reference_engine(
            owner,
            Some(owner),
            true,
            Some(EntityKind::Fact),
            Arc::new(AtomicUsize::new(0)),
        );
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);

        let authorized = engine
            .authorize_fact_ingest(
                &authz,
                Relation::Ingest,
                referenced_draft(&payload),
                &sidecars,
            )
            .await
            .expect("readable typed targets should authorize");

        assert_eq!(
            authorized.links().references(),
            &[
                EdgeEndpoint::memory(EntityKind::Fact, fact),
                EdgeEndpoint::goal(goal),
            ]
        );
    }

    #[tokio::test]
    async fn many_readable_fact_references_batch_their_kind_load() {
        let owner = test_owner();
        let first = MemoryId::new(uuid::Uuid::now_v7());
        let second = MemoryId::new(uuid::Uuid::now_v7());
        let third = MemoryId::new(uuid::Uuid::now_v7());
        let payload = ReferencedTestFact {
            fact_id: "batched-kinds".to_owned(),
            targets: vec![
                EdgeEndpoint::memory(EntityKind::Fact, first),
                EdgeEndpoint::memory(EntityKind::Fact, second),
                EdgeEndpoint::memory(EntityKind::Fact, first),
                EdgeEndpoint::memory(EntityKind::Fact, third),
            ],
        };
        let sidecars = [SidecarPayload::fact(payload.clone())];
        let engine = reference_engine(
            owner,
            Some(owner),
            true,
            Some(EntityKind::Fact),
            Arc::new(AtomicUsize::new(0)),
        );
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);

        let authorized = engine
            .authorize_fact_ingest(
                &authz,
                Relation::Ingest,
                referenced_draft(&payload),
                &sidecars,
            )
            .await
            .expect("readable targets of the declared kind should authorize");

        assert_eq!(
            authorized.links().references(),
            &[
                EdgeEndpoint::memory(EntityKind::Fact, first),
                EdgeEndpoint::memory(EntityKind::Fact, second),
                EdgeEndpoint::memory(EntityKind::Fact, third),
            ]
        );
    }

    #[tokio::test]
    async fn raw_fact_refs_cannot_disagree_with_payload_refs() {
        let owner = test_owner();
        let typed = MemoryId::new(uuid::Uuid::now_v7());
        let raw = MemoryId::new(uuid::Uuid::now_v7());
        let payload = ReferencedTestFact {
            fact_id: "raw-mismatch".to_owned(),
            targets: vec![EdgeEndpoint::memory(EntityKind::Fact, typed)],
        };
        let sidecars = [SidecarPayload::fact(payload.clone())];
        let engine = reference_engine(
            owner,
            Some(owner),
            true,
            Some(EntityKind::Fact),
            Arc::new(AtomicUsize::new(0)),
        );
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);

        let error = engine
            .authorize_fact_ingest(
                &authz,
                Relation::Ingest,
                referenced_draft(&payload).with_refs(vec![raw.into_inner()]),
                &sidecars,
            )
            .await
            .expect_err("raw refs must not replace typed declarations");

        assert_eq!(error.code, ErrorCode::InvalidArgument);
    }

    #[tokio::test]
    async fn authorized_sidecars_cannot_change_reference_declaration() {
        let owner = test_owner();
        let first = MemoryId::new(uuid::Uuid::now_v7());
        let second = MemoryId::new(uuid::Uuid::now_v7());
        let admitted = ReferencedTestFact {
            fact_id: "bound-sidecars".to_owned(),
            targets: vec![EdgeEndpoint::memory(EntityKind::Fact, first)],
        };
        let substituted = ReferencedTestFact {
            fact_id: "bound-sidecars".to_owned(),
            targets: vec![EdgeEndpoint::memory(EntityKind::Fact, second)],
        };
        let admitted_sidecars = [SidecarPayload::fact(admitted.clone())];
        let observed = Arc::new(AtomicUsize::new(0));
        let engine = reference_engine(
            owner,
            Some(owner),
            true,
            Some(EntityKind::Fact),
            observed.clone(),
        );
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        let authorized = engine
            .authorize_fact_ingest(
                &authz,
                Relation::Ingest,
                referenced_draft(&admitted),
                &admitted_sidecars,
            )
            .await
            .expect("the original declaration should authorize");

        let error = engine
            .ingest_fact_with_typed_sidecar(&authorized, &[SidecarPayload::fact(substituted)], None)
            .await
            .expect_err("a substituted declaration must fail before the port");

        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert_eq!(observed.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn fact_reference_wrong_stored_kind_stops_before_port() {
        let owner = test_owner();
        let target = MemoryId::new(uuid::Uuid::now_v7());
        let payload = ReferencedTestFact {
            fact_id: "wrong-kind".to_owned(),
            targets: Vec::new(),
        };
        let observed = Arc::new(AtomicUsize::new(0));
        let engine = reference_engine(
            owner,
            Some(owner),
            true,
            Some(EntityKind::Abstraction),
            observed.clone(),
        );
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);

        let error = engine
            .fact_ingest(
                &authz,
                referenced_draft(&payload).with_refs(vec![target.into_inner()]),
            )
            .await
            .expect_err("a raw Fact endpoint must match the stored kind");

        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert_eq!(observed.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn fact_reference_unreadable_stops_before_port() {
        let owner = test_owner();
        let target = MemoryId::new(uuid::Uuid::now_v7());
        let payload = ReferencedTestFact {
            fact_id: "unreadable".to_owned(),
            targets: Vec::new(),
        };
        let observed = Arc::new(AtomicUsize::new(0));
        let engine = reference_engine(
            owner,
            Some(owner),
            false,
            Some(EntityKind::Fact),
            observed.clone(),
        );
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);

        let error = engine
            .fact_ingest(
                &authz,
                referenced_draft(&payload).with_refs(vec![target.into_inner()]),
            )
            .await
            .expect_err("an unreadable target must fail before persistence");

        assert_eq!(error.code, ErrorCode::Forbidden);
        assert_eq!(observed.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn uow_session_visible_fact_reference_checks_kind() {
        let owner = test_owner();
        let target = MemoryId::new(uuid::Uuid::now_v7());
        let payload = ReferencedTestFact {
            fact_id: "session-kind".to_owned(),
            targets: vec![EdgeEndpoint::memory(EntityKind::Fact, target)],
        };
        let sidecars = [SidecarPayload::fact(payload.clone())];
        let engine = reference_engine(owner, None, false, None, Arc::new(AtomicUsize::new(0)));
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);

        let error = engine
            .authorize_fact_ingest_visible(
                &authz,
                Relation::Ingest,
                referenced_draft(&payload),
                &sidecars,
                &[target],
                &[(target, EntityKind::Abstraction)],
            )
            .await
            .expect_err("session-visible kind mismatch must fail closed");

        assert_eq!(error.code, ErrorCode::InvalidArgument);
    }

    #[tokio::test]
    async fn non_fact_source_cannot_mint_authorized_fact_write() {
        let owner = test_owner();
        let payload = ReferencedTestFact {
            fact_id: "wrong-source".to_owned(),
            targets: Vec::new(),
        };
        let observed = Arc::new(AtomicUsize::new(0));
        let engine = reference_engine(owner, None, false, None, observed.clone());
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        let mut draft = referenced_draft(&payload);
        "abstraction".clone_into(&mut draft.kind);

        let error = engine
            .fact_ingest(&authz, draft)
            .await
            .expect_err("a non-Fact source must not mint a Fact witness");

        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert_eq!(observed.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn malformed_goal_endpoint_stops_before_port() {
        let owner = test_owner();
        let payload = ReferencedTestFact {
            fact_id: "malformed-goal".to_owned(),
            targets: Vec::new(),
        };
        let observed = Arc::new(AtomicUsize::new(0));
        let engine = reference_engine(owner, None, false, None, observed.clone());
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        let malformed = EdgeEndpoint {
            kind: EntityKind::Fact,
            entity: crate::EntityRef::Goal(crate::GoalId::new(uuid::Uuid::now_v7())),
        };

        let error = engine
            .fact_ingest(
                &authz,
                referenced_draft(&payload).with_derived_from(vec![malformed]),
            )
            .await
            .expect_err("a malformed Goal endpoint must fail before persistence");

        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert_eq!(observed.load(Ordering::Relaxed), 0);
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
    async fn denied_context_precedes_invalid_fact_kind() {
        let owner = test_owner();
        let mut registry = FlavorRegistry::new();
        registry.add_fact_schema_or_panic_for_tests::<TestFact>();
        let engine = Engine::new(registry.freeze_or_panic_for_tests());
        let mut draft = FactWriteCommand::from_payload(
            "test/source",
            SourceBatchId::new(uuid::Uuid::now_v7()),
            &TestFact {
                fact_id: "fact-1".to_owned(),
            },
            time::OffsetDateTime::now_utc(),
        );
        "abstraction".clone_into(&mut draft.kind);

        let error = engine
            .authorize_fact_ingest(
                &AuthzContext::denied_for_owner(&owner),
                Relation::Editor,
                draft,
                &[],
            )
            .await
            .expect_err("authorization must fail before validating the supplied kind");

        assert_eq!(error.code, ErrorCode::Forbidden);
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

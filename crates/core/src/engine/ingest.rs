use super::Engine;
use crate::SchemaVersion;
use crate::access::Relation;
use crate::authz::{AuthzContext, EngineAuthority};
use crate::edge::EdgeEndpoint;
use crate::error::ProtocolError;
use crate::llm::EmbeddingSpace;
use crate::verbs::fact_ingest::{
    AuthorizedCitationAttachment, AuthorizedFactCore, AuthorizedFactWithCitation,
    AuthorizedFactWithCitationRef, AuthorizedFactWrite, AuthorizedInlineCitationMapping,
    AuthorizedInlineCitedObject, AuthorizedNodeLinks, CitationAttachmentRequest, CitationSpec,
    FactIngestOutcome, FactWriteCommand, InlineCitationMappingDraft, InlineCitedObjectDraft,
};
use crate::verbs::persist_mcp_call::{
    MCP_CALL_CITATION_SCHEMA, MCP_CALL_IO_SCHEMA, MCP_CALL_SOURCE_ID, McpCallLogInput,
    McpCallLogOutcome,
};
use crate::verbs::schema::{PayloadKind, ProtocolPayload, SchemaInfo};
use crate::{EntityKind, MemoryId, Owner, OwnerRef, SidecarPayload};

/// Capture only the values which can select a natural series; body replay semantics are unchanged.
fn bind_fact_natural_key(
    draft: &FactWriteCommand,
    columns: &[String],
    sidecars: &[SidecarPayload],
) -> Result<Option<Vec<(String, crate::verbs::query::SidecarAtom)>>, ProtocolError> {
    if columns.is_empty() || draft.handle.is_some() {
        return Ok(None);
    }
    let mut matches = sidecars.iter().filter(|payload| {
        payload.kind == PayloadKind::Fact
            && payload.schema_id == draft.schema_id
            && payload.schema_version == draft.schema_version
    });
    let Some(payload) = matches.next() else {
        // An ownerless raw command can be authorized without a typed payload;
        // automatic NK selection later refuses a missing binding.
        return Ok(None);
    };
    if matches.next().is_some() {
        return Err(ProtocolError::invalid_argument(
            "sidecars",
            "natural-key binding requires exactly one matching typed Fact payload",
        ));
    }
    let json = payload
        .to_protocol_json()
        .map_err(|err| ProtocolError::invalid_argument("sidecars", err))?;
    let columns = columns.iter().map(String::as_str).collect::<Vec<_>>();
    crate::verbs::query::SidecarAtom::bind_columns(&json, &columns)
        .map(Some)
        .map_err(|err| ProtocolError::invalid_argument("natural_key", err))
}

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

impl Engine {
    /// docs/14 §"`FactIngest`" — Owner-scoped write. Validates
    /// schemas and delegates to storage.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context cannot resolve exactly one writable owner or
    /// lacks [`Relation::Ingest`] on that owner space; `UnknownSchema` when the
    /// Fact schema or provided citation schemas are not registered.
    /// Caller-fixable storage rejections (concurrent citation) surface as
    /// `InvalidArgument`; infrastructure faults as `Internal`.
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
        let embedding_spaces = self
            .fact_embedding_spaces(
                authorized.owner_write_permit().owner(),
                authorized.draft().schema_id.as_str(),
            )
            .await?;
        let outcome = self
            .storage
            .ingest
            .fact_ingest
            .ingest_authorized_fact_atomic(&authorized, &embedding_spaces)
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
        draft: FactWriteCommand,
        sidecars: &[SidecarPayload],
        session_visible: &[MemoryId],
        session_visible_kinds: &[(MemoryId, EntityKind)],
    ) -> Result<AuthorizedFactWrite, ProtocolError>
    where
        A: EngineAuthority + ?Sized,
    {
        let owner = self.single_write_owner_for(authority, relation)?;
        let permit = self.authorize_write(authority, &owner, relation).await?;
        self.authorize_fact_ingest_permitted_visible(
            authority,
            permit,
            draft,
            sidecars,
            session_visible,
            session_visible_kinds,
        )
        .await
    }

    /// Shared normalization for the ownerless protocol and owner-explicit typed API.
    pub(in crate::engine) async fn authorize_fact_ingest_permitted_visible<
        A: EngineAuthority + ?Sized,
    >(
        &self,
        authority: &A,
        permit: super::pipeline::WritePermit,
        mut draft: FactWriteCommand,
        sidecars: &[SidecarPayload],
        session_visible: &[MemoryId],
        session_visible_kinds: &[(MemoryId, EntityKind)],
    ) -> Result<AuthorizedFactWrite, ProtocolError> {
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
        let natural_key_values =
            bind_fact_natural_key(&draft, &fact_natural_key_columns, sidecars)?;
        let publication =
            self.resolve_publication(authority, fact_info, *permit.owner(), sidecars)?;
        Ok(AuthorizedFactWrite::new(
            AuthorizedFactCore::new(
                permit,
                draft,
                fact_sidecar_table,
                fact_natural_key_columns,
                sidecars.to_vec(),
                links,
            )
            .with_natural_key_values(natural_key_values)
            .with_publication(publication),
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

        let natural_key_values =
            bind_fact_natural_key(&draft, &fact_natural_key_columns, sidecars)?;
        let publication =
            self.resolve_publication(authority, fact_info, *permit.owner(), sidecars)?;
        Ok(AuthorizedFactWithCitation::new(
            AuthorizedFactCore::new(
                permit,
                draft,
                fact_sidecar_table,
                fact_natural_key_columns,
                sidecars.to_vec(),
                links,
            )
            .with_natural_key_values(natural_key_values)
            .with_publication(publication),
            cited_object,
            mapping,
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

        let natural_key_values =
            bind_fact_natural_key(&draft, &fact_natural_key_columns, sidecars)?;
        let publication =
            self.resolve_publication(authority, fact_info, *permit.owner(), sidecars)?;
        Ok(AuthorizedFactWithCitationRef::new(
            AuthorizedFactCore::new(
                permit,
                draft,
                fact_sidecar_table,
                fact_natural_key_columns,
                sidecars.to_vec(),
                links,
            )
            .with_natural_key_values(natural_key_values)
            .with_publication(publication),
            cited_object_id,
            expected_object_schema,
            mapping,
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
        let additional = self
            .authorize_fact_link_targets(
                authority,
                &draft.additional_references,
                "additional_references",
                session_visible,
                session_visible_kinds,
            )
            .await?;
        let mut references = self
            .authorize_fact_link_targets(
                authority,
                &references,
                "references",
                session_visible,
                session_visible_kinds,
            )
            .await?;
        for target in additional {
            if !references.contains(&target) {
                references.push(target);
            }
        }
        Ok(AuthorizedNodeLinks::new(Vec::new(), references))
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
            let stored = self
                .load_required_memory_kinds(
                    self.operation_authority(authority)?.authz().owner_scope(),
                    owner,
                    &memory_ids,
                )
                .await?;
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

    /// The embedding spaces a new Fact of `schema_id` owned by `owner` is
    /// queued for: the spaces `owner`'s route names, unless the schema's
    /// recipe resolves to no embed unit.
    ///
    /// APPLIED HERE, NOT AT THE CALL SITES, because every typed Fact
    /// write in the process funnels through one of the four verbs that
    /// persist one — `fact_ingest` above, plus the three below — and none
    /// of them should have to remember. Count them when adding a fifth:
    /// `fact_ingest` was missed for a release because it does not share
    /// the `ingest_fact_*` name. Callers do not name a space at all: the
    /// engine owns which client embeds, so no verb can queue a Fact for a
    /// space the engine would not embed.
    ///
    /// Storage would be the lower boundary, and cannot host this: the
    /// answer lives in the flavor registry, which storage does not hold.
    ///
    /// # Errors
    ///
    /// `Internal` when the host cannot route `owner`: the write fails
    /// rather than landing without the job that would make it searchable.
    pub(in crate::engine) async fn fact_embedding_spaces(
        &self,
        owner: &Owner,
        schema_id: &str,
    ) -> Result<Vec<EmbeddingSpace>, ProtocolError> {
        if !self.registry().schema_is_embeddable(schema_id) {
            return Ok(Vec::new());
        }
        Ok(self.write_route(owner).await?.write_spaces())
    }

    /// Persist an already-authorized typed-sidecar Fact ingest, queued for
    /// the engine's embedding space unless the schema declines a vector.
    ///
    /// # Errors
    ///
    /// Returns caller-fixable storage rejections (concurrent citation) as
    /// `InvalidArgument` and infrastructure faults as `Internal`.
    pub async fn ingest_fact_with_typed_sidecar(
        &self,
        authorized: &AuthorizedFactWrite,
    ) -> Result<FactIngestOutcome, ProtocolError> {
        self.validate_write_permit(authorized.owner_write_permit())?;
        let embedding_spaces = self
            .fact_embedding_spaces(
                authorized.owner_write_permit().owner(),
                authorized.draft().schema_id.as_str(),
            )
            .await?;
        self.storage()
            .ingest
            .fact_ingest
            .ingest_fact_with_typed_sidecar(authorized, &embedding_spaces)
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
    /// Returns caller-fixable storage rejections (concurrent citation) as
    /// `InvalidArgument` and infrastructure faults as `Internal`.
    pub async fn ingest_fact_with_citation_and_typed_sidecar(
        &self,
        authorized: &AuthorizedFactWithCitation,
    ) -> Result<FactIngestOutcome, ProtocolError> {
        self.validate_write_permit(authorized.owner_write_permit())?;
        let embedding_spaces = self
            .fact_embedding_spaces(
                authorized.owner_write_permit().owner(),
                authorized.draft().schema_id.as_str(),
            )
            .await?;
        self.storage()
            .ingest
            .fact_ingest
            .ingest_fact_with_citation_and_typed_sidecar(authorized, &embedding_spaces)
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
    /// Returns caller-fixable storage rejections (missing or foreign
    /// cited object, mapping-target mismatch) as `InvalidArgument` and
    /// infrastructure faults as `Internal`.
    pub async fn ingest_fact_with_citation_ref_and_typed_sidecar(
        &self,
        authorized: &AuthorizedFactWithCitationRef,
    ) -> Result<FactIngestOutcome, ProtocolError> {
        self.validate_write_permit(authorized.owner_write_permit())?;
        let embedding_spaces = self
            .fact_embedding_spaces(
                authorized.owner_write_permit().owner(),
                authorized.draft().schema_id.as_str(),
            )
            .await?;
        self.storage()
            .ingest
            .fact_ingest
            .ingest_fact_with_citation_ref_and_typed_sidecar(authorized, &embedding_spaces)
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
    pub async fn authorize_citation_attachment(
        &self,
        authz: &AuthzContext,
        relation: Relation,
        requested_owner: OwnerRef,
        request: CitationAttachmentRequest,
    ) -> Result<AuthorizedCitationAttachment, ProtocolError> {
        let CitationAttachmentRequest {
            memory_id,
            memory_kind,
            cited_object,
            mapping,
        } = request;
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
            permit,
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

    /// Resolve the publication plan a Fact admission must capture, or
    /// `None` when its schema is not listenable.
    ///
    /// Everything the envelope needs that is NOT the `t` storage mints is
    /// bound here, at authorization time, in the one place that holds all
    /// three inputs: the frozen schema declaration, the deployment's
    /// configured source, and the authenticated edge's trusted model
    /// identity. In particular the model label is
    /// [`crate::AuthzContext::trusted_model_id`] and never the caller's
    /// own `model_id` argument, which is a claim rather than a credential.
    ///
    /// A listenable schema written through a route that carries no typed
    /// payload is REFUSED here, before any write: the export snapshot IS
    /// the payload's serde JSON, and a receipt-only admission has none.
    ///
    /// The deployment's [`PublicationLimits`] ride along in the returned
    /// plan. They are read from THIS engine's [`PublicationConfig`] and
    /// travel with the draft all the way to the seal, so the ceiling a
    /// host configured is necessarily the ceiling capture enforces.
    ///
    /// [`PublicationLimits`]: crate::publication::PublicationLimits
    /// [`PublicationConfig`]: crate::publication::PublicationConfig
    fn resolve_publication<A>(
        &self,
        authority: &A,
        fact_info: &SchemaInfo,
        owner: Owner,
        sidecars: &[SidecarPayload],
    ) -> Result<Option<crate::publication::PublicationPlan>, ProtocolError>
    where
        A: EngineAuthority + ?Sized,
    {
        if !fact_info.listenable {
            return Ok(None);
        }
        let schema_id = fact_info.schema_id.clone();
        let Some(source) = self.publication.source.clone() else {
            return Err(super::errors::map_publication_error(
                &crate::publication::PublicationError::SourceUnbound {
                    schema_id: schema_id.as_str().to_owned(),
                },
            ));
        };
        let mut matches = sidecars.iter().filter(|payload| {
            payload.kind == PayloadKind::Fact
                && payload.schema_id == schema_id
                && payload.schema_version == fact_info.schema_version
        });
        let Some(payload) = matches.next() else {
            return Err(super::errors::map_publication_error(
                &crate::publication::PublicationError::UntypedListenableWrite {
                    schema_id: schema_id.as_str().to_owned(),
                },
            ));
        };
        if matches.next().is_some() {
            return Err(super::errors::map_publication_error(
                &crate::publication::PublicationError::ExportFailed(format!(
                    "listenable schema {} was supplied twice in one admission",
                    schema_id.as_str()
                )),
            ));
        }
        let data = payload.to_protocol_json().map_err(|err| {
            super::errors::map_publication_error(
                &crate::publication::PublicationError::ExportFailed(err),
            )
        })?;
        let operation = self.operation_authority(authority)?;
        let authz = operation.authz();
        let model_id = authz.trusted_model_id().map(ToOwned::to_owned);
        // Host-bound attributes ride the SAME authorization context as the
        // trusted model id: nothing the payload carries can reach them.
        let extensions = authz.publication_extensions().clone();
        Ok(Some(crate::publication::PublicationPlan::new(
            crate::publication::PublicationDraft::new(
                schema_id,
                fact_info.schema_version,
                source,
                owner,
                model_id,
                extensions,
                data,
            ),
            self.publication.limits,
        )))
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
        let mut draft =
            FactWriteCommand::from_payload(MCP_CALL_SOURCE_ID, &payload, input.observed_at)
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
        let outcome = self.ingest_fact_with_typed_sidecar(&authorized).await?;
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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::engine::access_sets::tests::MembershipStorage;
    use crate::error::ErrorCode;
    use crate::ids::UserId;
    use crate::{
        AuthPath, FactPayload, FlavorRegistry, GroupId, PayloadKeyBuilder, PayloadReference,
        ReferenceBinding, SchemaId,
    };
    use serde::{Deserialize, Serialize};

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
        FactWriteCommand::from_payload("test/references", payload, time::OffsetDateTime::now_utc())
    }

    fn reference_engine(
        owner: Owner,
        home_owner: Option<Owner>,
        entity_readable: bool,
        memory_kind: Option<EntityKind>,
        observed_fact_writes: Arc<AtomicUsize>,
    ) -> Engine {
        observed_reference_engine(
            owner,
            home_owner,
            entity_readable,
            memory_kind,
            observed_fact_writes,
        )
        .0
    }

    /// `reference_engine` plus the two admission-path observations: the
    /// entities handed to `visible_home_owner` and the id batch of each
    /// `load_memory_kinds` call. Round-trip counts are a contract of this
    /// path, so a test can assert them rather than infer them from behaviour.
    #[allow(clippy::type_complexity)]
    fn observed_reference_engine(
        owner: Owner,
        home_owner: Option<Owner>,
        entity_readable: bool,
        memory_kind: Option<EntityKind>,
        observed_fact_writes: Arc<AtomicUsize>,
    ) -> (
        Engine,
        Arc<std::sync::Mutex<Vec<crate::EntityId>>>,
        Arc<std::sync::Mutex<Vec<Vec<MemoryId>>>>,
    ) {
        let observed_entity_reads = Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed_kind_loads = Arc::new(std::sync::Mutex::new(Vec::new()));
        let engine = Engine::compose_or_panic_for_tests(
            MembershipStorage {
                observed_entity_reads: observed_entity_reads.clone(),
                observed_kind_loads: observed_kind_loads.clone(),
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
        );
        (engine, observed_entity_reads, observed_kind_loads)
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
    async fn duplicate_additional_refs_are_admitted_once_and_batch_their_kind_load() {
        let owner = test_owner();
        let first = MemoryId::new(uuid::Uuid::now_v7());
        let second = MemoryId::new(uuid::Uuid::now_v7());
        let third = MemoryId::new(uuid::Uuid::now_v7());
        // `additional_references` reaches the admission loop exactly as the caller
        // wrote it — unlike payload references, which `authorize_fact_node_links`
        // folds before it delegates — so this is the path a duplicate can
        // actually reach.
        let payload = ReferencedTestFact {
            fact_id: "duplicate-refs".to_owned(),
            targets: Vec::new(),
        };
        let sidecars = [SidecarPayload::fact(payload.clone())];
        let (engine, entity_reads, kind_loads) = observed_reference_engine(
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
                referenced_draft(&payload).with_additional_references(vec![
                    EdgeEndpoint::memory(EntityKind::Fact, first),
                    EdgeEndpoint::memory(EntityKind::Fact, second),
                    EdgeEndpoint::memory(EntityKind::Fact, first),
                    EdgeEndpoint::memory(EntityKind::Fact, third),
                ]),
                &sidecars,
            )
            .await
            .expect("readable references of the declared kind should authorize");

        assert!(authorized.links().origins().is_empty());
        assert_eq!(
            authorized.links().references(),
            &[
                EdgeEndpoint::memory(EntityKind::Fact, first),
                EdgeEndpoint::memory(EntityKind::Fact, second),
                EdgeEndpoint::memory(EntityKind::Fact, third),
            ]
        );
        // Four declarations, three distinct endpoints: the repeat is admitted
        // once, so it costs one entry read rather than two.
        assert_eq!(
            entity_reads.lock().expect("entity reads").as_slice(),
            &[
                crate::EntityId::Memory(first),
                crate::EntityId::Memory(second),
                crate::EntityId::Memory(third),
            ]
        );
        // One owner, so exactly one kind load carrying every distinct target,
        // rather than one load per declaration.
        assert_eq!(
            kind_loads.lock().expect("kind loads").as_slice(),
            &[vec![first, second, third]]
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

        let bound = authorized
            .sidecar_payloads()
            .first()
            .expect("authorization binds the typed Fact sidecar")
            .references();
        assert_eq!(
            bound[0].target,
            EdgeEndpoint::memory(EntityKind::Fact, first)
        );
        assert_ne!(
            bound[0].target,
            EdgeEndpoint::memory(EntityKind::Fact, second)
        );
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
                referenced_draft(&payload).with_additional_references(vec![malformed]),
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

    // ── publication resolution (review R6) ──────────────────────────────

    /// An engine over the probe registry with a bound deployment source —
    /// the only shape in which a listenable schema can be admitted at all.
    fn listenable_engine() -> Engine {
        let source = crate::publication::PublicationSource::new("urn:proxima:r6-tests")
            .expect("a URN is an absolute source");
        Engine::new(crate::test_fixtures::probe_registry())
            .try_with_publication_config(crate::publication::PublicationConfig::new(source))
            .expect("a bound source boots")
    }

    fn listenable_probe(note: &str) -> crate::test_fixtures::ListenableProbeV1 {
        crate::test_fixtures::ListenableProbeV1 {
            probe_id: uuid::Uuid::now_v7(),
            note: note.to_owned(),
        }
    }

    fn listenable_draft(payload: &crate::test_fixtures::ListenableProbeV1) -> FactWriteCommand {
        FactWriteCommand::from_payload("probe/source", payload, time::OffsetDateTime::now_utc())
    }

    /// The capture is assembled from the DEPLOYMENT and the PERMIT, never
    /// from the draft: the source is the configured installation identity,
    /// the owner is the one authorization resolved, and the model label is
    /// the one the authenticated edge certified.
    #[tokio::test]
    async fn a_listenable_admission_captures_the_deployment_source_permit_owner_and_trusted_model()
    {
        let owner = test_owner();
        let engine = listenable_engine();
        let payload = listenable_probe("the quay is sound");
        let sidecars = [SidecarPayload::fact(payload.clone())];
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer)
            .with_trusted_model_id("runner/pinned")
            .expect("a valid operator label");

        let authorized = engine
            .authorize_fact_ingest(
                &authz,
                Relation::Ingest,
                listenable_draft(&payload),
                &sidecars,
            )
            .await
            .expect("a typed listenable write authorizes");

        let plan = authorized
            .publication()
            .expect("a listenable schema carries a capture plan");
        assert_eq!(plan.draft.source.as_str(), "urn:proxima:r6-tests");
        assert_eq!(
            plan.draft.owner, owner,
            "the permit's owner, not the draft's"
        );
        assert_eq!(plan.draft.model_id.as_deref(), Some("runner/pinned"));
        assert_eq!(plan.draft.schema_id.as_str(), "probe/listenable-v1");
        assert_eq!(
            plan.draft.data,
            serde_json::to_value(&payload).expect("the probe serializes"),
            "the export snapshot is the typed payload's own serde form"
        );
        assert_eq!(plan.limits, engine.publication_config().limits);
    }

    /// No certified model identity means no model identity on the wire.
    ///
    /// The only value `resolve_publication` reads is
    /// `AuthzContext::trusted_model_id`. The caller-supplied `model_id`
    /// label lives on [`crate::tool::ToolCaller`] and is never consulted
    /// here, so an unauthenticated deployment publishes `None` rather than
    /// whatever a caller wrote in its arguments.
    #[tokio::test]
    async fn an_uncertified_edge_captures_no_model_identity() {
        let owner = test_owner();
        let engine = listenable_engine();
        let payload = listenable_probe("no certified runner");
        let sidecars = [SidecarPayload::fact(payload.clone())];
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        assert_eq!(authz.trusted_model_id(), None);

        let authorized = engine
            .authorize_fact_ingest(
                &authz,
                Relation::Ingest,
                listenable_draft(&payload),
                &sidecars,
            )
            .await
            .expect("a typed listenable write authorizes");

        assert_eq!(
            authorized
                .publication()
                .expect("still captured")
                .draft
                .model_id,
            None
        );
    }

    /// A listenable Fact written through the untyped receipt-only route has
    /// no typed payload to export, so there is nothing to publish. Refused
    /// rather than published empty: a consumer that received an event with
    /// no data could not tell it from a bug.
    #[tokio::test]
    async fn an_untyped_listenable_write_is_refused() {
        let owner = test_owner();
        let engine = listenable_engine();
        let payload = listenable_probe("no sidecar");
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);

        let err = engine
            .authorize_fact_ingest(&authz, Relation::Ingest, listenable_draft(&payload), &[])
            .await
            .expect_err("a listenable schema without its typed payload must refuse");

        assert_eq!(err.code, ErrorCode::InvalidArgument);
        assert!(
            err.message.contains("probe/listenable-v1"),
            "the refusal names the schema: {}",
            err.message
        );
    }

    /// One admission, one event. Two typed payloads of the same listenable
    /// schema give the capture no way to choose, and choosing the first
    /// would silently drop the second.
    #[tokio::test]
    async fn the_same_listenable_schema_supplied_twice_is_refused() {
        let owner = test_owner();
        let engine = listenable_engine();
        let payload = listenable_probe("twice");
        let sidecars = [
            SidecarPayload::fact(payload.clone()),
            SidecarPayload::fact(listenable_probe("and again")),
        ];
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);

        let err = engine
            .authorize_fact_ingest(
                &authz,
                Relation::Ingest,
                listenable_draft(&payload),
                &sidecars,
            )
            .await
            .expect_err("two payloads of one listenable schema must refuse");

        assert!(
            err.message.contains("supplied twice"),
            "unexpected message: {}",
            err.message
        );
    }
}

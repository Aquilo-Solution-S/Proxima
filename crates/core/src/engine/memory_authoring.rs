use super::Engine;
use crate::MAX_MEMORY_HYDRATION_BATCH;
use crate::access::Relation;
use crate::authz::EngineAuthority;
use crate::edge::{EdgeEndpoint, validate_edge_layering};
use crate::error::ProtocolError;
use crate::storage::{
    AuthorDerivedOutcome, AuthorDerivedRequest, DerivedEmbedding, MemoryHydrationBatchOutcome,
    MemoryHydrationOutcome, StorageError,
};
#[cfg(test)]
use crate::storage_ports::OwnerWritePermit;
use crate::{
    AbstractionPayload, EntityId, EntityKind, InputContractId, MemoryId, MemoryOperatorKind,
    OperatorId, Owner, OwnerRef, PerspectivePayload, SchemaId, SchemaVersion, SidecarPayload,
};
use crate::{MemoryOutputInvocation, OperatorInvocationManifest, OutputEdgeManifest};

/// Owned embedding so a prepared batch can outlive the client borrow
/// used by [`DerivedEmbedding`].
pub(super) enum PreparedEmbedding {
    None,
    Ready { model_id: String, vector: Vec<f32> },
    Deferred { model_id: String },
}

impl PreparedEmbedding {
    pub(super) fn as_derived(&self) -> DerivedEmbedding<'_> {
        match self {
            Self::None => DerivedEmbedding::None,
            Self::Ready { model_id, vector } => DerivedEmbedding::Ready {
                model_id,
                vector: vector.clone(),
            },
            Self::Deferred { model_id } => DerivedEmbedding::Deferred { model_id },
        }
    }
}

pub(super) struct PreparedDerived {
    pub(super) write_permit: super::pipeline::WritePermit,
    pub(super) owner: Owner,
    pub(super) memory_id: MemoryId,
    pub(super) kind: EntityKind,
    pub(super) text: String,
    pub(super) schema_id: SchemaId,
    pub(super) schema_version: SchemaVersion,
    pub(super) operator_kind: MemoryOperatorKind,
    pub(super) sidecar_payload: SidecarPayload,
    pub(super) supersedes: Option<MemoryId>,
    pub(super) lexical_language: Option<String>,
    pub(super) embedding: PreparedEmbedding,
    pub(super) origins: Vec<EdgeEndpoint>,
    pub(super) references: Vec<EdgeEndpoint>,
}

#[cfg(test)]
#[derive(Debug)]
struct RawDerivedTestRequest<'a> {
    pub memory_id: MemoryId,
    pub owner: Owner,
    pub kind: EntityKind,
    pub text: String,
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub operator_kind: MemoryOperatorKind,
    /// Named in the [`OperatorInvocationManifest`] this write's declared
    /// origins prove. Not persisted — no operator table exists.
    pub operator_id: OperatorId,
    /// Manifest input contract, same lifetime as `operator_id`.
    pub input_contract_id: InputContractId,
    pub sidecar_payload: SidecarPayload,
    /// What this memory was made from. Each entry becomes an `Origin`
    /// pin on this memory's own row, written in the same transaction.
    /// The writer names targets; it never names a kind.
    pub derived_from: &'a [EdgeEndpoint],
    /// Extra reference pins (write-act, visit). Merged with payload
    /// `references()`; empty for ordinary derives.
    pub extra_refs: &'a [MemoryId],
    /// Prior `t` of the series this write revises. The engine hands it to
    /// storage, which appends this write to the prior row's `handle` — a
    /// later `t` on the same series is what supersession is.
    pub supersedes: Option<MemoryId>,
    /// Text-search configuration to stamp on the derived row, resolved
    /// by [`crate::lexical_language::resolve_lexical_language`].
    /// [`crate::lexical_language::LEXICAL_LANGUAGE_DEPLOYMENT_DEFAULT`]
    /// asks for the deployment's configuration; `None` states no
    /// language, which a `LanguagePolicy::PerRow` schema refuses.
    pub lexical_language: Option<&'a str>,
}

#[cfg(test)]
impl RawDerivedTestRequest<'_> {
    pub(super) fn into_typed(self) -> Result<DerivedMemory, ProtocolError> {
        let origins = self
            .derived_from
            .iter()
            .map(|endpoint| {
                endpoint.memory_id().ok_or_else(|| {
                    ProtocolError::invalid_argument(
                        "origins",
                        "derivation origins must name memory rows; Goals are not knowledge",
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(DerivedMemory {
            target: self.supersedes.map_or(
                MemoryTarget::Series(SeriesHandle::new(self.memory_id.into_inner())),
                MemoryTarget::Revision,
            ),
            owner: self.owner,
            text: self.text,
            sidecar_payload: self.sidecar_payload,
            identity: (!origins.is_empty()).then_some(DerivationIdentity::new(
                self.operator_id,
                self.input_contract_id,
            )),
            origins,
            extra_refs: self.extra_refs.to_vec(),
            lexical_language: self.lexical_language.map(str::to_owned),
        })
    }
}

impl PreparedDerived {
    pub(super) fn storage_request(&self) -> AuthorDerivedRequest<'_> {
        AuthorDerivedRequest {
            memory_id: self.memory_id,
            owner: self.owner,
            kind: self.kind,
            text: self.text.clone(),
            schema_id: self.schema_id.clone(),
            schema_version: self.schema_version,
            operator_kind: self.operator_kind,
            sidecar_payload: self.sidecar_payload.clone(),
            supersedes: self.supersedes,
            lexical_language: self.lexical_language.as_deref(),
            embedding: self.embedding.as_derived(),
            origins: &self.origins,
            references: &self.references,
        }
    }
}

impl From<AuthorDerivedOutcome> for DerivedMemoryOutcome {
    fn from(outcome: AuthorDerivedOutcome) -> Self {
        Self {
            memory_id: outcome.memory_id,
            idempotent_replay: outcome.idempotent_replay,
            edge_count: outcome.edge_count,
            embedding_deferred: outcome.embedding_deferred,
        }
    }
}

fn infer_derivation_phase(
    kind: EntityKind,
    origins: &[EdgeEndpoint],
) -> Result<MemoryOperatorKind, ProtocolError> {
    let first = origins.first().map(|origin| origin.kind);
    if origins.iter().any(|origin| Some(origin.kind) != first) {
        return Err(ProtocolError::invalid_argument(
            "origins",
            "origins must be all Facts or all Abstractions; mixed derivations are not supported",
        ));
    }
    match (kind, first) {
        (EntityKind::Abstraction, Some(EntityKind::Fact)) => Ok(MemoryOperatorKind::FtoA),
        (EntityKind::Abstraction, Some(EntityKind::Abstraction)) => Ok(MemoryOperatorKind::AtoA),
        (EntityKind::Perspective, Some(EntityKind::Abstraction) | None) => {
            Ok(MemoryOperatorKind::AtoP)
        }
        _ => Err(ProtocolError::invalid_argument(
            "origins",
            "an Abstraction needs Fact or Abstraction origins; a derived Perspective needs Abstraction origins",
        )),
    }
}

fn validate_typed_invocation(
    memory: &DerivedMemory,
    output: MemoryId,
    kind: EntityKind,
    operator_kind: MemoryOperatorKind,
    origins: &[EdgeEndpoint],
) -> Result<(), StorageError> {
    if origins.is_empty() {
        return Ok(());
    }
    let identity = memory.identity.ok_or_else(|| {
        StorageError::ConstraintViolation(
            "derivation requires an operator invocation identity".into(),
        )
    })?;
    let manifest = OperatorInvocationManifest::memory_output(MemoryOutputInvocation {
        phase: operator_kind.phase(),
        operator_id: identity.operator_id,
        input_contract_id: identity.input_contract_id,
        inputs: origins
            .iter()
            .map(|endpoint| (MemoryId::new(endpoint.entity_id()), endpoint.kind))
            .collect(),
        output_memory_id: output,
        output_kind: kind,
        schema_id: memory.sidecar_payload.schema_id.clone(),
        schema_version: memory.sidecar_payload.schema_version,
        output_edges: origins
            .iter()
            .filter_map(|origin| origin.memory_id())
            .map(|id| OutputEdgeManifest::memory_to_memory(output, id))
            .collect(),
    });
    manifest
        .validate()
        .map_err(|err| StorageError::ConstraintViolation(err.to_string()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Result of a typed derived-memory write.
pub struct DerivedMemoryOutcome {
    /// Persisted version t. Use this ID for reads, origins, and references.
    pub memory_id: MemoryId,
    pub idempotent_replay: bool,
    /// Pins declared by this write (`origins` + `refs`). A count, not a
    /// list of handles: a pin has no id to hand back, and re-running the
    /// write re-asserts the same column values.
    pub edge_count: usize,
    /// The memory landed with no vector and a pending embedding job; it is
    /// lexically findable and semantically invisible until a drain runs.
    /// Callers that need it searchable immediately can see that here rather
    /// than by reading logs.
    pub embedding_deferred: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
/// Stable series key. Storage allocates a separate `MemoryId` for each admitted version.
pub struct SeriesHandle(uuid::Uuid);
impl SeriesHandle {
    #[must_use]
    /// Wrap an existing deterministic key without changing its bytes.
    pub const fn new(inner: uuid::Uuid) -> Self {
        Self(inner)
    }
    #[must_use]
    pub const fn into_inner(self) -> uuid::Uuid {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Choose a new conclusion or a new version of an existing conclusion.
pub enum MemoryTarget {
    /// Create a series if absent; otherwise address that series. Matching
    /// owner/kind/schema/origins/refs replay the head; changed origins append a
    /// version. With unchanged origins, changed refs are a conflict.
    Series(SeriesHandle),
    /// Append to the prior row's series. Repeating a revision appends another version;
    /// it has no separate persisted request key. Origins remain required for derivations.
    Revision(MemoryId),
}

#[derive(Debug, Clone, Copy)]
/// Identity of the operator and its input contract in the invocation witness.
pub struct DerivationIdentity {
    pub operator_id: OperatorId,
    pub input_contract_id: InputContractId,
}
impl DerivationIdentity {
    #[must_use]
    pub const fn new(operator_id: OperatorId, input_contract_id: InputContractId) -> Self {
        Self {
            operator_id,
            input_contract_id,
        }
    }
}

#[derive(Debug, Clone)]
/// A typed conclusion or interpretation; the engine resolves source kinds and authority.
/// Payload kind, schema, and provenance cannot be overwritten by consumers.
///
/// ```compile_fail
/// fn tamper(request: &mut proxima_core::DerivedMemory) {
///     request.origins.clear();
///     let _ = &mut request.sidecar_payload;
/// }
/// ```
pub struct DerivedMemory {
    pub(crate) target: MemoryTarget,
    pub(crate) owner: OwnerRef,
    pub(crate) text: String,
    pub(crate) sidecar_payload: SidecarPayload,
    pub(crate) origins: Vec<MemoryId>,
    pub(crate) identity: Option<DerivationIdentity>,
    pub(crate) extra_refs: Vec<MemoryId>,
    pub(crate) lexical_language: Option<String>,
}

impl DerivedMemory {
    fn output_kind(&self) -> Result<EntityKind, ProtocolError> {
        match self.sidecar_payload.kind {
            crate::verbs::schema::PayloadKind::Abstraction => Ok(EntityKind::Abstraction),
            crate::verbs::schema::PayloadKind::Perspective => Ok(EntityKind::Perspective),
            _ => Err(ProtocolError::invalid_argument(
                "payload",
                "derived memory requires an Abstraction or Perspective payload",
            )),
        }
    }
    fn origins(
        origins: impl IntoIterator<Item = MemoryId>,
    ) -> Result<Vec<MemoryId>, ProtocolError> {
        let mut out = Vec::new();
        for id in origins {
            if !out.contains(&id) {
                out.push(id);
            }
        }
        if out.is_empty() {
            return Err(ProtocolError::invalid_argument(
                "origins",
                "derived memory requires at least one origin; revisions also need provenance",
            ));
        }
        Ok(out)
    }
    /// Create a conclusion from Facts or prior Abstractions. Source kinds are resolved by the engine.
    ///
    /// # Errors
    /// Rejects empty origins; revisions also require provenance.
    pub fn abstraction<P: AbstractionPayload>(
        target: MemoryTarget,
        owner: OwnerRef,
        text: impl Into<String>,
        payload: P,
        origins: impl IntoIterator<Item = MemoryId>,
        identity: DerivationIdentity,
    ) -> Result<Self, ProtocolError> {
        Ok(Self {
            target,
            owner,
            text: text.into(),
            sidecar_payload: SidecarPayload::abstraction(payload),
            origins: Self::origins(origins)?,
            identity: Some(identity),
            extra_refs: Vec::new(),
            lexical_language: Some(
                crate::lexical_language::LEXICAL_LANGUAGE_DEPLOYMENT_DEFAULT.to_owned(),
            ),
        })
    }
    /// Create a Perspective derived from Abstractions.
    ///
    /// # Errors
    /// Rejects empty origins. Use `interpretation` for a judgment grounded only through references.
    pub fn perspective<P: PerspectivePayload>(
        target: MemoryTarget,
        owner: OwnerRef,
        text: impl Into<String>,
        payload: P,
        origins: impl IntoIterator<Item = MemoryId>,
        identity: DerivationIdentity,
    ) -> Result<Self, ProtocolError> {
        Ok(Self {
            target,
            owner,
            text: text.into(),
            sidecar_payload: SidecarPayload::perspective(payload),
            origins: Self::origins(origins)?,
            identity: Some(identity),
            extra_refs: Vec::new(),
            lexical_language: Some(
                crate::lexical_language::LEXICAL_LANGUAGE_DEPLOYMENT_DEFAULT.to_owned(),
            ),
        })
    }
    /// Create an origin-free Perspective grounded through its payload references.
    #[must_use]
    pub fn interpretation<P: PerspectivePayload>(
        target: MemoryTarget,
        owner: OwnerRef,
        text: impl Into<String>,
        payload: P,
    ) -> Self {
        Self {
            target,
            owner,
            text: text.into(),
            sidecar_payload: SidecarPayload::perspective(payload),
            origins: Vec::new(),
            identity: None,
            extra_refs: Vec::new(),
            lexical_language: Some(
                crate::lexical_language::LEXICAL_LANGUAGE_DEPLOYMENT_DEFAULT.to_owned(),
            ),
        }
    }
    /// Add memory reference pins, distinct from derivation origins. Duplicates are removed.
    #[must_use]
    pub fn refs(mut self, refs: impl IntoIterator<Item = MemoryId>) -> Self {
        self.extra_refs.extend(refs);
        self
    }
    #[must_use]
    pub fn lexical_language(mut self, language: impl Into<String>) -> Self {
        self.lexical_language = Some(language.into());
        self
    }
}

impl Engine {
    /// Resolve actual kinds after entry-read authorization; one kind read per home owner.
    pub(in crate::engine) async fn resolve_memory_targets<A>(
        &self,
        authority: &A,
        ids: &[MemoryId],
        session_kinds: &[(MemoryId, EntityKind)],
    ) -> Result<Vec<EdgeEndpoint>, ProtocolError>
    where
        A: EngineAuthority + ?Sized,
    {
        let mut resolved = std::collections::HashMap::new();
        let mut groups: Vec<(Owner, Vec<MemoryId>)> = Vec::new();
        let mut unique = Vec::new();
        for id in ids {
            if unique.contains(id) {
                continue;
            }
            unique.push(*id);
            // A UoW holds one immutable authority. Successful writes imply reads
            // of their actual kind (Role enforces write ceiling <= read ceiling).
            if let Some((_, kind)) = session_kinds.iter().find(|(seen, _)| seen == id) {
                resolved.insert(*id, *kind);
                continue;
            }
            let permit = self
                .authorize_entry_read(authority, EntityId::Memory(*id))
                .await?;
            if let Some((_, group)) = groups.iter_mut().find(|(owner, _)| owner == permit.owner()) {
                group.push(*id);
            } else {
                groups.push((*permit.owner(), vec![*id]));
            }
        }
        for (owner, group) in groups {
            let kinds = self.load_required_memory_kinds(&owner, &group).await?;
            resolved.extend(group.into_iter().zip(kinds));
        }
        unique
            .into_iter()
            .map(|id| {
                resolved
                    .get(&id)
                    .copied()
                    .map(|kind| EdgeEndpoint::memory(kind, id))
                    .ok_or_else(|| {
                        ProtocolError::internal("authorized memory kind was not resolved")
                    })
            })
            .collect()
    }

    /// Cool one owned memory `t`. PUT cold first, then stub+delete hot.
    ///
    /// One-shot command-port: generic [`EngineAuthority`], including
    /// delegated workers. Multi-write forget lives on
    /// [`crate::UnitOfWork::forget`] (`AuthzContext` only). Both are
    /// legal; this is not a second flavor `Transaction`.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context lacks [`Relation::Editor`] on
    /// the owner, `NotFound` when `t` is absent, and storage errors from
    /// the forget transaction.
    pub async fn forget_memory<A>(
        &self,
        authority: &A,
        owner: Owner,
        memory_id: MemoryId,
    ) -> Result<(), ProtocolError>
    where
        A: EngineAuthority + ?Sized,
    {
        let write_permit = self
            .authorize_write(authority, &owner, Relation::Editor)
            .await?;
        self.storage()
            .memory_authoring
            .memory_authoring
            .forget_memory(write_permit.owner_write_permit(), memory_id)
            .await
            .map_err(|err| {
                super::errors::map_write_storage_error(err, "memory", "memory not found")
            })
    }

    /// Hydrate one owner-owned cooled Memory admission.
    ///
    /// The operation is owner-authorized with the same editor write gate as
    /// `forget_memory`; the storage tier receives only the sealed write
    /// permit. Missing and foreign ids collapse to `NotFound`, while cold
    /// object and integrity outcomes remain typed in the returned value.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context lacks [`Relation::Editor`] on the
    /// owner, `InvalidArgument` for storage configuration faults, and
    /// `Internal` for unavailable storage or an exhausted lifecycle retry.
    pub async fn hydrate_memory<A>(
        &self,
        authority: &A,
        owner: Owner,
        memory_id: MemoryId,
    ) -> Result<MemoryHydrationOutcome, ProtocolError>
    where
        A: EngineAuthority + ?Sized,
    {
        let outcome = self
            .hydrate_memories(authority, owner, std::slice::from_ref(&memory_id))
            .await?;
        outcome
            .outcomes
            .into_iter()
            .next()
            .ok_or_else(|| ProtocolError::internal("single-memory hydration returned no outcome"))
    }

    /// Hydrate a bounded owner-scoped set of cooled Memory admissions.
    ///
    /// Results are in request order. The storage transaction is atomic over
    /// owner-visible cooled items: if any cold object is missing, unsupported,
    /// or invalid, no cooled item is changed and otherwise-valid items are
    /// `NotAttempted`.
    /// `NotFound` covers both absent and foreign ids, preserving the
    /// non-disclosure rule used by owner-scoped reads. Duplicate ids and sets
    /// over [`MAX_MEMORY_HYDRATION_BATCH`] are rejected before storage.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context lacks [`Relation::Editor`] on the
    /// owner; `InvalidArgument` for an unbounded or duplicate request; and
    /// `Internal` for storage failures.
    pub async fn hydrate_memories<A>(
        &self,
        authority: &A,
        owner: Owner,
        memory_ids: &[MemoryId],
    ) -> Result<MemoryHydrationBatchOutcome, ProtocolError>
    where
        A: EngineAuthority + ?Sized,
    {
        if memory_ids.len() > MAX_MEMORY_HYDRATION_BATCH {
            return Err(ProtocolError::invalid_argument(
                "memory_ids",
                format!("at most {MAX_MEMORY_HYDRATION_BATCH} ids are allowed"),
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        if memory_ids
            .iter()
            .any(|memory_id| !seen.insert(memory_id.into_inner()))
        {
            return Err(ProtocolError::invalid_argument(
                "memory_ids",
                "duplicate memory ids are not allowed",
            ));
        }
        let write_permit = self
            .authorize_write(authority, &owner, Relation::Editor)
            .await?;
        let outcome = self
            .storage()
            .memory_authoring
            .memory_authoring
            .hydrate_memories(write_permit.owner_write_permit(), memory_ids)
            .await
            .map_err(|err| {
                super::errors::map_write_storage_error(err, "memory_ids", "memory not found")
            })?;
        if outcome.outcomes.len() != memory_ids.len() {
            return Err(ProtocolError::internal(
                "storage returned an incomplete hydration result",
            ));
        }
        Ok(outcome)
    }

    /// Authorized graph-write verb for agent-authored derived memory.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context lacks [`Relation::Editor`] on the
    /// source owner or read access to an edge target; `InvalidArgument` when
    /// referenced memories are absent or edge shape validation fails; and
    /// `Internal` for storage failures.
    pub async fn derive_memory<A>(
        &self,
        authority: &A,
        memory: DerivedMemory,
    ) -> Result<DerivedMemoryOutcome, ProtocolError>
    where
        A: EngineAuthority + ?Sized,
    {
        let item = self
            .prepare_derived_memory(authority, memory, &[], false)
            .await?;
        let req = item.storage_request();
        let outcome = self
            .storage()
            .memory_authoring
            .memory_authoring
            .author_derived(
                &req,
                item.write_permit.owner_write_permit(),
                crate::storage_ports::OperatorWriteProof::new(),
            )
            .await
            .map_err(map_derived_storage_error)?;
        Ok(outcome.into())
    }

    /// Shared authorization, provenance, references, and embedding preparation.
    pub(super) async fn prepare_derived_memory<A>(
        &self,
        authority: &A,
        memory: DerivedMemory,
        session_kinds: &[(MemoryId, EntityKind)],
        defer_embedding: bool,
    ) -> Result<PreparedDerived, ProtocolError>
    where
        A: EngineAuthority + ?Sized,
    {
        let write_permit = self
            .authorize_write(authority, &memory.owner, Relation::Editor)
            .await?;
        let kind = memory.output_kind()?;
        let (memory_id, supersedes) = match memory.target {
            MemoryTarget::Series(handle) => (MemoryId::new(handle.into_inner()), None),
            // Storage resolves the prior row's handle. This private draft key is
            // unused for series selection and must not pretend to be the prior t.
            MemoryTarget::Revision(prior) => (MemoryId::new(uuid::Uuid::now_v7()), Some(prior)),
        };
        if let Some(prior) = supersedes {
            self.validate_derived_revision(
                authority,
                *write_permit.owner(),
                kind,
                prior,
                session_kinds,
            )
            .await?;
        }
        let origins = self
            .resolve_memory_targets(authority, &memory.origins, session_kinds)
            .await?;
        let operator_kind = infer_derivation_phase(kind, &origins)?;
        // A new version's t is allocated by storage. The layer check needs only
        // the output kind; neither a handle nor the prior t is its source row.
        let source = EdgeEndpoint::memory(kind, MemoryId::new(uuid::Uuid::now_v7()));
        for origin in &origins {
            validate_edge_layering(source, *origin)
                .map_err(|err| ProtocolError::invalid_argument("origins", err))?;
        }
        let references = self
            .prepare_derived_references(authority, source, &memory, session_kinds)
            .await?;
        validate_typed_invocation(&memory, memory_id, kind, operator_kind, &origins)
            .map_err(map_derived_storage_error)?;
        let embedding = self
            .prepare_memory_embedding(
                memory_id,
                memory.sidecar_payload.schema_id.as_str(),
                &memory.text,
                defer_embedding,
            )
            .await
            .map_err(map_derived_storage_error)?;
        Ok(PreparedDerived {
            owner: *write_permit.owner(),
            write_permit,
            memory_id,
            kind,
            text: memory.text,
            schema_id: memory.sidecar_payload.schema_id.clone(),
            schema_version: memory.sidecar_payload.schema_version,
            operator_kind,
            sidecar_payload: memory.sidecar_payload,
            supersedes,
            lexical_language: memory.lexical_language,
            embedding,
            origins,
            references,
        })
    }

    async fn prepare_derived_references<A: EngineAuthority + ?Sized>(
        &self,
        authority: &A,
        source: EdgeEndpoint,
        memory: &DerivedMemory,
        session_kinds: &[(MemoryId, EntityKind)],
    ) -> Result<Vec<EdgeEndpoint>, ProtocolError> {
        let declared = memory.sidecar_payload.references();
        for reference in &declared {
            reference
                .validate()
                .map_err(|err| ProtocolError::invalid_argument("references", err))?;
        }
        let ids = declared
            .iter()
            .filter_map(|reference| reference.target.memory_id())
            .chain(memory.extra_refs.iter().copied())
            .collect::<Vec<_>>();
        let resolved = self
            .resolve_memory_targets(authority, &ids, session_kinds)
            .await?;
        let mut references = Vec::new();
        for reference in declared {
            let target = match reference.target.entity {
                crate::EntityRef::Memory(id) => {
                    let actual = resolved
                        .iter()
                        .find(|target| target.memory_id() == Some(id))
                        .ok_or_else(|| {
                            ProtocolError::internal("declared memory reference was not resolved")
                        })?;
                    if actual.kind != reference.target.kind {
                        return Err(ProtocolError::invalid_argument(
                            "references",
                            format!(
                                "reference {} declares {}, but the readable target is {}",
                                reference.field,
                                reference.target.kind.as_str(),
                                actual.kind.as_str(),
                            ),
                        ));
                    }
                    *actual
                }
                crate::EntityRef::Goal(id) => {
                    self.authorize_entry_read(authority, EntityId::Goal(id))
                        .await?;
                    EdgeEndpoint::goal(id)
                }
            };
            if !references.contains(&target) {
                references.push(target);
            }
        }
        for target in resolved {
            if !references.contains(&target) {
                references.push(target);
            }
        }
        for target in &references {
            validate_edge_layering(source, *target)
                .map_err(|err| ProtocolError::invalid_argument("refs", err))?;
        }
        Ok(references)
    }

    async fn validate_derived_revision<A: EngineAuthority + ?Sized>(
        &self,
        authority: &A,
        owner: Owner,
        kind: EntityKind,
        prior: MemoryId,
        session_kinds: &[(MemoryId, EntityKind)],
    ) -> Result<(), ProtocolError> {
        let prior_kind = if let Some((_, kind)) = session_kinds.iter().find(|(id, _)| *id == prior)
        {
            // The transaction's head check enforces same owner/schema as well.
            *kind
        } else {
            let permit = self
                .authorize_entry_read(authority, EntityId::Memory(prior))
                .await?;
            if permit.owner() != &owner {
                return Err(ProtocolError::forbidden(
                    "revision target must belong to the destination owner",
                ));
            }
            self.load_required_memory_kind(permit.owner(), prior)
                .await?
        };
        if prior_kind != kind {
            return Err(ProtocolError::invalid_argument(
                "target",
                "revision must retain the prior memory kind and schema",
            ));
        }
        Ok(())
    }

    async fn prepare_memory_embedding(
        &self,
        memory_id: MemoryId,
        schema_id: &str,
        text: &str,
        defer: bool,
    ) -> Result<PreparedEmbedding, StorageError> {
        let client = self.embed_client();
        let Some(client) = client
            .as_deref()
            .filter(|_| self.registry().schema_is_embeddable(schema_id))
        else {
            return Ok(PreparedEmbedding::None);
        };
        if defer {
            return Ok(PreparedEmbedding::Deferred {
                model_id: client.model_id().to_owned(),
            });
        }
        Ok(
            match resolve_derived_embedding(client, memory_id, text).await? {
                DerivedEmbedding::None => PreparedEmbedding::None,
                DerivedEmbedding::Ready { model_id, vector } => PreparedEmbedding::Ready {
                    model_id: model_id.to_owned(),
                    vector,
                },
                DerivedEmbedding::Deferred { model_id } => PreparedEmbedding::Deferred {
                    model_id: model_id.to_owned(),
                },
            },
        )
    }

    /// Author one derived Memory and its already-resolved edges. When an
    /// embedding client is configured AND the schema's recipe resolves to a
    /// unit, the Engine embeds before storage; otherwise storage receives
    /// [`DerivedEmbedding::None`] and persists no embedding row.
    ///
    /// An input this client cannot embed does not fail the write. A text
    /// refused whole is rescued by the drain's bisection and lands as
    /// [`DerivedEmbedding::Ready`] in the same transaction as the row —
    /// so a long unit costs no job and no second round trip. Storage keeps
    /// one vec per version, so the first piece is what is stored. A text
    /// rejected at every length lands with no vector and a pending
    /// embedding job enqueued in the same transaction
    /// ([`DerivedEmbedding::Deferred`]), so [`Engine::drain_embedding_jobs`]
    /// — which owns terminal failures and retries — picks it up. The
    /// alternative is what production hit: a derive phase that dies
    /// deterministically, forever, on one over-long section, discarding
    /// every model call already paid for upstream of it.
    ///
    /// A provider that is merely *down* is a different thing and still
    /// fails the write, so an outage cannot quietly mint a corpus of
    /// unembedded memories. [`crate::llm::embed_failure_blames_the_input`]
    /// is what separates the two.
    ///
    /// # Errors
    ///
    /// Returns `Internal` when the embedding client fails *and* the
    /// provider does not answer a liveness probe,
    /// `ConstraintViolation` on embedding dimension mismatch, and storage
    /// errors from the atomic write.
    ///
    /// Test-only adapter for recording the raw storage/proof boundary.
    #[cfg(test)]
    async fn author_derived(
        &self,
        permit: &OwnerWritePermit,
        req: RawDerivedTestRequest<'_>,
        references: &[EdgeEndpoint],
    ) -> Result<AuthorDerivedOutcome, StorageError> {
        validate_operator_memory_invocation_request(&req)?;
        // Bound outside the call: `DerivedEmbedding` borrows the client's
        // model id for the length of the storage request.
        let client = self.embed_client();
        let embedding = match client.as_deref() {
            Some(client) if self.registry().schema_is_embeddable(req.schema_id.as_str()) => {
                resolve_derived_embedding(client, req.memory_id, &req.text).await?
            }
            _ => DerivedEmbedding::None,
        };

        let storage_req = AuthorDerivedRequest {
            memory_id: req.memory_id,
            owner: req.owner,
            kind: req.kind,
            text: req.text,
            schema_id: req.schema_id,
            schema_version: req.schema_version,
            operator_kind: req.operator_kind,
            sidecar_payload: req.sidecar_payload,
            // Supersession is a later `t` on the same series, not an
            // edge and not a column: storage resolves the prior row's
            // handle and appends to it inside the same transaction.
            supersedes: req.supersedes,
            lexical_language: req.lexical_language,
            embedding,
            origins: req.derived_from,
            references,
        };
        self.storage()
            .memory_authoring
            .memory_authoring
            .author_derived(
                &storage_req,
                permit,
                crate::storage_ports::OperatorWriteProof::new(),
            )
            .await
    }

    pub(in crate::engine) async fn load_required_memory_kind(
        &self,
        owner: &Owner,
        memory_id: MemoryId,
    ) -> Result<EntityKind, ProtocolError> {
        let mut kinds = self.load_required_memory_kinds(owner, &[memory_id]).await?;
        Ok(kinds.remove(0))
    }

    pub(in crate::engine) async fn load_required_memory_kinds(
        &self,
        owner: &Owner,
        memory_ids: &[MemoryId],
    ) -> Result<Vec<EntityKind>, ProtocolError> {
        if memory_ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = self
            .storage()
            .memory_authoring
            .memory_authoring
            .load_memory_kinds(owner, memory_ids)
            .await
            .map_err(|err| ProtocolError::internal(err.to_string()))?;
        let by_id = rows
            .into_iter()
            .map(|row| (row.memory_id, row.kind))
            .collect::<std::collections::HashMap<_, _>>();
        memory_ids
            .iter()
            .map(|memory_id| {
                by_id.get(memory_id).copied().ok_or_else(|| {
                    ProtocolError::invalid_argument(
                        "memory_id",
                        "authorized memory is unavailable; refresh its row ID and retry",
                    )
                })
            })
            .collect()
    }
}

/// Decide what a derived write should do about its vector, given a
/// configured embedding client.
///
/// A text this client refuses whole is bisected into pieces it accepts
/// ([`crate::llm::embed_in_chunks_after_failure`], the drain's rescue) —
/// but only after a liveness probe, because an outage says nothing about
/// the text. An over-limit text is routine for a corpus of long units, so
/// it is rescued inline rather than refused now and rescued by a job
/// later. Storage keeps one vec per version, so a successful rescue lands
/// as [`DerivedEmbedding::Ready`] with the first piece. Only a text
/// rejected at every length, or a rescue that fails midway, downgrades
/// the write to a job.
///
/// # Errors
///
/// `ConstraintViolation` when a vector's length disagrees with the
/// client's declared `dim` (a misconfiguration, never the input's fault,
/// so it is not deferrable), and `Internal` when the provider fails and
/// does not answer a liveness probe.
pub(in crate::engine) async fn resolve_derived_embedding<'client>(
    client: &'client dyn crate::llm::EmbeddingClient,
    memory_id: MemoryId,
    text: &str,
) -> Result<DerivedEmbedding<'client>, StorageError> {
    let err = match client.embed(text).await {
        Ok(vector) => {
            ensure_derived_embedding_dim(client, std::slice::from_ref(&vector))?;
            return Ok(DerivedEmbedding::Ready {
                model_id: client.model_id(),
                vector,
            });
        }
        Err(err) if crate::llm::embed_failure_blames_the_input(client, &err).await => err,
        Err(err) => {
            return Err(StorageError::Internal(format!(
                "embed derived memory text: {err}"
            )));
        }
    };
    let refusal = err.to_string();
    match crate::llm::embed_in_chunks_after_failure(client, text, err).await {
        Ok(Some(vectors)) => {
            let Some(vector) = vectors.into_iter().next() else {
                return Err(StorageError::ConstraintViolation(
                    "embedding version needs at least one chunk".into(),
                ));
            };
            ensure_derived_embedding_dim(client, std::slice::from_ref(&vector))?;
            tracing::info!(
                memory_id = ?memory_id,
                text_bytes = text.len(),
                "over-limit derived memory text embedded inline"
            );
            Ok(DerivedEmbedding::Ready {
                model_id: client.model_id(),
                vector,
            })
        }
        Ok(None) => {
            tracing::warn!(
                error = %refusal,
                memory_id = ?memory_id,
                text_bytes = text.len(),
                "derived memory text refused by a live embedding provider at every length; \
                 writing the memory without a vector and enqueueing an embedding job"
            );
            Ok(DerivedEmbedding::Deferred {
                model_id: client.model_id(),
            })
        }
        Err(rescue_err) => {
            tracing::warn!(
                error = %refusal,
                rescue_error = %rescue_err,
                memory_id = ?memory_id,
                text_bytes = text.len(),
                "derived memory text refused by a live embedding provider and the chunked \
                 rescue failed; writing the memory without a vector and enqueueing an \
                 embedding job"
            );
            Ok(DerivedEmbedding::Deferred {
                model_id: client.model_id(),
            })
        }
    }
}

/// The inline write's dim check, shared by the whole-text and chunked arms
/// and the same one the drain applies before its chunk insert.
fn ensure_derived_embedding_dim(
    client: &dyn crate::llm::EmbeddingClient,
    vectors: &[Vec<f32>],
) -> Result<(), StorageError> {
    if vectors.is_empty() || vectors.iter().any(|vector| vector.len() != client.dim()) {
        return Err(StorageError::ConstraintViolation(format!(
            "embedding dim mismatch: client dim {} but got {} vector(s) of lens {:?}",
            client.dim(),
            vectors.len(),
            vectors.iter().map(Vec::len).collect::<Vec<_>>(),
        )));
    }
    Ok(())
}

#[cfg(test)]
fn validate_operator_memory_invocation_request(
    req: &RawDerivedTestRequest<'_>,
) -> Result<(), StorageError> {
    // The operator manifest proves a *derivation*: output kind, input
    // kinds, and one origin row per declared input. A write that declares
    // no origins has no derivation to prove — an interpretation
    // Perspective, for instance, grounds through the references its
    // payload carries, not through inputs it consumed — so there is no
    // manifest, rather than an empty one that would fail its own
    // nonempty-inputs obligation.
    if req.derived_from.is_empty() {
        return Ok(());
    }

    let mut inputs = Vec::with_capacity(req.derived_from.len());
    let mut output_edges = Vec::with_capacity(req.derived_from.len());
    for origin in req.derived_from {
        let Some(target_memory_id) = origin.memory_id() else {
            return Err(StorageError::ConstraintViolation(
                "an operator provenance origin must name a memory row".into(),
            ));
        };
        inputs.push((target_memory_id, origin.kind));
        output_edges.push(OutputEdgeManifest::memory_to_memory(
            req.memory_id,
            target_memory_id,
        ));
    }
    let manifest = OperatorInvocationManifest::memory_output(MemoryOutputInvocation {
        phase: req.operator_kind.phase(),
        operator_id: req.operator_id,
        input_contract_id: req.input_contract_id,
        inputs,
        output_memory_id: req.memory_id,
        output_kind: req.kind,
        schema_id: req.schema_id.clone(),
        schema_version: req.schema_version,
        output_edges,
    });
    manifest
        .validate()
        .map_err(|err| StorageError::ConstraintViolation(err.to_string()))
}

/// Maps `author_derived`'s raw storage error onto the public `ProtocolError`
/// surface. `ConstraintViolation`/`Conflict` are caller-fixable (a
/// malformed operator invocation manifest, an idempotent-replay proof
/// mismatch) and must surface as `InvalidArgument`, not `Internal`.
pub(in crate::engine) fn map_derived_storage_error(err: StorageError) -> ProtocolError {
    super::errors::map_write_storage_error(
        err,
        "operator_invocation",
        "operator invocation referenced row not found",
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::authz::AuthzContext;
    use crate::error::ErrorCode;
    use crate::{
        AbstractionPayload, AgentDerivationV1, EntityKind, FlavorRegistry, OwnerRef, UserId,
    };

    use super::*;

    fn owner() -> Owner {
        OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()))
    }

    fn engine() -> Engine {
        Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests())
    }

    fn derivation_sidecar() -> SidecarPayload {
        SidecarPayload::abstraction(AgentDerivationV1 {
            title: "Derived".into(),
            body: "Body".into(),
            tags: Vec::new(),
            idempotency_key: None,
            source_memory_ids: Vec::new(),
            model_id: "test-model".into(),
            client_name: "test".into(),
            client_version: "1".into(),
        })
    }

    fn request(owner: Owner, derived_from: &[EdgeEndpoint]) -> RawDerivedTestRequest<'_> {
        RawDerivedTestRequest {
            memory_id: MemoryId::new(uuid::Uuid::now_v7()),
            owner,
            kind: EntityKind::Abstraction,
            text: "body".into(),
            schema_id: SchemaId::new(<AgentDerivationV1 as AbstractionPayload>::SCHEMA_ID.into()),
            schema_version: SchemaVersion::new(
                <AgentDerivationV1 as AbstractionPayload>::SCHEMA_VERSION,
            ),
            operator_kind: MemoryOperatorKind::FtoA,
            operator_id: OperatorId::new(uuid::Uuid::now_v7()),
            input_contract_id: InputContractId::new(uuid::Uuid::now_v7()),
            sidecar_payload: derivation_sidecar(),
            derived_from,
            extra_refs: &[],
            supersedes: None,
            lexical_language: None,
        }
    }

    /// An F→A derive that names its inputs carries a well-formed
    /// manifest and reaches storage. There is no batch input left on the
    /// request for a validator to demand: the derived-write API declares
    /// origins and nothing else about the operator run.
    #[tokio::test]
    async fn author_derived_ftoa_with_origins_passes_manifest_validation() {
        let engine = engine();
        let owner = owner();
        let permit = OwnerWritePermit::new(owner, crate::access::AccessKind::Perspective);
        let origins = [EdgeEndpoint::memory(
            EntityKind::Fact,
            MemoryId::new(uuid::Uuid::now_v7()),
        )];
        let err = engine
            .author_derived(&permit, request(owner, &origins), &[])
            .await
            .expect_err("the fake storage port refuses every write");
        assert!(
            !matches!(&err, StorageError::ConstraintViolation(_)),
            "a well-formed F→A manifest must reach storage: {err}"
        );
    }

    /// A write that declares no derivation has no operator invocation to
    /// prove — the manifest's nonempty-inputs obligation is about
    /// derivations, and an interpretation Perspective is not one. The
    /// write proceeds to storage (which the test engine refuses) rather
    /// than being rejected as a malformed manifest.
    #[tokio::test]
    async fn a_write_with_no_origins_carries_no_operator_manifest() {
        let engine = engine();
        let owner = owner();
        let permit = OwnerWritePermit::new(owner, crate::access::AccessKind::Perspective);
        let mut req = request(owner, &[]);
        req.operator_kind = MemoryOperatorKind::AtoP;
        req.kind = EntityKind::Perspective;
        let err = engine
            .author_derived(&permit, req, &[])
            .await
            .expect_err("the fake storage port refuses every write");
        assert!(
            !matches!(&err, StorageError::ConstraintViolation(msg) if msg.contains("inputs")),
            "an origin-free write must not be judged as an operator invocation: {err}"
        );
    }

    #[tokio::test]
    async fn derive_memory_denies_denied_context() {
        let engine = engine();
        let owner = owner();
        let err = engine
            .derive_memory(
                &AuthzContext::denied_for_owner(&owner),
                request(owner, &[]).into_typed().expect("synthetic request"),
            )
            .await
            .expect_err("denied context must fail before storage");

        assert_eq!(err.code, ErrorCode::Forbidden);
    }

    #[test]
    fn kind_resolution_reuses_the_authorized_home_owner() {
        let source = include_str!("memory_authoring.rs");
        let start = source
            .find("async fn resolve_memory_targets")
            .expect("kind resolver");
        let end = source[start..]
            .find("/// Cool one owned memory")
            .expect("end of resolver");
        let body = &source[start..start + end];
        let reload = format!("{}{}", ".home_", "owner(");
        assert!(
            !body.contains(&reload),
            "target kind lookup must reuse the authorized home owner, not query an unscoped owner again"
        );
        assert!(
            body.contains("authorize_entry_read"),
            "committed targets pass the read gate before their kind is loaded"
        );
        assert!(
            body.contains("permit.owner()"),
            "kind lookup uses the owner returned by the read permit"
        );
    }

    /// Public origin declarations carry row IDs, not caller-selected kinds.
    #[test]
    fn no_public_write_input_accepts_an_edge_kind() {
        let source = MemoryId::new(uuid::Uuid::now_v7());
        let req = DerivedMemory::abstraction(
            MemoryTarget::Series(SeriesHandle::new(uuid::Uuid::now_v7())),
            owner(),
            "conclusion",
            AgentDerivationV1 {
                title: "conclusion".into(),
                body: "body".into(),
                tags: Vec::new(),
                idempotency_key: None,
                source_memory_ids: vec![source.into_inner()],
                model_id: "fixture".into(),
                client_name: "fixture".into(),
                client_version: "1".into(),
            },
            [source, source],
            DerivationIdentity::new(
                OperatorId::new(uuid::Uuid::now_v7()),
                InputContractId::new(uuid::Uuid::now_v7()),
            ),
        )
        .expect("typed public constructor");
        let targets: &[MemoryId] = &req.origins;
        assert_eq!(
            targets,
            [source],
            "duplicate origins do not duplicate invocation inputs"
        );
        assert_eq!(req.origins.len(), 1);
    }

    /// What storage is handed, verbatim: the `derived_from` declaration
    /// arrives as `origins`, the payload's declared fields arrive as
    /// `references`, and the supersession target arrives as a pointer
    /// with no edge attached.
    #[tokio::test]
    async fn a_node_write_hands_storage_its_origins_and_references() {
        let recorder = Arc::new(RecordingAuthoring::default());
        let engine = Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests())
            .with_storage_ports(crate::StoragePorts::rejecting_with_memory_authoring(
                recorder.clone(),
            ));
        let owner = owner();
        let permit = OwnerWritePermit::new(owner, crate::access::AccessKind::Perspective);
        let prior = MemoryId::new(uuid::Uuid::now_v7());
        let origins = [
            EdgeEndpoint::memory(EntityKind::Fact, MemoryId::new(uuid::Uuid::now_v7())),
            EdgeEndpoint::memory(EntityKind::Fact, MemoryId::new(uuid::Uuid::now_v7())),
        ];
        let references = [EdgeEndpoint::memory(
            EntityKind::Abstraction,
            MemoryId::new(uuid::Uuid::now_v7()),
        )];
        let mut req = request(owner, &origins);
        req.supersedes = Some(prior);
        engine
            .author_derived(&permit, req, &references)
            .await
            .expect("the recording port accepts the write");

        let seen = recorder.seen.lock().expect("recorder");
        let seen = seen.as_ref().expect("one write recorded");
        assert_eq!(seen.origins, origins);
        assert_eq!(seen.references, references);
        assert_eq!(seen.supersedes, Some(prior));
    }

    #[tokio::test]
    async fn hydrate_uses_the_owner_write_gate_and_returns_typed_storage_results() {
        let recorder = Arc::new(RecordingAuthoring::default());
        let engine = Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests())
            .with_storage_ports(crate::StoragePorts::rejecting_with_memory_authoring(
                recorder.clone(),
            ));
        let owner = owner();
        let memory_id = MemoryId::new(uuid::Uuid::now_v7());
        let authz = AuthzContext::single_owner(&owner, crate::AuthPath::HostBearer);

        let outcome = engine
            .hydrate_memory(&authz, owner, memory_id)
            .await
            .expect("recording port returns a typed result");
        assert_eq!(outcome.memory_id, memory_id);
        assert_eq!(outcome.status, crate::MemoryHydrationStatus::AlreadyHot);
        assert_eq!(
            recorder
                .hydration_calls
                .lock()
                .expect("recorder")
                .as_slice(),
            &[(owner, vec![memory_id])]
        );
    }

    #[tokio::test]
    async fn hydrate_denies_before_reaching_storage() {
        let recorder = Arc::new(RecordingAuthoring::default());
        let engine = Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests())
            .with_storage_ports(crate::StoragePorts::rejecting_with_memory_authoring(
                recorder.clone(),
            ));
        let owner = owner();
        let memory_id = MemoryId::new(uuid::Uuid::now_v7());

        let error = engine
            .hydrate_memory(&AuthzContext::denied_for_owner(&owner), owner, memory_id)
            .await
            .expect_err("denied owner cannot hydrate");
        assert_eq!(error.code, ErrorCode::Forbidden);
        assert!(
            recorder
                .hydration_calls
                .lock()
                .expect("recorder")
                .is_empty()
        );
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct RecordedWrite {
        origins: Vec<EdgeEndpoint>,
        references: Vec<EdgeEndpoint>,
        supersedes: Option<MemoryId>,
    }

    #[derive(Debug, Default)]
    struct RecordingAuthoring {
        seen: std::sync::Mutex<Option<RecordedWrite>>,
        hydration_calls: std::sync::Mutex<Vec<(OwnerRef, Vec<MemoryId>)>>,
    }

    #[async_trait::async_trait]
    impl crate::MemoryAuthoringPort for RecordingAuthoring {
        async fn author_derived(
            &self,
            req: &AuthorDerivedRequest<'_>,
            _permit: &OwnerWritePermit,
            _proof: crate::storage_ports::OperatorWriteProof,
        ) -> Result<AuthorDerivedOutcome, StorageError> {
            *self.seen.lock().expect("recorder") = Some(RecordedWrite {
                origins: req.origins.to_vec(),
                references: req.references.to_vec(),
                supersedes: req.supersedes,
            });
            Ok(AuthorDerivedOutcome {
                memory_id: req.memory_id,
                idempotent_replay: false,
                edge_count: req.origins.len() + req.references.len(),
                embedding_deferred: false,
            })
        }

        async fn load_memory_kinds(
            &self,
            _owner: &Owner,
            _memory_ids: &[MemoryId],
        ) -> Result<Vec<crate::MemoryKindRow>, StorageError> {
            Ok(Vec::new())
        }

        async fn forget_memory(
            &self,
            _permit: &OwnerWritePermit,
            _memory_id: MemoryId,
        ) -> Result<(), StorageError> {
            Ok(())
        }

        async fn hydrate_memories(
            &self,
            permit: &OwnerWritePermit,
            memory_ids: &[MemoryId],
        ) -> Result<crate::MemoryHydrationBatchOutcome, StorageError> {
            self.hydration_calls
                .lock()
                .expect("recorder")
                .push((*permit.owner(), memory_ids.to_vec()));
            Ok(crate::MemoryHydrationBatchOutcome {
                outcomes: memory_ids
                    .iter()
                    .copied()
                    .map(|memory_id| {
                        crate::MemoryHydrationOutcome::simple(
                            memory_id,
                            crate::MemoryHydrationStatus::AlreadyHot,
                        )
                    })
                    .collect(),
                committed: true,
            })
        }
    }
}

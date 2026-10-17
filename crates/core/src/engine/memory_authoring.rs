use super::Engine;
use crate::MAX_MEMORY_HYDRATION_BATCH;
use crate::authz::EngineAuthority;
use crate::edge::{EdgeEndpoint, validate_edge_layering};
use crate::error::ProtocolError;
use crate::storage::{
    AuthorDerivedOutcome, AuthorDerivedRequest, MemoryHydrationBatchOutcome,
    MemoryHydrationOutcome, StorageError,
};
#[cfg(test)]
use crate::storage_ports::OwnerWritePermit;
use crate::{
    AbstractionPayload, EntityId, EntityKind, InputContractId, MemoryId, MemoryOperatorKind,
    OperatorId, OwnerId, PerspectivePayload, SchemaId, SchemaVersion, SidecarPayload,
};
use crate::{MemoryOutputInvocation, OperatorInvocationManifest, OutputEdgeManifest};

pub(super) struct PreparedDerived {
    pub(super) write_permit: super::pipeline::WritePermit,
    pub(super) owner: OwnerId,
    pub(super) memory_id: MemoryId,
    pub(super) kind: EntityKind,
    pub(super) text: String,
    pub(super) schema_id: SchemaId,
    pub(super) schema_version: SchemaVersion,
    pub(super) operator_kind: MemoryOperatorKind,
    pub(super) sidecar_payload: SidecarPayload,
    pub(super) supersedes: Option<MemoryId>,
    pub(super) lexical_language: Option<String>,
    pub(super) queued_spaces: Vec<crate::EmbeddingSpace>,
    pub(super) origins: Vec<EdgeEndpoint>,
    pub(super) references: Vec<EdgeEndpoint>,
    pub(super) external_origins: Vec<uuid::Uuid>,
    pub(super) external_references: Vec<uuid::Uuid>,
}

#[cfg(test)]
#[derive(Debug)]
struct RawDerivedTestRequest<'a> {
    pub memory_id: MemoryId,
    pub owner: OwnerId,
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
            external_origins: Vec::new(),
            external_references: Vec::new(),
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
            queued_spaces: &self.queued_spaces,
            origins: &self.origins,
            references: &self.references,
            external_origins: &self.external_origins,
            external_references: &self.external_references,
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

/// The handles of `external` once each sits on the layer of the local
/// `origins`, as [`infer_derivation_phase`] requires of the local ones. With
/// no local origin (an interpretation) they are subjects, which have no
/// layer rule.
fn external_on_layer(
    origins: &[EdgeEndpoint],
    external: &[crate::storage_ports::other_stores::ReadOrigin],
) -> Result<Vec<uuid::Uuid>, ProtocolError> {
    if let Some(layer) = origins.first().map(|origin| origin.kind)
        && let Some(off) = external.iter().find(|origin| origin.kind() != layer)
    {
        return Err(ProtocolError::invalid_argument(
            "origins",
            format!(
                "{} is a {:?} and the local origins are {layer:?}s; origins must be all Facts or all Abstractions",
                off.handle(),
                off.kind()
            ),
        ));
    }
    Ok(external
        .iter()
        .map(crate::storage_ports::other_stores::ReadOrigin::handle)
        .collect())
}

/// An other-store memory is an origin or a reference of one memory, never
/// both: `memory_external_origin` keys on `(t, memory)`.
fn external_references(
    origins: &[uuid::Uuid],
    references: &[crate::storage_ports::other_stores::ReadOrigin],
) -> Result<Vec<uuid::Uuid>, ProtocolError> {
    let references: Vec<uuid::Uuid> = references
        .iter()
        .map(crate::storage_ports::other_stores::ReadOrigin::handle)
        .collect();
    if let Some(both) = references.iter().find(|id| origins.contains(id)) {
        return Err(ProtocolError::invalid_argument(
            "references",
            format!("{both} is both an origin and a reference of this memory"),
        ));
    }
    Ok(references)
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
    /// This admission queued embedding work in its memory transaction.
    /// The vector becomes available after a drain. Nonembeddable schemas,
    /// routes with no clients, and idempotent replays report false.
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
    pub(crate) owner: OwnerId,
    pub(crate) text: String,
    pub(crate) sidecar_payload: SidecarPayload,
    pub(crate) origins: Vec<MemoryId>,
    pub(crate) identity: Option<DerivationIdentity>,
    pub(crate) extra_refs: Vec<MemoryId>,
    pub(crate) lexical_language: Option<String>,
    pub(crate) external_origins: Vec<crate::storage_ports::other_stores::ReadOrigin>,
    pub(crate) external_references: Vec<crate::storage_ports::other_stores::ReadOrigin>,
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
        owner: OwnerId,
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
            external_origins: Vec::new(),
            external_references: Vec::new(),
        })
    }
    /// Create a Perspective derived from Abstractions.
    ///
    /// # Errors
    /// Rejects empty origins. Use `interpretation` for a judgment grounded only through references.
    pub fn perspective<P: PerspectivePayload>(
        target: MemoryTarget,
        owner: OwnerId,
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
            external_origins: Vec::new(),
            external_references: Vec::new(),
        })
    }
    /// Create an origin-free Perspective grounded through its payload references.
    #[must_use]
    pub fn interpretation<P: PerspectivePayload>(
        target: MemoryTarget,
        owner: OwnerId,
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
            external_origins: Vec::new(),
            external_references: Vec::new(),
        }
    }
    /// Add memories of other stores, each read through the other-stores
    /// port as the caller ([`OtherStoresService::read_origin`]); duplicates
    /// are removed. Beside local origins they are origins on the same layer
    /// and are recorded as `origin` edges. An interpretation's remote
    /// subjects are references instead: see [`Self::external_references`].
    ///
    /// [`OtherStoresService::read_origin`]: crate::storage_ports::other_stores::OtherStoresService::read_origin
    #[must_use]
    pub fn external_origins(
        mut self,
        origins: impl IntoIterator<Item = crate::storage_ports::other_stores::ReadOrigin>,
    ) -> Self {
        for origin in origins {
            if !self.external_origins.contains(&origin) {
                self.external_origins.push(origin);
            }
        }
        self
    }
    /// Add memories of other stores this memory refers to, read like
    /// [`Self::external_origins`]; duplicates are removed. They are recorded
    /// as `reference` edges: an interpretation's remote subjects, the
    /// counterpart of its local subject references (#480).
    #[must_use]
    pub fn external_references(
        mut self,
        references: impl IntoIterator<Item = crate::storage_ports::other_stores::ReadOrigin>,
    ) -> Self {
        for reference in references {
            if !self.external_references.contains(&reference) {
                self.external_references.push(reference);
            }
        }
        self
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
    /// Read each unique memory's stored kind after authorizing it, in request order.
    /// Cooled memories use the same authorized kind stream as hot memories.
    ///
    /// # Errors
    ///
    /// Returns an error when an id is absent, unreadable, or storage cannot resolve it.
    pub async fn memory_kinds<A>(
        &self,
        authority: &A,
        ids: &[MemoryId],
    ) -> Result<Vec<(MemoryId, EntityKind)>, ProtocolError>
    where
        A: EngineAuthority + ?Sized,
    {
        let mut seen = std::collections::HashSet::with_capacity(ids.len());
        let mut kinds = Vec::with_capacity(ids.len());
        for id in ids {
            if !seen.insert(*id) {
                continue;
            }
            let permit = self
                .authorize_entry_read(authority, EntityId::Memory(*id))
                .await?;
            let kind = match permit.kind() {
                crate::AccessKind::Fact => EntityKind::Fact,
                crate::AccessKind::Abstraction => EntityKind::Abstraction,
                crate::AccessKind::Perspective => EntityKind::Perspective,
                crate::AccessKind::Goal => EntityKind::Goal,
            };
            kinds.push((*id, kind));
        }
        Ok(kinds)
    }

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
        let mut groups: Vec<(OwnerId, Vec<MemoryId>)> = Vec::new();
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
            let scope = self.operation_authority(authority)?.authz().owner_scope();
            let kinds = self
                .load_required_memory_kinds(scope, &owner, &group)
                .await?;
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
    /// Returns `Forbidden` when the context may write nothing on the owner,
    /// `NotFound` when `t` is absent or above the context's write limit, and
    /// storage errors from the forget transaction.
    pub async fn forget_memory<A>(
        &self,
        authority: &A,
        owner: OwnerId,
        memory_id: MemoryId,
    ) -> Result<(), ProtocolError>
    where
        A: EngineAuthority + ?Sized,
    {
        let write_permit = self.authorize_write_limit(authority, &owner).await?;
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
    /// The operation is owner-authorized with the same write gate as
    /// `forget_memory`; the storage tier receives only the sealed write
    /// permit. Missing, foreign and unwritable-kind ids collapse to
    /// `NotFound`, while cold object and integrity outcomes remain typed in
    /// the returned value.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context may write nothing on the
    /// owner, `InvalidArgument` for storage configuration faults, and
    /// `Internal` for unavailable storage or an exhausted lifecycle retry.
    pub async fn hydrate_memory<A>(
        &self,
        authority: &A,
        owner: OwnerId,
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
    /// Returns `Forbidden` when the context may write nothing on the owner;
    /// `InvalidArgument` for an unbounded or duplicate request; and
    /// `Internal` for storage failures or when the host cannot route the
    /// owner's embeddings.
    pub async fn hydrate_memories<A>(
        &self,
        authority: &A,
        owner: OwnerId,
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
        let write_permit = self.authorize_write_limit(authority, &owner).await?;
        // Hydrated memories are queued for the spaces the Owner's route names
        // now; a route error refuses the hydrate like any other write.
        let embedding_spaces = self.write_route(&owner).await?.write_spaces();
        let outcome = self
            .storage()
            .memory_authoring
            .memory_authoring
            .hydrate_memories(
                write_permit.owner_write_permit(),
                memory_ids,
                &embedding_spaces,
            )
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
    /// Returns `Forbidden` when the context cannot write the derived kind on
    /// the source owner or read an edge target; `InvalidArgument` when
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
        let item = self.prepare_derived_memory(authority, memory, &[]).await?;
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
    ) -> Result<PreparedDerived, ProtocolError>
    where
        A: EngineAuthority + ?Sized,
    {
        let kind = memory.output_kind()?;
        let write_permit = self
            .authorize_write(authority, &memory.owner, kind.into())
            .await?;
        let (memory_id, supersedes) = match memory.target {
            MemoryTarget::Series(handle) => (MemoryId::new(handle.into_inner()), None),
            // Storage resolves the prior row's handle. This private draft key is
            // unused for series selection and must not pretend to be the prior t.
            MemoryTarget::Revision(prior) => (
                MemoryId::new(crate::new_memory_uuid(self.store_number())),
                Some(prior),
            ),
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
        let external_origins = external_on_layer(&origins, &memory.external_origins)?;
        let external_references =
            external_references(&external_origins, &memory.external_references)?;
        // A new version's t is allocated by storage. The layer check needs only
        // the output kind; neither a handle nor the prior t is its source row.
        let source = EdgeEndpoint::memory(kind, memory_id);
        for origin in &origins {
            validate_edge_layering(source, *origin)
                .map_err(|err| ProtocolError::invalid_argument("origins", err))?;
        }
        let references = self
            .prepare_derived_references(authority, source, &memory, session_kinds)
            .await?;
        validate_typed_invocation(&memory, memory_id, kind, operator_kind, &origins)
            .map_err(map_derived_storage_error)?;
        let queued_spaces = self
            .embedding_spaces_for_write(
                write_permit.owner(),
                memory.sidecar_payload.schema_id.as_str(),
            )
            .await?;
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
            queued_spaces,
            origins,
            references,
            external_origins,
            external_references,
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
        owner: OwnerId,
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
            self.load_required_memory_kind(
                self.operation_authority(authority)?.authz().owner_scope(),
                permit.owner(),
                prior,
            )
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

    /// Test-only adapter for recording the raw storage/proof boundary.
    /// Like typed admission, it queues work without requesting a vector.
    ///
    /// # Errors
    /// Returns validation, routing-policy, or atomic storage errors.
    #[cfg(test)]
    async fn author_derived(
        &self,
        permit: &OwnerWritePermit,
        req: RawDerivedTestRequest<'_>,
        references: &[EdgeEndpoint],
    ) -> Result<AuthorDerivedOutcome, StorageError> {
        validate_operator_memory_invocation_request(&req)?;
        let queued_spaces = self
            .embedding_spaces_for_write(&req.owner, req.schema_id.as_str())
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;

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
            queued_spaces: &queued_spaces,
            origins: req.derived_from,
            references,
            external_origins: &[],
            external_references: &[],
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
        scope: Option<&crate::OwnerScope>,
        owner: &OwnerId,
        memory_id: MemoryId,
    ) -> Result<EntityKind, ProtocolError> {
        let mut kinds = self
            .load_required_memory_kinds(scope, owner, &[memory_id])
            .await?;
        Ok(kinds.remove(0))
    }

    pub(in crate::engine) async fn load_required_memory_kinds(
        &self,
        scope: Option<&crate::OwnerScope>,
        owner: &OwnerId,
        memory_ids: &[MemoryId],
    ) -> Result<Vec<EntityKind>, ProtocolError> {
        if memory_ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = self
            .storage()
            .memory_authoring
            .memory_authoring
            .load_memory_kinds(scope, owner, memory_ids)
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

/// Validate every chunk before storage can publish an embedding version.
pub(in crate::engine) fn space_vectors(
    bound: &crate::llm::BoundEmbeddingClient,
    vectors: Vec<Vec<f32>>,
) -> Result<Vec<crate::SpaceVector>, StorageError> {
    if vectors.is_empty() {
        return Err(StorageError::ConstraintViolation(
            "embedding version needs at least one chunk".into(),
        ));
    }
    vectors
        .into_iter()
        .map(|vector| space_vector(bound, vector))
        .collect()
}

/// A vector the client returned, in its space. A width other than the one
/// the client declared is a misconfiguration, never the input's fault.
pub(in crate::engine) fn space_vector(
    bound: &crate::llm::BoundEmbeddingClient,
    vector: Vec<f32>,
) -> Result<crate::SpaceVector, StorageError> {
    bound
        .vector(vector)
        .map_err(|err| StorageError::ConstraintViolation(err.to_string()))
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
        AbstractionPayload, AgentDerivationV1, EntityKind, FlavorRegistry, OwnerId, UserId,
    };

    use super::*;

    fn owner() -> OwnerId {
        OwnerId::from(UserId::new(uuid::Uuid::now_v7()))
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

    fn request(owner: OwnerId, derived_from: &[EdgeEndpoint]) -> RawDerivedTestRequest<'_> {
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

    /// A memory of another store joins local origins only on their layer,
    /// whoever builds the request; an interpretation's other-store subjects
    /// have no layer rule.
    #[test]
    fn other_store_origins_share_the_local_layer() {
        use crate::storage_ports::other_stores::ReadOrigin;
        let fact_id = crate::new_memory_uuid(5);
        let abstraction_id = crate::new_memory_uuid(5);
        let perspective_id = crate::new_memory_uuid(5);
        let facts = [EdgeEndpoint::memory(
            EntityKind::Fact,
            MemoryId::new(crate::new_memory_uuid(3)),
        )];
        let same = external_on_layer(&facts, &[ReadOrigin::for_tests(fact_id, EntityKind::Fact)])
            .expect("a Fact beside Facts");
        assert_eq!(same, vec![fact_id]);
        let err = external_on_layer(
            &facts,
            &[ReadOrigin::for_tests(
                abstraction_id,
                EntityKind::Abstraction,
            )],
        )
        .expect_err("an Abstraction beside Facts");
        assert_eq!(err.code, ErrorCode::InvalidArgument);
        assert!(err.message.contains(&abstraction_id.to_string()), "{err:?}");
        let subjects = external_on_layer(
            &[],
            &[ReadOrigin::for_tests(
                perspective_id,
                EntityKind::Perspective,
            )],
        )
        .expect("an interpretation's subject");
        assert_eq!(subjects, [perspective_id]);
    }

    /// An F→A derive that names its inputs carries a well-formed
    /// manifest and reaches storage. There is no batch input left on the
    /// request for a validator to demand: the derived-write API declares
    /// origins and nothing else about the operator run.
    #[tokio::test]
    async fn author_derived_ftoa_with_origins_passes_manifest_validation() {
        let engine = engine();
        let owner = owner();
        let permit = OwnerWritePermit::new(owner, crate::access::AccessKind::Perspective, None);
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
        let permit = OwnerWritePermit::new(owner, crate::access::AccessKind::Perspective, None);
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

    #[tokio::test]
    async fn write_embedding_spaces_preserve_schema_exclusion_and_route_policy() {
        let owner = owner();
        let router = Arc::new(crate::test_fixtures::TestEmbeddingRouter::default());
        router.refuse(owner, "owner route is not configured");
        let engine = engine().with_embedding_router(router.clone());
        assert!(
            engine
                .embedding_spaces_for_write(&owner, "core/write-act-v1")
                .await
                .expect("a nonembeddable schema does not need a route")
                .is_empty()
        );
        let error = engine
            .embedding_spaces_for_write(&owner, "core/agent-derivation-v1")
            .await
            .expect_err("routing policy still refuses an embeddable write");
        assert_eq!(error.code, ErrorCode::Internal);
        router.set_owner(owner, None);
        assert!(
            engine
                .embedding_spaces_for_write(&owner, "core/agent-derivation-v1")
                .await
                .expect("a route naming no clients requests no jobs")
                .is_empty()
        );
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
        let permit = OwnerWritePermit::new(owner, crate::access::AccessKind::Perspective, None);
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
        hydration_calls: std::sync::Mutex<Vec<(OwnerId, Vec<MemoryId>)>>,
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
            _scope: Option<&crate::OwnerScope>,
            _owner: &OwnerId,
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
            _embedding_spaces: &[crate::EmbeddingSpace],
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

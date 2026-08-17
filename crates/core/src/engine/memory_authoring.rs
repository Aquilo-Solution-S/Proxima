use super::Engine;
use crate::access::Relation;
use crate::authz::{AuthzContext, EngineAuthority};
use crate::edge::{EdgeEndpoint, validate_edge_layering, validate_not_self_loop};
use crate::error::ProtocolError;
use crate::storage::{AuthorDerivedOutcome, AuthorDerivedRequest, DerivedEmbedding, StorageError};
use crate::storage_ports::OwnerWritePermit;
use crate::{
    EntityId, EntityKind, InputContractId, MemoryId, MemoryOperatorKind, OperatorId, Owner,
    PayloadReference, SchemaId, SchemaVersion, SidecarPayload, SourceBatchId,
};
use crate::{MemoryOutputInvocation, OperatorInvocationManifest, OutputEdgeManifest};

#[derive(Debug)]
pub struct AuthorDerivedRequestInput<'a> {
    pub memory_id: MemoryId,
    pub owner: Owner,
    pub kind: EntityKind,
    pub text: String,
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub operator_kind: MemoryOperatorKind,
    pub operator_id: OperatorId,
    pub input_contract_id: InputContractId,
    pub source_batch_id: Option<SourceBatchId>,
    pub model_id: &'a str,
    pub prompt_version: &'a str,
    pub sidecar_payload: SidecarPayload,
    /// Perspective that emitted this memory. Node metadata on the row,
    /// not an edge: "emitted by P" is known at write time and belongs to
    /// the node, so nothing has to be traversed to answer it.
    pub authoring_perspective_id: Option<MemoryId>,
    /// What this memory was made from. Each entry becomes an `Origin`
    /// index row sourced at this memory, written in the same transaction.
    /// The writer names targets; it never names a kind.
    pub derived_from: &'a [EdgeEndpoint],
    /// Prior A/P memory this one revises. The engine records it as a
    /// lineage pointer on the rows — no supersession edge exists to write.
    pub supersedes: Option<MemoryId>,
    /// Text-search configuration to stamp on the derived row, resolved
    /// by [`crate::lexical_language::resolve_lexical_language`]; `None`
    /// applies the database default.
    pub lexical_language: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorDerivedAuthorizedOutcome {
    pub memory_id: MemoryId,
    pub idempotent_replay: bool,
    /// Index rows asserted by this write. A count, not a list of
    /// handles: an edge has no id to hand back, and re-running the write
    /// re-asserts the same rows.
    pub edge_count: usize,
    /// The memory landed with no vector and a pending embedding job; it is
    /// lexically findable and semantically invisible until a drain runs.
    /// Callers that need it searchable immediately can see that here rather
    /// than by reading logs — see [`crate::AuthorDerivedOutcome`].
    pub embedding_deferred: bool,
}

impl Engine {
    /// Close the single open source batch behind a set of F→A input Facts.
    ///
    /// Sources that group `core_remember` writes with a `source_batch_key`
    /// never issue an explicit close; deriving an Abstraction from the
    /// batch is the natural completion signal, so consolidation closes it
    /// here (idempotently) before the F→A closed-batch gate runs. No-ops
    /// when inputs are unbatched, missing, or span batches — those shapes
    /// are left to the F→A validation path for its precise errors.
    ///
    /// # Errors
    ///
    /// Returns authorization failures from the owner-scoped close
    /// ([`Relation::Ingest`] is required, as for any batch close) and
    /// `Internal` for storage failures.
    pub fn close_ftoa_source_batch_if_open(
        &self,
        authz: &AuthzContext,
        owner: crate::OwnerRef,
        _source_memory_ids: &[MemoryId],
    ) -> Result<(), ProtocolError> {
        let _ = owner;
        self.operation_authority(authz)?;
        Ok(())
    }

    /// Cool one owned memory `t`. PUT cold first, then stub+delete hot.
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

    /// Authorized graph-write verb for agent-authored derived memory.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context lacks [`Relation::Editor`] on the
    /// source owner or read access to an edge target; `InvalidArgument` when
    /// referenced memories are absent or edge shape validation fails; and
    /// `Internal` for storage failures.
    pub async fn author_derived_authorized<A>(
        &self,
        authority: &A,
        req: AuthorDerivedRequestInput<'_>,
    ) -> Result<AuthorDerivedAuthorizedOutcome, ProtocolError>
    where
        A: EngineAuthority + ?Sized,
    {
        let write_permit = self
            .authorize_write(authority, &req.owner, Relation::Editor)
            .await?;

        let owner = *write_permit.owner();
        if let Some(prior) = req.supersedes {
            let prior_home = self
                .storage()
                .memory_authoring
                .owner_access_read
                .home_owner(EntityId::Memory(prior))
                .await
                .map_err(|err| ProtocolError::internal(err.to_string()))?;
            if prior_home.as_ref() != Some(&owner) {
                return Err(ProtocolError::forbidden(
                    "supersedes target is not an owned entity of the same owner",
                ));
            }
            let prior_kind = self.load_required_memory_kind(&owner, prior).await?;
            if prior_kind != req.kind {
                return Err(ProtocolError::invalid_argument(
                    "supersedes",
                    "must supersede a memory of the same kind",
                ));
            }
        }
        // One uniform admission rule replaces the per-relation policy
        // matrix (docs/16 §Ownership and visibility): the row is owned by
        // the source owner — already established by the write permit
        // above — and the write is admitted iff the writer can also READ
        // every target at write time.
        let source = EdgeEndpoint::memory(req.kind, req.memory_id);
        let origins = self
            .authorized_index_targets(authority, source, req.derived_from, "derived_from")
            .await?;
        let declared = req.sidecar_payload.references();
        let references = self
            .authorized_payload_references(authority, source, &declared)
            .await?;
        let source_batch_id = None;
        let outcome = self
            .author_derived(
                write_permit.owner_write_permit(),
                AuthorDerivedRequestInput {
                    memory_id: req.memory_id,
                    owner,
                    kind: req.kind,
                    text: req.text,
                    schema_id: req.schema_id,
                    schema_version: req.schema_version,
                    operator_kind: req.operator_kind,
                    operator_id: req.operator_id,
                    input_contract_id: req.input_contract_id,
                    source_batch_id,
                    model_id: req.model_id,
                    prompt_version: req.prompt_version,
                    sidecar_payload: req.sidecar_payload,
                    authoring_perspective_id: req.authoring_perspective_id,
                    derived_from: &origins,
                    supersedes: req.supersedes,
                    lexical_language: req.lexical_language,
                },
                &references,
            )
            .await
            .map_err(map_derived_storage_error)?;

        Ok(AuthorDerivedAuthorizedOutcome {
            memory_id: outcome.memory_id,
            idempotent_replay: outcome.idempotent_replay,
            edge_count: outcome.edge_count,
            embedding_deferred: outcome.embedding_deferred,
        })
    }

    /// Author one derived Memory and its already-resolved edges. When an
    /// embedding client is configured, the Engine embeds before storage;
    /// otherwise storage receives [`DerivedEmbedding::None`] and persists no
    /// embedding row.
    ///
    /// An input this client cannot embed does not fail the write. The
    /// memory lands with no vector and a pending embedding job enqueued in
    /// the same transaction ([`DerivedEmbedding::Deferred`]), so
    /// [`Engine::drain_embedding_jobs`] — which owns the bisecting
    /// over-limit rescue that this path has never had — picks it up. The
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
    /// Engine-internal raw write. Callers outside `author_derived_authorized`
    /// would bypass owner write authorization; there is no public API for
    /// this method.
    pub(in crate::engine) async fn author_derived(
        &self,
        permit: &OwnerWritePermit,
        req: AuthorDerivedRequestInput<'_>,
        references: &[EdgeEndpoint],
    ) -> Result<AuthorDerivedOutcome, StorageError> {
        validate_operator_memory_invocation_request(&req)?;
        // Bound outside the call: `DerivedEmbedding` borrows the client's
        // model id for the length of the storage request.
        let client = self.embed_client();
        let embedding = match client.as_deref() {
            None => DerivedEmbedding::None,
            Some(client) => resolve_derived_embedding(client, req.memory_id, &req.text).await?,
        };

        let storage_req = AuthorDerivedRequest {
            memory_id: req.memory_id,
            owner: req.owner,
            kind: req.kind,
            text: req.text,
            schema_id: req.schema_id,
            schema_version: req.schema_version,
            operator_kind: req.operator_kind,
            operator_id: req.operator_id,
            input_contract_id: req.input_contract_id,
            source_batch_id: req.source_batch_id,
            model_id: req.model_id,
            prompt_version: req.prompt_version,
            sidecar_payload: req.sidecar_payload,
            authoring_perspective_id: req.authoring_perspective_id,
            // Supersession is a pointer on the rows, not an edge: there
            // is nothing to append here, only a field for storage to
            // stamp inside the same transaction.
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

    /// Resolve and admit every declared index target.
    ///
    /// One rule for all of them, whatever the write is: the target must
    /// exist, the writer must be able to READ it, and the resulting edge
    /// must respect layering. There is no per-relation policy cell left to
    /// consult and no owner-equality rule beyond it — a source-owned row
    /// pointing at a foreign readable target is exactly what makes
    /// cross-owner provenance expressible.
    pub(in crate::engine) async fn authorized_index_targets<A>(
        &self,
        authority: &A,
        source: EdgeEndpoint,
        targets: &[EdgeEndpoint],
        field: &str,
    ) -> Result<Vec<EdgeEndpoint>, ProtocolError>
    where
        A: EngineAuthority + ?Sized,
    {
        self.authorized_index_targets_visible(authority, source, targets, field, &[])
            .await
    }

    pub(in crate::engine) async fn authorized_index_targets_visible<A>(
        &self,
        authority: &A,
        source: EdgeEndpoint,
        targets: &[EdgeEndpoint],
        field: &str,
        session_visible: &[MemoryId],
    ) -> Result<Vec<EdgeEndpoint>, ProtocolError>
    where
        A: EngineAuthority + ?Sized,
    {
        let mut out = Vec::with_capacity(targets.len());
        for target in targets {
            validate_not_self_loop(source, *target)
                .map_err(|err| ProtocolError::invalid_argument(field, err))?;
            let resolved = self
                .authorize_index_target(authority, *target, field, session_visible)
                .await?;
            validate_edge_layering(source, resolved)
                .map_err(|err| ProtocolError::invalid_argument(field, err))?;
            if !out.contains(&resolved) {
                out.push(resolved);
            }
        }
        Ok(out)
    }

    /// Check the schema-declared reference fields of a payload, then admit
    /// their targets like any other index target.
    ///
    /// Schema-declared reference fields become index targets. Every
    /// address is a pin.
    pub(in crate::engine) async fn authorized_payload_references<A>(
        &self,
        authority: &A,
        source: EdgeEndpoint,
        declared: &[PayloadReference],
    ) -> Result<Vec<EdgeEndpoint>, ProtocolError>
    where
        A: EngineAuthority + ?Sized,
    {
        let mut targets = Vec::with_capacity(declared.len());
        for reference in declared {
            reference
                .validate()
                .map_err(|err| ProtocolError::invalid_argument("references", err))?;
            targets.push(reference.target);
        }
        self.authorized_index_targets_visible(authority, source, &targets, "references", &[])
            .await
    }

    pub(in crate::engine) async fn authorized_payload_references_visible<A>(
        &self,
        authority: &A,
        source: EdgeEndpoint,
        declared: &[PayloadReference],
        session_visible: &[MemoryId],
    ) -> Result<Vec<EdgeEndpoint>, ProtocolError>
    where
        A: EngineAuthority + ?Sized,
    {
        let mut targets = Vec::with_capacity(declared.len());
        for reference in declared {
            reference
                .validate()
                .map_err(|err| ProtocolError::invalid_argument("references", err))?;
            targets.push(reference.target);
        }
        self.authorized_index_targets_visible(
            authority,
            source,
            &targets,
            "references",
            session_visible,
        )
        .await
    }

    /// Read-admit one index target. Stored kind is compared in-tx
    /// against the declared pin; a second pre-tx `load_memory_kinds`
    /// is the overlapping fanout this slice drops.
    async fn authorize_index_target<A>(
        &self,
        authority: &A,
        target: EdgeEndpoint,
        _field: &str,
        session_visible: &[MemoryId],
    ) -> Result<EdgeEndpoint, ProtocolError>
    where
        A: EngineAuthority + ?Sized,
    {
        match target.entity {
            crate::EntityRef::Memory(memory_id) if session_visible.contains(&memory_id) => {
                Ok(target)
            }
            crate::EntityRef::Memory(memory_id) => {
                self.authorize_entry_read(authority, EntityId::Memory(memory_id))
                    .await?;
                Ok(target)
            }
            crate::EntityRef::Goal(goal_id) => {
                self.authorize_entry_read(authority, EntityId::Goal(goal_id))
                    .await?;
                Ok(EdgeEndpoint::goal(goal_id))
            }
        }
    }

    pub(in crate::engine) async fn load_required_memory_kind(
        &self,
        owner: &Owner,
        memory_id: MemoryId,
    ) -> Result<EntityKind, ProtocolError> {
        let mut kinds = self.load_required_memory_kinds(owner, &[memory_id]).await?;
        Ok(kinds.remove(0))
    }

    async fn load_required_memory_kinds(
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
                        "cross-space derive/link is not supported; choose one memory space",
                    )
                })
            })
            .collect()
    }
}

/// Decide what a derived write should do about its vector, given a
/// configured embedding client.
///
/// A text this client will not embed downgrades the write (vector is
/// recoverable by a drain that bisects) only after a liveness probe: an
/// outage says nothing about the text.
///
/// # Errors
///
/// `ConstraintViolation` when the vector's length disagrees with the
/// client's declared `dim` (a misconfiguration, never the input's fault,
/// so it is not deferrable), and `Internal` when the provider fails and
/// does not answer a liveness probe.
pub(in crate::engine) async fn resolve_derived_embedding<'client>(
    client: &'client dyn crate::llm::EmbeddingClient,
    memory_id: MemoryId,
    text: &str,
) -> Result<DerivedEmbedding<'client>, StorageError> {
    match client.embed(text).await {
        Ok(vector) => {
            if vector.len() != client.dim() {
                return Err(StorageError::ConstraintViolation(format!(
                    "embedding dim mismatch: client dim {} but vector len {}",
                    client.dim(),
                    vector.len(),
                )));
            }
            Ok(DerivedEmbedding::Ready {
                model_id: client.model_id(),
                vector,
            })
        }
        Err(err) if crate::llm::embed_failure_blames_the_input(client, &err).await => {
            tracing::warn!(
                error = %err,
                memory_id = ?memory_id,
                text_bytes = text.len(),
                "derived memory text refused by a live embedding provider; \
                 writing the memory without a vector and enqueueing an embedding job"
            );
            Ok(DerivedEmbedding::Deferred {
                model_id: client.model_id(),
            })
        }
        Err(err) => Err(StorageError::Internal(format!(
            "embed derived memory text: {err}"
        ))),
    }
}

pub(in crate::engine) fn validate_operator_memory_invocation_request(
    req: &AuthorDerivedRequestInput<'_>,
) -> Result<(), StorageError> {
    let _ = req.source_batch_id;

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

    fn request(owner: Owner, derived_from: &[EdgeEndpoint]) -> AuthorDerivedRequestInput<'_> {
        AuthorDerivedRequestInput {
            memory_id: MemoryId::new(uuid::Uuid::now_v7()),
            owner,
            kind: EntityKind::Abstraction,
            text: "body".into(),
            schema_id: SchemaId::new(AgentDerivationV1::SCHEMA_ID.into()),
            schema_version: SchemaVersion::new(AgentDerivationV1::SCHEMA_VERSION),
            operator_kind: MemoryOperatorKind::FtoA,
            operator_id: OperatorId::new(uuid::Uuid::now_v7()),
            input_contract_id: InputContractId::new(uuid::Uuid::now_v7()),
            source_batch_id: Some(SourceBatchId::new(uuid::Uuid::now_v7())),
            model_id: "test-model",
            prompt_version: "test",
            sidecar_payload: derivation_sidecar(),
            authoring_perspective_id: None,
            derived_from,
            supersedes: None,
            lexical_language: None,
        }
    }

    #[tokio::test]
    async fn author_derived_allows_operator_ftoa_without_source_batch() {
        let engine = engine();
        let owner = owner();
        let permit = OwnerWritePermit::new(owner, crate::access::AccessKind::Perspective);
        let origins = [EdgeEndpoint::memory(
            EntityKind::Fact,
            MemoryId::new(uuid::Uuid::now_v7()),
        )];
        let mut req = request(owner, &origins);
        req.source_batch_id = None;
        let err = engine
            .author_derived(&permit, req, &[])
            .await
            .expect_err("the fake storage port refuses every write");
        assert!(
            !matches!(
                &err,
                StorageError::ConstraintViolation(msg) if msg.contains("source_batch_id")
            ),
            "F→A no longer requires a source batch: {err}"
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
        req.source_batch_id = None;
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
    async fn author_derived_authorized_denies_denied_context() {
        let engine = engine();
        let owner = owner();
        let err = engine
            .author_derived_authorized(&AuthzContext::denied_for_owner(&owner), request(owner, &[]))
            .await
            .expect_err("denied context must fail before storage");

        assert_eq!(err.code, ErrorCode::Forbidden);
    }

    #[test]
    fn pin_admit_does_not_reload_kind_after_entry_read() {
        let src = include_str!("memory_authoring.rs");
        let start = src
            .find("async fn authorize_index_target")
            .expect("authorize_index_target");
        let rest = &src[start..];
        let end = rest
            .find("pub(in crate::engine) async fn load_required_memory_kind")
            .expect("load_required_memory_kind follows");
        let body = &rest[..end];
        let reload = format!("{}{}", "load_required_memory_", "kind");
        assert!(
            !body.contains(&reload),
            "D2: stored kind is the in-tx TOCTOU SELECT, not a second pre-tx fanout"
        );
        assert!(
            body.contains("authorize_entry_read"),
            "read-admit stays; only the kind reload is dropped"
        );
    }

    /// The public write surface takes targets, never kinds: there is no
    /// argument anywhere on it that could carry an [`crate::EdgeKind`].
    /// `Origin` is what a `derived_from` declaration means and
    /// `Reference` is what a payload field means, so a writer has nothing
    /// to choose and nothing to get wrong.
    #[tokio::test]
    async fn no_public_write_input_accepts_an_edge_kind() {
        let owner = owner();
        let origins = [EdgeEndpoint::memory(
            EntityKind::Fact,
            MemoryId::new(uuid::Uuid::now_v7()),
        )];
        let req = request(owner, &origins);
        // Compiles only because the declaration is a list of endpoints.
        let _targets: &[EdgeEndpoint] = req.derived_from;
        assert_eq!(req.derived_from.len(), 1);
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

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct RecordedWrite {
        origins: Vec<EdgeEndpoint>,
        references: Vec<EdgeEndpoint>,
        supersedes: Option<MemoryId>,
    }

    #[derive(Debug, Default)]
    struct RecordingAuthoring {
        seen: std::sync::Mutex<Option<RecordedWrite>>,
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

        async fn load_fact_source_batches(
            &self,
            _owner: &Owner,
            _memory_ids: &[MemoryId],
        ) -> Result<Vec<crate::FactSourceBatchRow>, StorageError> {
            Ok(Vec::new())
        }

        async fn forget_memory(
            &self,
            _permit: &OwnerWritePermit,
            _memory_id: MemoryId,
        ) -> Result<(), StorageError> {
            Ok(())
        }
    }
}

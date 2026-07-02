use super::Engine;
use crate::access::Relation;
use crate::authz::AuthzContext;
use crate::error::ProtocolError;
use crate::storage::{AuthorDerivedOutcome, AuthorDerivedRequest, DerivedEdgeSpec, StorageError};
use crate::{
    CORE_SUPERSEDES_RELATION, EdgeAuthorshipKind, EdgeId, EndpointBinding, EntityId, EntityKind,
    InputContractId, MemoryId, MemoryOperatorKind, OperatorId, Owner, RegisteredRelation,
    RelationOwnerPolicy, RelationTargetAccessPolicy, SchemaId, SchemaVersion, SidecarPayload,
    SourceBatchId, validate_operator_edge_shape,
};
use crate::{MemoryOutputInvocation, OperatorInvocationManifest, OutputEdgeManifest};

#[derive(Debug, Clone)]
pub struct AuthorDerivedEdgeInput<'a> {
    pub relation: RegisteredRelation<'a>,
    pub source_kind: EntityKind,
    pub source_memory_id: MemoryId,
    pub target_kind: EntityKind,
    pub target_memory_id: MemoryId,
    pub authorship_kind: EdgeAuthorshipKind,
    pub authorship_owner_memory_id: Option<MemoryId>,
}

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
    /// Prior A/P memory superseded by this derived memory. The engine
    /// records both `memories.supersedes` and a same-transaction
    /// `core/supersedes` edge.
    pub supersedes: Option<MemoryId>,
    pub edges: &'a [AuthorDerivedEdgeInput<'a>],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorDerivedAuthorizedOutcome {
    pub memory_id: MemoryId,
    pub idempotent_replay: bool,
    pub edge_ids: Vec<EdgeId>,
}

#[derive(Debug, Clone)]
pub struct AppendMemoryEdgeRequestInput<'a> {
    pub owner: Owner,
    pub relation: RegisteredRelation<'a>,
    pub source_memory_id: MemoryId,
    pub target_memory_id: MemoryId,
    pub authorship_kind: EdgeAuthorshipKind,
    pub authorship_owner_memory_id: Option<MemoryId>,
    pub sidecar_payload: Option<&'a SidecarPayload>,
}

impl Engine {
    /// Authorized graph-write verb for agent-authored derived memory.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context lacks [`Relation::Editor`] on the
    /// source owner or read access to an edge target; `InvalidArgument` when
    /// referenced memories are absent or edge shape validation fails; and
    /// `Internal` for storage failures.
    pub async fn author_derived_authorized(
        &self,
        authz: &AuthzContext,
        req: AuthorDerivedRequestInput<'_>,
    ) -> Result<AuthorDerivedAuthorizedOutcome, ProtocolError> {
        let write_permit = self
            .authorize_write(authz, &req.owner, Relation::Editor)
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
        let edges = self.validated_author_derived_edges(authz, &req).await?;
        let source_batch_id = self
            .effective_operator_source_batch_id(&owner, &req, &edges)
            .await?;
        let target_ids = req
            .edges
            .iter()
            .map(|edge| edge.target_memory_id)
            .collect::<Vec<_>>();
        let relation = req
            .edges
            .first()
            .map(|edge| edge.relation.descriptor.relation.as_str());

        let outcome = self
            .author_derived(AuthorDerivedRequestInput {
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
                supersedes: req.supersedes,
                edges: &edges,
            })
            .await
            .map_err(|err| ProtocolError::internal(err.to_string()))?;

        let edge_ids = if outcome.idempotent_replay || target_ids.is_empty() {
            Vec::new()
        } else if let Some(relation) = relation {
            self.storage()
                .memory_authoring
                .memory_authoring
                .load_memory_edge_ids(&owner, relation, outcome.memory_id, &target_ids)
                .await
                .map_err(|err| ProtocolError::internal(err.to_string()))?
        } else {
            Vec::new()
        };

        Ok(AuthorDerivedAuthorizedOutcome {
            memory_id: outcome.memory_id,
            idempotent_replay: outcome.idempotent_replay,
            edge_ids,
        })
    }

    /// Authorized graph-write verb for appending one memory edge.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context lacks [`Relation::Editor`] on the
    /// source owner or read access to the target; `InvalidArgument` when
    /// endpoints are absent or the relation rejects the shape; and
    /// `Internal` for storage failures.
    pub async fn append_memory_edge_authorized(
        &self,
        authz: &AuthzContext,
        req: AppendMemoryEdgeRequestInput<'_>,
    ) -> Result<EdgeId, ProtocolError> {
        let (owner, source_kind) = self.load_memory_owner_kind(req.source_memory_id).await?;
        self.authorize_write(authz, &owner, write_relation_for_entity_kind(source_kind))
            .await?;
        let (target_owner, target_kind) = self
            .authorize_edge_target_policy(authz, req.relation, req.target_memory_id)
            .await?;
        validate_relation_owner_policy(req.relation, &owner, &target_owner, "edge")?;
        validate_relation_shape(
            req.relation,
            source_kind,
            target_kind,
            req.authorship_kind,
            "edge",
        )?;
        if req.authorship_kind.is_operator() {
            return Err(ProtocolError::invalid_argument(
                "edge",
                "operator-authored edges require an operator proof-carrier write path",
            ));
        }

        let edge = DerivedEdgeSpec {
            owner: &owner,
            relation: req.relation,
            source_kind,
            source_memory_id: req.source_memory_id,
            target_kind,
            target_memory_id: req.target_memory_id,
            authorship_kind: req.authorship_kind,
            authorship_owner_memory_id: req.authorship_owner_memory_id,
            sidecar_payload: req.sidecar_payload,
        };
        self.storage()
            .memory_authoring
            .memory_authoring
            .append_memory_edge(&edge, crate::storage_ports::EdgeWriteProof::new())
            .await
            .map_err(|err| ProtocolError::internal(err.to_string()))
    }

    /// Author one derived Memory and its already-resolved edges. When an
    /// embedding client is configured, the Engine embeds before storage;
    /// otherwise storage receives `None` and persists no embedding row.
    ///
    /// # Errors
    ///
    /// Returns `Internal` when the embedding client fails,
    /// `ConstraintViolation` on embedding dimension mismatch, and storage
    /// errors from the atomic write.
    ///
    /// Engine-internal raw write. Callers outside `author_derived_authorized`
    /// would bypass owner write authorization; there is no public API for
    /// this method.
    pub(in crate::engine) async fn author_derived(
        &self,
        req: AuthorDerivedRequestInput<'_>,
    ) -> Result<AuthorDerivedOutcome, StorageError> {
        validate_operator_memory_invocation_request(&req)?;
        let (embedding, embedding_model_id) = if let Some(client) = self.embed_client() {
            let embedding = client
                .embed(&req.text)
                .await
                .map_err(|e| StorageError::Internal(format!("embed derived memory text: {e}")))?;
            if embedding.len() != client.dim() {
                return Err(StorageError::ConstraintViolation(format!(
                    "embedding dim mismatch: client dim {} but vector len {}",
                    client.dim(),
                    embedding.len(),
                )));
            }
            (Some(embedding), Some(client.model_id().to_string()))
        } else {
            (None, None)
        };

        let supersedes_relation = if req.supersedes.is_some() {
            Some(
                self.registry()
                    .resolve_relation(CORE_SUPERSEDES_RELATION)
                    .ok_or_else(|| {
                        StorageError::ConstraintViolation(format!(
                            "relation {CORE_SUPERSEDES_RELATION} not registered"
                        ))
                    })?,
            )
        } else {
            None
        };

        let owner = req.owner;
        let mut edges: Vec<DerivedEdgeSpec<'_>> = req
            .edges
            .iter()
            .map(|edge| DerivedEdgeSpec {
                owner: &owner,
                relation: edge.relation,
                source_kind: edge.source_kind,
                source_memory_id: edge.source_memory_id,
                target_kind: edge.target_kind,
                target_memory_id: edge.target_memory_id,
                authorship_kind: edge.authorship_kind,
                authorship_owner_memory_id: edge.authorship_owner_memory_id,
                sidecar_payload: None,
            })
            .collect();
        if let (Some(prior), Some(relation)) = (req.supersedes, supersedes_relation) {
            edges.push(DerivedEdgeSpec {
                owner: &owner,
                relation,
                source_kind: req.kind,
                source_memory_id: req.memory_id,
                target_kind: req.kind,
                target_memory_id: prior,
                authorship_kind: EdgeAuthorshipKind::Engine,
                authorship_owner_memory_id: None,
                sidecar_payload: None,
            });
        }
        let storage_req = AuthorDerivedRequest {
            memory_id: req.memory_id,
            owner,
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
            supersedes: req.supersedes,
            embedding,
            embedding_model_id: embedding_model_id.as_deref(),
            edges: &edges,
        };
        self.storage()
            .memory_authoring
            .memory_authoring
            .author_derived(
                &storage_req,
                crate::storage_ports::OperatorWriteProof::new(),
            )
            .await
    }

    async fn effective_operator_source_batch_id(
        &self,
        owner: &Owner,
        req: &AuthorDerivedRequestInput<'_>,
        edges: &[AuthorDerivedEdgeInput<'_>],
    ) -> Result<Option<SourceBatchId>, ProtocolError> {
        match req.operator_kind {
            MemoryOperatorKind::FtoA => {
                let input_ids = edges
                    .iter()
                    .filter(|edge| {
                        edge.authorship_kind == MemoryOperatorKind::FtoA.edge_authorship()
                    })
                    .map(|edge| edge.target_memory_id)
                    .collect::<Vec<_>>();
                let rows = self
                    .storage()
                    .memory_authoring
                    .memory_authoring
                    .load_fact_source_batches(owner, &input_ids)
                    .await
                    .map_err(|err| ProtocolError::internal(err.to_string()))?;
                if rows.len() != input_ids.len() {
                    return Err(ProtocolError::invalid_argument(
                        "source_handles",
                        "F→A operator inputs must be Fact memories with source receipts",
                    ));
                }
                let first = rows.first().map(|row| row.source_batch_id).ok_or_else(|| {
                    ProtocolError::invalid_argument(
                        "source_handles",
                        "F→A operator invocation requires source inputs",
                    )
                })?;
                if rows.iter().any(|row| row.source_batch_id != first) {
                    return Err(ProtocolError::invalid_argument(
                        "source_handles",
                        "F→A operator inputs must belong to one source batch",
                    ));
                }
                if let Some(requested) = req.source_batch_id
                    && requested != first
                {
                    return Err(ProtocolError::invalid_argument(
                        "source_batch_id",
                        "must match the F→A input Facts",
                    ));
                }
                Ok(Some(first))
            }
            MemoryOperatorKind::AtoA | MemoryOperatorKind::AtoP => Ok(req.source_batch_id),
        }
    }

    async fn validated_author_derived_edges<'a>(
        &self,
        authz: &AuthzContext,
        req: &AuthorDerivedRequestInput<'a>,
    ) -> Result<Vec<AuthorDerivedEdgeInput<'a>>, ProtocolError> {
        if req.edges.is_empty() {
            return Ok(Vec::new());
        }
        let mut out = Vec::with_capacity(req.edges.len());
        for edge in req.edges {
            let source_kind = if edge.source_memory_id == req.memory_id {
                req.kind
            } else {
                let (source_owner, source_kind) =
                    self.load_memory_owner_kind(edge.source_memory_id).await?;
                self.authorize_write(
                    authz,
                    &source_owner,
                    write_relation_for_entity_kind(source_kind),
                )
                .await?;
                source_kind
            };
            let source_owner = if edge.source_memory_id == req.memory_id {
                req.owner
            } else {
                self.storage()
                    .memory_authoring
                    .owner_access_read
                    .home_owner(EntityId::Memory(edge.source_memory_id))
                    .await
                    .map_err(|err| ProtocolError::internal(err.to_string()))?
                    .ok_or_else(|| ProtocolError::invalid_argument("edges", "memory not found"))?
            };
            let (target_owner, target_kind) = if edge.target_memory_id == req.memory_id {
                (req.owner, req.kind)
            } else {
                self.authorize_edge_target_policy(authz, edge.relation, edge.target_memory_id)
                    .await?
            };
            validate_relation_owner_policy(edge.relation, &source_owner, &target_owner, "edges")?;
            validate_relation_shape(
                edge.relation,
                source_kind,
                target_kind,
                edge.authorship_kind,
                "edges",
            )?;
            out.push(AuthorDerivedEdgeInput {
                relation: edge.relation,
                source_kind,
                source_memory_id: edge.source_memory_id,
                target_kind,
                target_memory_id: edge.target_memory_id,
                authorship_kind: edge.authorship_kind,
                authorship_owner_memory_id: edge.authorship_owner_memory_id,
            });
        }
        Ok(out)
    }

    async fn load_memory_owner_kind(
        &self,
        memory_id: MemoryId,
    ) -> Result<(Owner, EntityKind), ProtocolError> {
        let owner = self
            .storage()
            .memory_authoring
            .owner_access_read
            .home_owner(EntityId::Memory(memory_id))
            .await
            .map_err(|err| ProtocolError::internal(err.to_string()))?
            .ok_or_else(|| ProtocolError::invalid_argument("memory_id", "memory not found"))?;
        let kind = self.load_required_memory_kind(&owner, memory_id).await?;
        Ok((owner, kind))
    }

    async fn authorize_edge_target_policy(
        &self,
        authz: &AuthzContext,
        relation: RegisteredRelation<'_>,
        target_memory_id: MemoryId,
    ) -> Result<(Owner, EntityKind), ProtocolError> {
        match relation.descriptor.target_access_policy {
            RelationTargetAccessPolicy::None => self.load_memory_owner_kind(target_memory_id).await,
            RelationTargetAccessPolicy::Read => {
                let target_read = self
                    .authorize_entry_read(authz, EntityId::Memory(target_memory_id))
                    .await?;
                let target_kind = self
                    .load_required_memory_kind(target_read.owner(), target_memory_id)
                    .await?;
                Ok((*target_read.owner(), target_kind))
            }
            RelationTargetAccessPolicy::Write => {
                let (target_owner, target_kind) =
                    self.load_memory_owner_kind(target_memory_id).await?;
                self.authorize_write(
                    authz,
                    &target_owner,
                    write_relation_for_entity_kind(target_kind),
                )
                .await?;
                Ok((target_owner, target_kind))
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
            .map(|row| (row.memory_id, memory_kind_for_edge(row.kind)))
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

fn validate_operator_memory_invocation_request(
    req: &AuthorDerivedRequestInput<'_>,
) -> Result<(), StorageError> {
    match req.operator_kind {
        MemoryOperatorKind::FtoA if req.source_batch_id.is_none() => {
            return Err(StorageError::ConstraintViolation(
                "F→A operator invocation requires source_batch_id".into(),
            ));
        }
        MemoryOperatorKind::AtoA | MemoryOperatorKind::AtoP if req.source_batch_id.is_some() => {
            return Err(StorageError::ConstraintViolation(
                "source_batch_id is only valid for F→A operator invocations".into(),
            ));
        }
        MemoryOperatorKind::FtoA | MemoryOperatorKind::AtoA | MemoryOperatorKind::AtoP => {}
    }

    let output_edges = req
        .edges
        .iter()
        .map(|edge| {
            OutputEdgeManifest::memory_to_memory(
                edge.source_memory_id,
                edge.target_memory_id,
                edge.authorship_kind,
            )
        })
        .collect::<Vec<_>>();
    let inputs = req
        .edges
        .iter()
        .map(|edge| (edge.target_memory_id, edge.target_kind))
        .collect::<Vec<_>>();
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
        .map_err(|err| StorageError::ConstraintViolation(err.to_string()))?;
    for edge in req.edges {
        if edge.source_memory_id != req.memory_id {
            return Err(StorageError::ConstraintViolation(
                "operator provenance edge source must be the output memory".into(),
            ));
        }
    }
    Ok(())
}

fn write_relation_for_entity_kind(kind: EntityKind) -> Relation {
    match kind {
        EntityKind::Fact => Relation::Ingest,
        EntityKind::Abstraction | EntityKind::Perspective => Relation::Editor,
        EntityKind::Goal => Relation::Admin,
    }
}

fn validate_relation_owner_policy(
    relation: RegisteredRelation<'_>,
    source_owner: &Owner,
    target_owner: &Owner,
    field: &str,
) -> Result<(), ProtocolError> {
    match relation.descriptor.owner_policy {
        RelationOwnerPolicy::SourceOwned => Ok(()),
        RelationOwnerPolicy::SameOwner if source_owner == target_owner => Ok(()),
        RelationOwnerPolicy::SameOwner => Err(ProtocolError::invalid_argument(
            field,
            format!(
                "relation {} requires source and target to have the same owner",
                relation.descriptor.relation
            ),
        )),
    }
}

fn memory_kind_for_edge(kind: Option<EntityKind>) -> EntityKind {
    match kind {
        Some(EntityKind::Abstraction) => EntityKind::Abstraction,
        Some(EntityKind::Perspective) => EntityKind::Perspective,
        None | Some(_) => EntityKind::Fact,
    }
}

fn validate_relation_shape(
    relation: RegisteredRelation<'_>,
    source_kind: EntityKind,
    target_kind: EntityKind,
    authorship_kind: EdgeAuthorshipKind,
    field: &str,
) -> Result<(), ProtocolError> {
    relation
        .descriptor
        .validate_edge_shape(
            source_kind.as_str(),
            EndpointBinding::Pin,
            target_kind.as_str(),
            EndpointBinding::Pin,
            authorship_kind.as_str(),
        )
        .map_err(|err| ProtocolError::invalid_argument(field, err))?;
    validate_operator_edge_shape(
        relation.descriptor.class,
        source_kind,
        target_kind,
        authorship_kind,
    )
    .map_err(|err| ProtocolError::invalid_argument(field, err))
}

#[cfg(test)]
mod tests {
    use crate::authz::AuthzContext;
    use crate::error::ErrorCode;
    use crate::{
        AbstractionPayload, AgentDerivationV1, CORE_DERIVED_FROM_RELATION, EntityKind,
        FlavorRegistry, OwnerRef, UserId,
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

    #[tokio::test]
    async fn author_derived_authorized_denies_denied_context() {
        let engine = engine();
        let owner = owner();
        let err = engine
            .author_derived_authorized(
                &AuthzContext::denied_for_owner(&owner),
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
                    supersedes: None,
                    edges: &[],
                },
            )
            .await
            .expect_err("denied context must fail before storage");

        assert_eq!(err.code, ErrorCode::Forbidden);
    }

    #[tokio::test]
    async fn append_memory_edge_authorized_rejects_missing_source_before_authz() {
        let engine = engine();
        let owner = owner();
        let relation = engine
            .registry()
            .resolve_relation(CORE_DERIVED_FROM_RELATION)
            .expect("core relation registered");
        let err = engine
            .append_memory_edge_authorized(
                &AuthzContext::denied_for_owner(&owner),
                AppendMemoryEdgeRequestInput {
                    owner,
                    relation,
                    source_memory_id: MemoryId::new(uuid::Uuid::now_v7()),
                    target_memory_id: MemoryId::new(uuid::Uuid::now_v7()),
                    authorship_kind: EdgeAuthorshipKind::OperatorFtoA,
                    authorship_owner_memory_id: None,
                    sidecar_payload: None,
                },
            )
            .await
            .expect_err("source-owned admission must load an existing source owner");

        assert_eq!(err.code, ErrorCode::InvalidArgument);
    }
}

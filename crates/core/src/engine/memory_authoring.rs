use super::Engine;
use crate::access::Relation;
use crate::authz::AuthzContext;
use crate::error::ProtocolError;
use crate::storage::{AuthorDerivedOutcome, AuthorDerivedRequest, DerivedEdgeSpec, StorageError};
use crate::{
    CORE_SUPERSEDES_RELATION, EdgeAuthorshipKind, EdgeId, EndpointBinding, EntityId, EntityKind,
    MemoryId, MemoryOperatorKind, Owner, PersonalityInstanceId, RegisteredRelation, SchemaId,
    SchemaVersion, SidecarPayload,
};

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
    pub model_id: &'a str,
    pub prompt_version: &'a str,
    pub author_personality_instance_id: Option<PersonalityInstanceId>,
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
                model_id: req.model_id,
                prompt_version: req.prompt_version,
                author_personality_instance_id: req.author_personality_instance_id,
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
        let write_permit = self
            .authorize_write(authz, &req.owner, Relation::Editor)
            .await?;
        let owner = *write_permit.owner();
        let source_kind = self
            .load_required_memory_kind(&owner, req.source_memory_id)
            .await?;
        let target_read = self
            .authorize_entry_read(authz, EntityId::Memory(req.target_memory_id))
            .await?;
        let target_kind = self
            .load_required_memory_kind(target_read.owner(), req.target_memory_id)
            .await?;
        validate_relation_shape(
            req.relation,
            source_kind,
            target_kind,
            req.authorship_kind,
            "edge",
        )?;

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
            .append_memory_edge(&edge)
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
    pub async fn author_derived(
        &self,
        req: AuthorDerivedRequestInput<'_>,
    ) -> Result<AuthorDerivedOutcome, StorageError> {
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
            model_id: req.model_id,
            prompt_version: req.prompt_version,
            author_personality_instance_id: req.author_personality_instance_id,
            sidecar_payload: req.sidecar_payload,
            supersedes: req.supersedes,
            embedding,
            embedding_model_id: embedding_model_id.as_deref(),
            edges: &edges,
        };
        self.storage()
            .memory_authoring
            .memory_authoring
            .author_derived(&storage_req)
            .await
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
                let source_owner = self
                    .storage()
                    .memory_authoring
                    .owner_access_read
                    .home_owner(EntityId::Memory(edge.source_memory_id))
                    .await
                    .map_err(|err| ProtocolError::internal(err.to_string()))?
                    .ok_or_else(|| ProtocolError::invalid_argument("edges", "memory not found"))?;
                self.authorize_write(authz, &source_owner, Relation::Editor)
                    .await?;
                self.load_required_memory_kind(&source_owner, edge.source_memory_id)
                    .await?
            };
            let target_kind = if edge.target_memory_id == req.memory_id {
                req.kind
            } else {
                let target_read = self
                    .authorize_entry_read(authz, EntityId::Memory(edge.target_memory_id))
                    .await?;
                self.load_required_memory_kind(target_read.owner(), edge.target_memory_id)
                    .await?
            };
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
        Engine::new(FlavorRegistry::new().freeze())
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
                    operator_kind: MemoryOperatorKind::ExternalAgent,
                    model_id: "test-model",
                    prompt_version: "test",
                    author_personality_instance_id: None,
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
    async fn append_memory_edge_authorized_denies_denied_context() {
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
                    authorship_kind: EdgeAuthorshipKind::ExternalAgent,
                    authorship_owner_memory_id: None,
                    sidecar_payload: None,
                },
            )
            .await
            .expect_err("denied context must fail before storage");

        assert_eq!(err.code, ErrorCode::Forbidden);
    }
}

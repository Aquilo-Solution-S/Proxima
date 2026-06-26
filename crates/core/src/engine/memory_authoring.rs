use super::Engine;
use crate::authz::{AuthzContext, MemoryAction, Role};
use crate::error::ProtocolError;
use crate::storage::{AuthorDerivedOutcome, AuthorDerivedRequest, DerivedEdgeSpec, StorageError};
use crate::verbs::event_ingest::EventDraft;
use crate::{
    AgentNoteV1, CORE_SUPERSEDES_RELATION, EdgeAuthorshipKind, EdgeId, EndpointBinding, EntityKind,
    FactPayload, MemoryId, MemoryOperatorKind, Owner, PersonalityInstanceId, RegisteredRelation,
    SchemaId, SchemaVersion, SidecarPayload, SourceBatchId,
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

#[derive(Debug, Clone)]
pub struct PublishMemoryRequestInput {
    pub source_owner: Owner,
    pub target_owner: Owner,
    pub memory_id: MemoryId,
    pub title_override: Option<String>,
    pub body_override: Option<String>,
    pub tags: Vec<String>,
    pub author_personality_instance_id: Option<PersonalityInstanceId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishMemoryOutcome {
    pub source_memory_id: MemoryId,
    pub published_memory_id: MemoryId,
}

impl Engine {
    /// Authorized graph-write verb for agent-authored derived memory.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context lacks graph-read/read or
    /// graph-write/write on the owner; `InvalidArgument` when referenced
    /// memories are not present in that owner space or edge shape validation
    /// fails; and `Internal` for storage failures.
    pub async fn author_derived_authorized(
        &self,
        authz: &AuthzContext,
        req: AuthorDerivedRequestInput<'_>,
    ) -> Result<AuthorDerivedAuthorizedOutcome, ProtocolError> {
        let read_permit =
            self.authorize_request(authz, &req.owner, Role::GraphRead, MemoryAction::Read)?;
        let write_permit =
            self.authorize_request(authz, &req.owner, Role::GraphWrite, MemoryAction::Write)?;
        if read_permit.owner() != write_permit.owner() {
            return Err(ProtocolError::forbidden(
                "read and write permits resolved to different owners",
            ));
        }

        let owner = write_permit.owner().clone();
        let edges = self.validated_author_derived_edges(&owner, &req).await?;
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
                owner: owner.clone(),
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
    /// Returns `Forbidden` when the context lacks graph-read/read or
    /// graph-write/write on the owner; `InvalidArgument` when endpoints are
    /// absent from the owner space or the relation rejects the shape; and
    /// `Internal` for storage failures.
    pub async fn append_memory_edge_authorized(
        &self,
        authz: &AuthzContext,
        req: AppendMemoryEdgeRequestInput<'_>,
    ) -> Result<EdgeId, ProtocolError> {
        let read_permit =
            self.authorize_request(authz, &req.owner, Role::GraphRead, MemoryAction::Read)?;
        let write_permit =
            self.authorize_request(authz, &req.owner, Role::GraphWrite, MemoryAction::Write)?;
        if read_permit.owner() != write_permit.owner() {
            return Err(ProtocolError::forbidden(
                "read and write permits resolved to different owners",
            ));
        }
        let owner = write_permit.owner().clone();
        let kinds = self
            .load_required_memory_kinds(&owner, &[req.source_memory_id, req.target_memory_id])
            .await?;
        let source_kind = kinds[0];
        let target_kind = kinds[1];
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
            .append_memory_edge(&edge)
            .await
            .map_err(|err| ProtocolError::internal(err.to_string()))
    }

    /// Authorized owner-to-owner publication of a core agent-note Fact.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the caller lacks source read, source publish,
    /// or target write; `InvalidArgument` for unsupported source memory payloads;
    /// and `Internal` for storage failures.
    pub async fn publish_memory(
        &self,
        authz: &AuthzContext,
        req: PublishMemoryRequestInput,
    ) -> Result<PublishMemoryOutcome, ProtocolError> {
        let source_read = self.authorize_request(
            authz,
            &req.source_owner,
            Role::GraphRead,
            MemoryAction::Read,
        )?;
        let source_publish = self.authorize_request(
            authz,
            &req.source_owner,
            Role::GraphWrite,
            MemoryAction::Publish,
        )?;
        let target_write = self.authorize_request(
            authz,
            &req.target_owner,
            Role::GraphWrite,
            MemoryAction::Write,
        )?;
        if source_read.owner() != source_publish.owner() {
            return Err(ProtocolError::forbidden(
                "source read and publish permits resolved to different owners",
            ));
        }

        let sidecars = self.sidecar_specs();
        let snapshot = self
            .storage()
            .load_memory_by_id(source_read.owner(), req.memory_id, None, &sidecars)
            .await
            .map_err(|err| ProtocolError::internal(err.to_string()))?
            .ok_or_else(|| ProtocolError::invalid_argument("memory_id", "memory not found"))?;
        if snapshot.schema_id != AgentNoteV1::schema_id() {
            return Err(ProtocolError::invalid_argument(
                "memory_id",
                "core_publish_memory v1 supports only core/agent-note-v1",
            ));
        }
        let Some(payload) = snapshot
            .payload
            .as_ref()
            .and_then(SidecarPayload::downcast_ref::<AgentNoteV1>)
        else {
            return Err(ProtocolError::internal("agent note payload missing"));
        };

        let copied = AgentNoteV1 {
            note_id: uuid::Uuid::now_v7(),
            title: req.title_override.unwrap_or_else(|| payload.title.clone()),
            body: req.body_override.unwrap_or_else(|| payload.body.clone()),
            tags: if req.tags.is_empty() {
                payload.tags.clone()
            } else {
                req.tags
            },
            idempotency_key: None,
        };
        let observed_at = time::OffsetDateTime::now_utc();
        let mut draft = EventDraft::from_payload(
            target_write.owner(),
            "core/agent-publish",
            SourceBatchId::new(uuid::Uuid::now_v7()),
            &copied,
            observed_at,
        );
        if let Some(author) = req.author_personality_instance_id {
            draft = draft.author_personality(author);
        }
        let authorized = self.authorize_event_ingest(authz, Role::GraphWrite, draft)?;
        let embedding_client = self.embed_client();
        let embedding_model_id = embedding_client.as_ref().map(|client| client.model_id());
        let outcome = self
            .ingest_event_with_typed_sidecar(
                &authorized,
                &SidecarPayload::fact(copied),
                embedding_model_id,
            )
            .await?;

        Ok(PublishMemoryOutcome {
            source_memory_id: snapshot.memory_id,
            published_memory_id: outcome.memory_id,
        })
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
            owner: owner.clone(),
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
        self.storage().author_derived(&storage_req).await
    }

    async fn validated_author_derived_edges<'a>(
        &self,
        owner: &Owner,
        req: &AuthorDerivedRequestInput<'a>,
    ) -> Result<Vec<AuthorDerivedEdgeInput<'a>>, ProtocolError> {
        if req.edges.is_empty() {
            return Ok(Vec::new());
        }
        let mut existing = Vec::with_capacity(req.edges.len() * 2);
        for edge in req.edges {
            if edge.source_memory_id != req.memory_id {
                existing.push(edge.source_memory_id);
            }
            if edge.target_memory_id != req.memory_id {
                existing.push(edge.target_memory_id);
            }
        }
        existing.sort_by_key(|memory_id| memory_id.into_inner());
        existing.dedup();
        let loaded = self.load_required_memory_kinds(owner, &existing).await?;
        let by_id = existing
            .into_iter()
            .zip(loaded)
            .collect::<std::collections::HashMap<_, _>>();

        let mut out = Vec::with_capacity(req.edges.len());
        for edge in req.edges {
            let source_kind = if edge.source_memory_id == req.memory_id {
                req.kind
            } else {
                *by_id.get(&edge.source_memory_id).ok_or_else(|| {
                    ProtocolError::invalid_argument(
                        "edges",
                        "cross-space derive/link is not supported; choose one memory space",
                    )
                })?
            };
            let target_kind = if edge.target_memory_id == req.memory_id {
                req.kind
            } else {
                *by_id.get(&edge.target_memory_id).ok_or_else(|| {
                    ProtocolError::invalid_argument(
                        "edges",
                        "cross-space derive/link is not supported; choose one memory space",
                    )
                })?
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
    use crate::authz::{
        AuthPath, AuthzContext, CapabilitySet, Identity, MemoryActionSet, MemorySpaceGrant,
        MemorySpaceGrants, RoleSet, ToolScope,
    };
    use crate::error::ErrorCode;
    use crate::{
        AbstractionPayload, AgentDerivationV1, CORE_DERIVED_FROM_RELATION, EntityKind,
        FlavorRegistry, Principal, UserId,
    };

    use super::*;

    fn owner() -> Owner {
        Principal::User(UserId::new(uuid::Uuid::now_v7()))
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

    fn publish_authz(owner: &Owner, read: bool, publish: bool, write: bool) -> AuthzContext {
        let mut accessible_principals = std::collections::HashSet::new();
        accessible_principals.insert(owner.clone());
        AuthzContext {
            identity: Identity {
                principal: owner.clone(),
                accessible_principals,
                expires_at: None,
                auth_epoch: 0,
            },
            capabilities: CapabilitySet {
                tool_scope: ToolScope::All,
                roles: RoleSet {
                    graph_read: true,
                    graph_write: true,
                    source_ingest: false,
                    admin: false,
                },
                memory_spaces: MemorySpaceGrants::explicit(vec![MemorySpaceGrant {
                    key: "test".into(),
                    label: "Test".into(),
                    owner: owner.clone(),
                    actions: MemoryActionSet {
                        search: false,
                        read,
                        write,
                        publish,
                        admin: false,
                    },
                }]),
            },
            auth_path: AuthPath::HostBearer,
        }
    }

    #[tokio::test]
    async fn author_derived_authorized_denies_denied_context() {
        let engine = engine();
        let owner = owner();
        let err = engine
            .author_derived_authorized(
                &AuthzContext::denied(&owner),
                AuthorDerivedRequestInput {
                    memory_id: MemoryId::new(uuid::Uuid::now_v7()),
                    owner: owner.clone(),
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
                &AuthzContext::denied(&owner),
                AppendMemoryEdgeRequestInput {
                    owner: owner.clone(),
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

    #[tokio::test]
    async fn publish_memory_denies_denied_context() {
        let engine = engine();
        let owner = owner();
        let err = engine
            .publish_memory(
                &AuthzContext::denied(&owner),
                PublishMemoryRequestInput {
                    source_owner: owner.clone(),
                    target_owner: owner.clone(),
                    memory_id: MemoryId::new(uuid::Uuid::now_v7()),
                    title_override: None,
                    body_override: None,
                    tags: Vec::new(),
                    author_personality_instance_id: None,
                },
            )
            .await
            .expect_err("denied context must fail before storage");

        assert_eq!(err.code, ErrorCode::Forbidden);
    }

    #[tokio::test]
    async fn publish_memory_denies_when_any_required_grant_is_missing() {
        let engine = engine();
        let owner = owner();

        for (read, publish, write) in [
            (false, true, true),
            (true, false, true),
            (true, true, false),
        ] {
            let err = engine
                .publish_memory(
                    &publish_authz(&owner, read, publish, write),
                    PublishMemoryRequestInput {
                        source_owner: owner.clone(),
                        target_owner: owner.clone(),
                        memory_id: MemoryId::new(uuid::Uuid::now_v7()),
                        title_override: None,
                        body_override: None,
                        tags: Vec::new(),
                        author_personality_instance_id: None,
                    },
                )
                .await
                .expect_err("missing required grant must fail before storage");

            assert_eq!(err.code, ErrorCode::Forbidden);
        }
    }
}

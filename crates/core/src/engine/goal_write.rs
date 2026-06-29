use super::{Engine, pipeline::WritePermit};
use crate::access::{EntityId, Relation};
use crate::authz::AuthzContext;
use crate::error::ProtocolError;
use crate::storage::StorageError;
use crate::verbs::goal_write::{
    AchieveGoalAtomicRequest, ChildGoalDraft, CreateGoalAtomicRequest, DecomposeGoalAtomicRequest,
    DecomposeGoalOutcome, GoalAtomicContext, GoalAuthorship, GoalCreateRequest, GoalDraft,
    GoalPayloadWrite, GoalState, GoalWriteBuildError, GoalWriteOutcome, IdempotencyKey,
    ModifyGoalAtomicRequest, TransitionGoalAtomicRequest,
};
use crate::verbs::schema::PayloadKind;
use crate::{EntityKind, GoalPayload, MemoryId, PersonalityInstanceId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalTargetSelf {
    SelfPerspective(MemoryId),
    Personality(PersonalityInstanceId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalCreatePayloadWriteRequest {
    pub principal: crate::OwnerRef,
    pub target_self: GoalTargetSelf,
    pub payload: GoalPayloadWrite,
    pub request_id: IdempotencyKey,
    pub evidence: Vec<crate::verbs::goal_write::GoalEvidenceRef>,
    pub parent_goal_ids: Vec<crate::GoalId>,
    pub authorship: GoalAuthorship,
    pub author_self_perspective_id: Option<MemoryId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalTransitionRequest {
    pub principal: crate::OwnerRef,
    pub prior_goal_id: crate::GoalId,
    pub next_state: GoalState,
    pub authorship: GoalAuthorship,
    pub request_id: IdempotencyKey,
    pub author_self_perspective_id: Option<MemoryId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalMarkAchievedRequest {
    pub principal: crate::OwnerRef,
    pub prior_goal_id: crate::GoalId,
    pub authorship: GoalAuthorship,
    pub request_id: IdempotencyKey,
    pub evidence: Vec<crate::verbs::goal_write::GoalEvidenceRef>,
    pub author_self_perspective_id: Option<MemoryId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalModifyRequest {
    pub principal: crate::OwnerRef,
    pub prior_goal_id: crate::GoalId,
    pub replacement: GoalPayloadWrite,
    pub authorship: GoalAuthorship,
    pub request_id: IdempotencyKey,
    pub evidence: Option<Vec<crate::verbs::goal_write::GoalEvidenceRef>>,
    pub author_self_perspective_id: Option<MemoryId>,
}

#[derive(Debug)]
pub struct GoalDecomposeRequest {
    pub principal: crate::OwnerRef,
    pub parent_goal_id: crate::GoalId,
    pub authorship: GoalAuthorship,
    pub target_self: GoalTargetSelf,
    pub children: Vec<ChildGoalDraft>,
    pub author_self_perspective_id: Option<MemoryId>,
}

impl Engine {
    /// Create an Active typed Goal for an embedded host or protocol
    /// caller without exposing `proxima_core.goals` storage shape.
    ///
    /// The request must name the target Self Perspective explicitly;
    /// current Proxima Goal assignment is `Goal --core/inspires--> Self`,
    /// not a detached owner-scoped Goal row.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when `authz` cannot access the request Owner or
    /// lacks [`Relation::Editor`] on the owner space;
    /// `UnknownSchema` when the typed [`GoalPayload`] schema is not registered
    /// as a Goal; `InvalidArgument` for malformed title/text/evidence/parent
    /// references; or `Internal` for storage failures.
    pub async fn create_goal<P>(
        &self,
        authz: &AuthzContext,
        request: GoalCreateRequest<P>,
    ) -> Result<GoalWriteOutcome, ProtocolError>
    where
        P: GoalPayload,
    {
        let permit = self
            .authorize_write(authz, &request.principal, Relation::Editor)
            .await?;
        self.create_goal_authorized(authz, &permit, request).await
    }

    /// Create an Active Goal from a dynamic protocol payload.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when `authz` cannot access the request Owner or
    /// lacks [`Relation::Editor`] on the owner space;
    /// `UnknownSchema` when the payload schema is not registered as a Goal;
    /// `InvalidArgument` for malformed target/evidence/parent references; or
    /// `Internal` for storage failures.
    pub async fn create_goal_from_payload_write(
        &self,
        authz: &AuthzContext,
        req: &GoalCreatePayloadWriteRequest,
    ) -> Result<GoalWriteOutcome, ProtocolError> {
        let permit = self
            .authorize_write(authz, &req.principal, Relation::Editor)
            .await?;
        let payload = self.normalize_payload_write(req.payload.clone())?;
        let target_self = self
            .target_self_perspective_authorized(authz, &permit, req.target_self)
            .await?;
        let author_self_perspective_id = self
            .author_self_perspective_authorized(authz, req.author_self_perspective_id)
            .await?;
        let draft = GoalDraft::active_from_payload_write(
            *permit.owner(),
            payload,
            req.parent_goal_ids.clone(),
            req.authorship.clone(),
            req.request_id.clone(),
        );
        let embedding_client = self.embed_client();
        let context =
            self.goal_atomic_context(embedding_client.as_ref(), author_self_perspective_id);
        self.storage()
            .create_goal_atomic(&CreateGoalAtomicRequest {
                draft,
                context,
                target_self_perspective_id: target_self,
                evidence: req.evidence.clone(),
            })
            .await
            .map_err(map_goal_storage_error)
    }

    /// Transition a Goal head to Paused, Active, or Abandoned.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when `authz` cannot access the request Owner or
    /// lacks [`Relation::Editor`]; `InvalidArgument`
    /// or `NotFound` for rejected goal references; or `Internal` for storage
    /// failures.
    pub async fn transition_goal(
        &self,
        authz: &AuthzContext,
        req: &GoalTransitionRequest,
    ) -> Result<GoalWriteOutcome, ProtocolError> {
        let permit = self
            .authorize_write(authz, &req.principal, Relation::Editor)
            .await?;
        let author_self_perspective_id = self
            .author_self_perspective_authorized(authz, req.author_self_perspective_id)
            .await?;
        let embedding_client = self.embed_client();
        let context =
            self.goal_atomic_context(embedding_client.as_ref(), author_self_perspective_id);
        self.storage()
            .transition_goal_atomic(&TransitionGoalAtomicRequest {
                owner: *permit.owner(),
                prior_goal_id: req.prior_goal_id,
                next_state: req.next_state,
                authorship: req.authorship.clone(),
                request_id: req.request_id.clone(),
                context,
            })
            .await
            .map_err(map_goal_storage_error)
    }

    /// Mark a Goal head Achieved with evidence.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when `authz` cannot access the request Owner or
    /// lacks [`Relation::Editor`]; `InvalidArgument`
    /// or `NotFound` for rejected references; or `Internal` for storage
    /// failures.
    pub async fn mark_goal_achieved(
        &self,
        authz: &AuthzContext,
        req: &GoalMarkAchievedRequest,
    ) -> Result<GoalWriteOutcome, ProtocolError> {
        let permit = self
            .authorize_write(authz, &req.principal, Relation::Editor)
            .await?;
        let author_self_perspective_id = self
            .author_self_perspective_authorized(authz, req.author_self_perspective_id)
            .await?;
        let embedding_client = self.embed_client();
        let context =
            self.goal_atomic_context(embedding_client.as_ref(), author_self_perspective_id);
        self.storage()
            .achieve_goal_atomic(&AchieveGoalAtomicRequest {
                owner: *permit.owner(),
                prior_goal_id: req.prior_goal_id,
                authorship: req.authorship.clone(),
                request_id: req.request_id.clone(),
                context,
                evidence: req.evidence.clone(),
            })
            .await
            .map_err(map_goal_storage_error)
    }

    /// Replace an Active Goal head's content.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when `authz` cannot access the request Owner or
    /// lacks [`Relation::Editor`]; `UnknownSchema`
    /// when the replacement payload schema is not registered as a Goal;
    /// `InvalidArgument` or `NotFound` for rejected references; or `Internal`
    /// for storage failures.
    pub async fn modify_goal(
        &self,
        authz: &AuthzContext,
        req: &GoalModifyRequest,
    ) -> Result<GoalWriteOutcome, ProtocolError> {
        let permit = self
            .authorize_write(authz, &req.principal, Relation::Editor)
            .await?;
        let author_self_perspective_id = self
            .author_self_perspective_authorized(authz, req.author_self_perspective_id)
            .await?;
        let embedding_client = self.embed_client();
        let context =
            self.goal_atomic_context(embedding_client.as_ref(), author_self_perspective_id);
        self.storage()
            .modify_goal_atomic(&ModifyGoalAtomicRequest {
                owner: *permit.owner(),
                prior_goal_id: req.prior_goal_id,
                replacement: self.normalize_payload_write(req.replacement.clone())?,
                authorship: req.authorship.clone(),
                request_id: req.request_id.clone(),
                context,
                evidence: req.evidence.clone(),
            })
            .await
            .map_err(map_goal_storage_error)
    }

    /// Create Active child Goals under a parent Goal.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when `authz` cannot access the request Owner or
    /// lacks [`Relation::Editor`]; `UnknownSchema`
    /// when any child payload schema is not registered as a Goal;
    /// `InvalidArgument` or `NotFound` for rejected references; or `Internal`
    /// for storage failures.
    pub async fn decompose_goal(
        &self,
        authz: &AuthzContext,
        req: &GoalDecomposeRequest,
    ) -> Result<DecomposeGoalOutcome, ProtocolError> {
        let permit = self
            .authorize_write(authz, &req.principal, Relation::Editor)
            .await?;
        let target_self = self
            .target_self_perspective_authorized(authz, &permit, req.target_self)
            .await?;
        let mut children = Vec::with_capacity(req.children.len());
        for child in &req.children {
            children.push(ChildGoalDraft {
                payload: self.normalize_payload_write(child.payload.clone())?,
                evidence: child.evidence.clone(),
                request_id: child.request_id.clone(),
            });
        }
        let author_self_perspective_id = self
            .author_self_perspective_authorized(authz, req.author_self_perspective_id)
            .await?;
        let embedding_client = self.embed_client();
        let context =
            self.goal_atomic_context(embedding_client.as_ref(), author_self_perspective_id);
        self.storage()
            .decompose_goal_atomic(&DecomposeGoalAtomicRequest {
                owner: *permit.owner(),
                parent_goal_id: req.parent_goal_id,
                authorship: req.authorship.clone(),
                context,
                target_self_perspective_id: target_self,
                children,
            })
            .await
            .map_err(map_goal_storage_error)
    }

    async fn create_goal_authorized<P>(
        &self,
        authz: &AuthzContext,
        permit: &WritePermit,
        request: GoalCreateRequest<P>,
    ) -> Result<GoalWriteOutcome, ProtocolError>
    where
        P: GoalPayload,
    {
        let GoalCreateRequest {
            principal: _,
            target_self_perspective_id,
            title,
            text,
            payload,
            request_id,
            evidence,
            parent_goal_ids,
            authorship,
            author_self_perspective_id,
        } = request;

        let target_self_perspective_id = self
            .target_self_memory_authorized(authz, target_self_perspective_id)
            .await?;
        let author_self_perspective_id = self
            .author_self_perspective_authorized(authz, author_self_perspective_id)
            .await?;

        let mut payload_write =
            GoalPayloadWrite::from_payload(title, text, payload).map_err(map_goal_build_error)?;
        let schema = self
            .registry()
            .lookup_payload(
                &payload_write.schema_id,
                payload_write.schema_version,
                PayloadKind::Goal,
            )
            .ok_or_else(|| {
                ProtocolError::unknown_schema(
                    payload_write.schema_id.as_str(),
                    payload_write.schema_version.into_inner(),
                )
            })?;
        if schema.sidecar_table.is_none() {
            payload_write.sidecar_payload = None;
        }

        let embedding_client = self.embed_client();
        let embedding_model_id = embedding_client.as_ref().map(|client| client.model_id());
        let draft = GoalDraft::active_from_payload_write(
            *permit.owner(),
            payload_write,
            parent_goal_ids,
            authorship,
            request_id,
        );
        let outcome = self
            .storage()
            .create_goal_atomic(&CreateGoalAtomicRequest {
                draft,
                context: GoalAtomicContext {
                    registry: self.registry(),
                    embedding_model_id,
                    author_self_perspective_id,
                },
                target_self_perspective_id,
                evidence,
            })
            .await
            .map_err(map_goal_storage_error)?;
        Ok(outcome)
    }

    fn goal_atomic_context<'a>(
        &'a self,
        embedding_client: Option<&'a std::sync::Arc<dyn crate::llm::EmbeddingClient>>,
        author_self_perspective_id: Option<MemoryId>,
    ) -> GoalAtomicContext<'a> {
        let embedding_model_id = embedding_client.map(|client| client.model_id());
        GoalAtomicContext {
            registry: self.registry(),
            embedding_model_id,
            author_self_perspective_id,
        }
    }

    fn normalize_payload_write(
        &self,
        mut payload_write: GoalPayloadWrite,
    ) -> Result<GoalPayloadWrite, ProtocolError> {
        let schema = self
            .registry()
            .lookup_payload(
                &payload_write.schema_id,
                payload_write.schema_version,
                PayloadKind::Goal,
            )
            .ok_or_else(|| {
                ProtocolError::unknown_schema(
                    payload_write.schema_id.as_str(),
                    payload_write.schema_version.into_inner(),
                )
            })?;
        if schema.sidecar_table.is_none() {
            payload_write.sidecar_payload = None;
        }
        Ok(payload_write)
    }

    async fn target_self_perspective_authorized(
        &self,
        authz: &AuthzContext,
        permit: &WritePermit,
        target_self: GoalTargetSelf,
    ) -> Result<MemoryId, ProtocolError> {
        match target_self {
            GoalTargetSelf::SelfPerspective(memory_id) => {
                self.target_self_memory_authorized(authz, memory_id).await
            }
            GoalTargetSelf::Personality(instance_id) => self
                .storage()
                .active_personality_root(permit.owner(), instance_id)
                .await
                .map_err(map_goal_storage_error)?
                .ok_or_else(|| {
                    ProtocolError::invalid_argument(
                        "target_personality",
                        "target personality not found",
                    )
                }),
        }
    }

    async fn target_self_memory_authorized(
        &self,
        authz: &AuthzContext,
        memory_id: MemoryId,
    ) -> Result<MemoryId, ProtocolError> {
        let permit = self
            .authorize_entry_read(authz, EntityId::Memory(memory_id))
            .await?;
        self.require_perspective_kind(permit.owner(), memory_id, "target_self")
            .await?;
        Ok(memory_id)
    }

    async fn author_self_perspective_authorized(
        &self,
        authz: &AuthzContext,
        memory_id: Option<MemoryId>,
    ) -> Result<Option<MemoryId>, ProtocolError> {
        let Some(memory_id) = memory_id else {
            return Ok(None);
        };
        let home_owner = self
            .storage()
            .home_owner(EntityId::Memory(memory_id))
            .await
            .map_err(|err| ProtocolError::internal(format!("home_owner: {err}")))?
            .ok_or_else(|| ProtocolError::forbidden("entry not found"))?;
        self.authorize_write(authz, &home_owner, Relation::Editor)
            .await?;
        self.require_perspective_kind(&home_owner, memory_id, "author_self_perspective_id")
            .await?;
        Ok(Some(memory_id))
    }

    async fn require_perspective_kind(
        &self,
        owner: &crate::Owner,
        memory_id: MemoryId,
        field: &'static str,
    ) -> Result<(), ProtocolError> {
        let kind = self.load_required_memory_kind(owner, memory_id).await?;
        if kind != EntityKind::Perspective {
            return Err(ProtocolError::invalid_argument(
                field,
                "self perspective must be a Perspective",
            ));
        }
        Ok(())
    }
}

fn map_goal_build_error(err: GoalWriteBuildError) -> ProtocolError {
    match err {
        GoalWriteBuildError::InvalidTitle => {
            ProtocolError::invalid_argument("title", err.to_string())
        }
        GoalWriteBuildError::InvalidText => {
            ProtocolError::invalid_argument("text", err.to_string())
        }
    }
}

fn map_goal_storage_error(err: StorageError) -> ProtocolError {
    match err {
        StorageError::NotFound => ProtocolError::not_found("goal write referenced row not found"),
        StorageError::ConstraintViolation(message)
            if message.starts_with("idempotency_conflict:") =>
        {
            let request_id = message
                .strip_prefix("idempotency_conflict:")
                .unwrap_or(message.as_str());
            ProtocolError::idempotency_conflict(request_id)
        }
        StorageError::ConstraintViolation(message) | StorageError::Conflict(message) => {
            ProtocolError::invalid_argument("goal", message)
        }
        StorageError::Unavailable(message) | StorageError::Internal(message) => {
            ProtocolError::internal(message)
        }
        StorageError::V004ResetRequired { details } => ProtocolError::internal(details),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use super::super::access_sets::tests::MembershipStorage;
    use crate::error::ErrorCode;
    use crate::verbs::goal_write::{GoalEvidenceRef, SystemOrigin};
    use crate::{
        AuthPath, Engine, FlavorRegistry, GoalId, GoalPayload, GroupId, Owner, OwnerRef, SchemaId,
        SchemaVersion, ToolId, UserId,
    };

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    struct TestGoalPayload;

    impl GoalPayload for TestGoalPayload {
        const SCHEMA_ID: &'static str = "test/goal";
        const SCHEMA_VERSION: u32 = 1;

        fn goal_key(&self) -> Vec<u8> {
            b"test-goal".to_vec()
        }
    }

    fn engine() -> Engine {
        Engine::new(FlavorRegistry::new().freeze())
    }

    fn owner() -> crate::OwnerRef {
        crate::OwnerRef::Personal(crate::UserId::new(uuid::Uuid::now_v7()))
    }

    fn storage_with_memory(
        member: OwnerRef,
        home_owner: Owner,
        entity_readable: bool,
        memory_kind: EntityKind,
    ) -> MembershipStorage {
        MembershipStorage {
            member,
            group: GroupId::new(uuid::Uuid::now_v7()),
            membership_relation: Relation::Viewer,
            home_owner: Some(home_owner),
            entity_readable,
            memory_kind: Some(memory_kind),
        }
    }

    fn engine_with_storage(storage: MembershipStorage) -> Engine {
        Engine::compose(Arc::new(storage), |registry| {
            registry.add_goal_schema::<TestGoalPayload>();
        })
    }

    fn goal_id() -> GoalId {
        GoalId::new(uuid::Uuid::now_v7())
    }

    fn memory_id() -> MemoryId {
        MemoryId::new(uuid::Uuid::now_v7())
    }

    fn request_id(label: &str) -> IdempotencyKey {
        IdempotencyKey::new(label).expect("valid idempotency key")
    }

    fn payload_write() -> GoalPayloadWrite {
        GoalPayloadWrite {
            schema_id: SchemaId::new("test/goal".to_string()),
            schema_version: SchemaVersion::new(1),
            title: "Test".into(),
            text: "Test goal".into(),
            payload: Vec::new(),
            sidecar_payload: None,
        }
    }

    fn tool_authorship() -> GoalAuthorship {
        GoalAuthorship::System(SystemOrigin::Tool {
            tool_id: ToolId::new("test/tool"),
        })
    }

    fn assert_forbidden(err: &ProtocolError) {
        assert_eq!(err.code, ErrorCode::Forbidden);
    }

    #[tokio::test]
    async fn author_self_perspective_denies_foreign_write_owner() {
        let owner = owner();
        let foreign = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
        let memory_id = memory_id();
        let engine = engine_with_storage(storage_with_memory(
            owner,
            foreign,
            true,
            EntityKind::Perspective,
        ));

        let err = engine
            .author_self_perspective_authorized(
                &AuthzContext::single_owner(&owner, AuthPath::System),
                Some(memory_id),
            )
            .await
            .expect_err("foreign author Self perspective must require write authority");

        assert_forbidden(&err);
    }

    #[tokio::test]
    async fn author_self_perspective_allows_writable_perspective() {
        let owner = owner();
        let memory_id = memory_id();
        let engine = engine_with_storage(storage_with_memory(
            owner,
            owner,
            true,
            EntityKind::Perspective,
        ));

        let authorized = engine
            .author_self_perspective_authorized(
                &AuthzContext::single_owner(&owner, AuthPath::System),
                Some(memory_id),
            )
            .await
            .expect("writable Perspective should authorize");

        assert_eq!(authorized, Some(memory_id));
    }

    #[tokio::test]
    async fn create_goal_from_payload_write_denies_unreadable_self_perspective_target() {
        let owner = owner();
        let foreign = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
        let target = memory_id();
        let engine = engine_with_storage(storage_with_memory(
            owner,
            foreign,
            false,
            EntityKind::Perspective,
        ));
        let req = GoalCreatePayloadWriteRequest {
            principal: owner,
            target_self: GoalTargetSelf::SelfPerspective(target),
            payload: payload_write(),
            request_id: request_id("create-unreadable-target"),
            evidence: Vec::new(),
            parent_goal_ids: Vec::new(),
            authorship: tool_authorship(),
            author_self_perspective_id: None,
        };

        let err = engine
            .create_goal_from_payload_write(
                &AuthzContext::single_owner(&owner, AuthPath::System),
                &req,
            )
            .await
            .expect_err("unreadable target Self perspective must be rejected before write");

        assert_forbidden(&err);
    }

    #[tokio::test]
    async fn decompose_goal_denies_unreadable_self_perspective_target() {
        let owner = owner();
        let foreign = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
        let target = memory_id();
        let engine = engine_with_storage(storage_with_memory(
            owner,
            foreign,
            false,
            EntityKind::Perspective,
        ));
        let req = GoalDecomposeRequest {
            principal: owner,
            parent_goal_id: goal_id(),
            authorship: tool_authorship(),
            target_self: GoalTargetSelf::SelfPerspective(target),
            children: vec![ChildGoalDraft {
                payload: payload_write(),
                evidence: Vec::new(),
                request_id: request_id("decompose-unreadable-target"),
            }],
            author_self_perspective_id: None,
        };

        let err = engine
            .decompose_goal(&AuthzContext::single_owner(&owner, AuthPath::System), &req)
            .await
            .expect_err("unreadable target Self perspective must be rejected before write");

        assert_forbidden(&err);
    }

    #[tokio::test]
    async fn target_self_perspective_allows_readable_perspective() {
        let owner = owner();
        let target = memory_id();
        let engine = engine_with_storage(storage_with_memory(
            owner,
            owner,
            true,
            EntityKind::Perspective,
        ));
        let authz = AuthzContext::single_owner(&owner, AuthPath::System);
        let permit = engine
            .authorize_write(&authz, &owner, Relation::Editor)
            .await
            .expect("owner write should authorize");

        let authorized = engine
            .target_self_perspective_authorized(
                &authz,
                &permit,
                GoalTargetSelf::SelfPerspective(target),
            )
            .await
            .expect("readable Perspective should authorize");

        assert_eq!(authorized, target);
    }

    #[tokio::test]
    async fn create_goal_from_payload_write_denies_denied_context() {
        let owner = owner();
        let req = GoalCreatePayloadWriteRequest {
            principal: owner,
            target_self: GoalTargetSelf::SelfPerspective(memory_id()),
            payload: payload_write(),
            request_id: request_id("create"),
            evidence: Vec::new(),
            parent_goal_ids: Vec::new(),
            authorship: tool_authorship(),
            author_self_perspective_id: None,
        };
        let err = engine()
            .create_goal_from_payload_write(&AuthzContext::denied_for_owner(&owner), &req)
            .await
            .expect_err("denied context must fail before schema or storage");
        assert_forbidden(&err);
    }

    #[tokio::test]
    async fn transition_goal_denies_denied_context() {
        let owner = owner();
        let req = GoalTransitionRequest {
            principal: owner,
            prior_goal_id: goal_id(),
            next_state: GoalState::Paused,
            authorship: GoalAuthorship::User,
            request_id: request_id("transition"),
            author_self_perspective_id: None,
        };
        let err = engine()
            .transition_goal(&AuthzContext::denied_for_owner(&owner), &req)
            .await
            .expect_err("denied context must fail before storage");
        assert_forbidden(&err);
    }

    #[tokio::test]
    async fn modify_goal_denies_denied_context() {
        let owner = owner();
        let req = GoalModifyRequest {
            principal: owner,
            prior_goal_id: goal_id(),
            replacement: payload_write(),
            authorship: GoalAuthorship::User,
            request_id: request_id("modify"),
            evidence: None,
            author_self_perspective_id: None,
        };
        let err = engine()
            .modify_goal(&AuthzContext::denied_for_owner(&owner), &req)
            .await
            .expect_err("denied context must fail before schema or storage");
        assert_forbidden(&err);
    }

    #[tokio::test]
    async fn mark_goal_achieved_denies_denied_context() {
        let owner = owner();
        let req = GoalMarkAchievedRequest {
            principal: owner,
            prior_goal_id: goal_id(),
            authorship: tool_authorship(),
            request_id: request_id("achieved"),
            evidence: vec![GoalEvidenceRef {
                memory_id: memory_id(),
            }],
            author_self_perspective_id: None,
        };
        let err = engine()
            .mark_goal_achieved(&AuthzContext::denied_for_owner(&owner), &req)
            .await
            .expect_err("denied context must fail before storage");
        assert_forbidden(&err);
    }

    #[tokio::test]
    async fn decompose_goal_denies_denied_context() {
        let owner = owner();
        let req = GoalDecomposeRequest {
            principal: owner,
            parent_goal_id: goal_id(),
            authorship: tool_authorship(),
            target_self: GoalTargetSelf::SelfPerspective(memory_id()),
            children: vec![ChildGoalDraft {
                payload: payload_write(),
                evidence: Vec::new(),
                request_id: request_id("decompose-child"),
            }],
            author_self_perspective_id: None,
        };
        let err = engine()
            .decompose_goal(&AuthzContext::denied_for_owner(&owner), &req)
            .await
            .expect_err("denied context must fail before target lookup or storage");
        assert_forbidden(&err);
    }
}

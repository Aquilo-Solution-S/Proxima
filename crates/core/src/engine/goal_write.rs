use super::{Engine, pipeline::WritePermit};
use crate::access::{EntityId, Relation};
use crate::authz::AuthzContext;
use crate::error::ProtocolError;
use crate::storage::StorageError;
use crate::storage_ports::GoalCommandStoragePorts;
use crate::storage_ports::OwnerWritePermit;
use crate::verbs::goal_write::{
    AchieveGoalAtomicRequest, ChildGoalDraft, CreateGoalAtomicRequest, DecomposeGoalAtomicRequest,
    DecomposeGoalOutcome, GoalAtomicContext, GoalAuthorship, GoalCreateRequest, GoalDraft,
    GoalEvidenceRef, GoalPayloadWrite, GoalState, GoalTopologyWrite, GoalWakeConfigWrite,
    GoalWakeTrigger, GoalWriteBuildError, GoalWriteOutcome, IdempotencyKey,
    ModifyGoalAtomicRequest, TransitionGoalAtomicRequest,
};
use crate::verbs::schema::PayloadKind;
use crate::{EntityKind, GoalPayload, MemoryId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalCreatePayloadWriteRequest {
    pub owner: crate::OwnerRef,
    pub topology: GoalTopologyWrite,
    pub wake: Option<GoalWakeConfigWrite>,
    pub payload: GoalPayloadWrite,
    pub request_id: IdempotencyKey,
    pub authorship: GoalAuthorship,
    pub author_self_perspective_id: Option<MemoryId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalTransitionRequest {
    pub owner: crate::OwnerRef,
    pub prior_goal_id: crate::GoalId,
    pub next_state: GoalState,
    pub authorship: GoalAuthorship,
    pub request_id: IdempotencyKey,
    pub author_self_perspective_id: Option<MemoryId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalMarkAchievedRequest {
    pub owner: crate::OwnerRef,
    pub prior_goal_id: crate::GoalId,
    pub authorship: GoalAuthorship,
    pub request_id: IdempotencyKey,
    pub evidence: Vec<crate::verbs::goal_write::GoalEvidenceRef>,
    pub author_self_perspective_id: Option<MemoryId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalModifyRequest {
    pub owner: crate::OwnerRef,
    pub prior_goal_id: crate::GoalId,
    pub replacement: GoalPayloadWrite,
    pub wake: Option<Option<GoalWakeConfigWrite>>,
    pub authorship: GoalAuthorship,
    pub request_id: IdempotencyKey,
    pub evidence: Option<Vec<crate::verbs::goal_write::GoalEvidenceRef>>,
    pub author_self_perspective_id: Option<MemoryId>,
}

#[derive(Debug)]
pub struct GoalDecomposeRequest {
    pub owner: crate::OwnerRef,
    pub parent_goal_id: crate::GoalId,
    pub authorship: GoalAuthorship,
    pub topology: GoalTopologyWrite,
    pub children: Vec<ChildGoalDraft>,
    pub author_self_perspective_id: Option<MemoryId>,
}

impl Engine {
    /// Create an Active typed Goal for an embedded host or protocol
    /// caller without exposing `proxima_core.goal` storage shape.
    ///
    /// The request must name the target Perspective explicitly;
    /// current Proxima Goal assignment is a Goal-declared reference to its
    /// Perspective,
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
            .authorize_write(authz, &request.owner, Relation::Editor)
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
            .authorize_write(authz, &req.owner, Relation::Editor)
            .await?;
        let payload = self.normalize_payload_write(req.payload.clone())?;
        self.validate_goal_topology_authorized(authz, permit.owner(), &req.topology)
            .await?;
        let author_self_perspective_id = self
            .author_self_perspective_authorized(authz, req.author_self_perspective_id)
            .await?;
        self.validate_wake_config_for_write(authz, req.wake.as_ref())
            .await?;
        let draft = GoalDraft::active_from_payload_write(
            *permit.owner(),
            payload,
            req.topology.clone(),
            req.wake.clone(),
            req.authorship.clone(),
            req.request_id.clone(),
        );
        let embedding_client = self.embed_client();
        let context =
            self.goal_atomic_context(embedding_client.as_ref(), author_self_perspective_id);
        self.storage()
            .goal_command
            .goal_write
            .create_goal_atomic(
                &CreateGoalAtomicRequest {
                    draft,
                    context,
                    write_act_t: None,
                },
                permit.owner_write_permit(),
            )
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
            .authorize_write(authz, &req.owner, Relation::Editor)
            .await?;
        // `Achieved` is never a legal plain-transition target (achievement
        // carries mandatory evidence via `mark_goal_achieved`); the
        // authoritative matrix is `GoalState::{may_transition_to, may_achieve}`,
        // enforced storage-side. Pre-screen only this target here for an
        // actionable error before opening a storage transaction.
        if req.next_state == GoalState::Achieved {
            return Err(ProtocolError::invalid_argument(
                "next_state",
                "Achieved is not a valid transition target; use mark_goal_achieved with evidence",
            ));
        }
        let author_self_perspective_id = self
            .author_self_perspective_authorized(authz, req.author_self_perspective_id)
            .await?;
        let embedding_client = self.embed_client();
        let context =
            self.goal_atomic_context(embedding_client.as_ref(), author_self_perspective_id);
        transition_goal_authorized(
            &self.storage().goal_command,
            &TransitionGoalAtomicRequest {
                owner: *permit.owner(),
                prior_goal_id: req.prior_goal_id,
                next_state: req.next_state,
                authorship: req.authorship.clone(),
                request_id: req.request_id.clone(),
                context,
            },
            permit.owner_write_permit(),
        )
        .await
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
            .authorize_write(authz, &req.owner, Relation::Editor)
            .await?;
        let author_self_perspective_id = self
            .author_self_perspective_authorized(authz, req.author_self_perspective_id)
            .await?;
        self.validate_goal_evidence_authorized(authz, &req.evidence)
            .await?;
        let embedding_client = self.embed_client();
        let context =
            self.goal_atomic_context(embedding_client.as_ref(), author_self_perspective_id);
        self.storage()
            .goal_command
            .goal_write
            .achieve_goal_atomic(
                &AchieveGoalAtomicRequest {
                    owner: *permit.owner(),
                    prior_goal_id: req.prior_goal_id,
                    authorship: req.authorship.clone(),
                    request_id: req.request_id.clone(),
                    context,
                    evidence: req.evidence.clone(),
                },
                permit.owner_write_permit(),
            )
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
            .authorize_write(authz, &req.owner, Relation::Editor)
            .await?;
        let author_self_perspective_id = self
            .author_self_perspective_authorized(authz, req.author_self_perspective_id)
            .await?;
        if let Some(Some(config)) = &req.wake {
            self.validate_wake_config_for_write(authz, Some(config))
                .await?;
        }
        let replacement = self.normalize_payload_write(req.replacement.clone())?;
        let evidence = if let Some(evidence) = req.evidence.clone() {
            self.validate_goal_evidence_authorized(authz, &evidence)
                .await?;
            Some(evidence)
        } else {
            let carried = self
                .storage()
                .read_verb
                .goal_read
                .load_goal_evidence(permit.owner(), req.prior_goal_id)
                .await
                .map_err(|err| super::errors::internal_storage_error("load_goal_evidence", &err))?;
            let Some(ids) = carried else {
                // Keep None for an absent or foreign Goal. Storage then
                // performs its normal owner-scoped prior-head lookup and
                // preserves the existing not-found collapse.
                return self
                    .modify_goal_with_evidence(
                        &permit,
                        req,
                        replacement,
                        None,
                        author_self_perspective_id,
                    )
                    .await;
            };
            let evidence: Vec<_> = ids.into_iter().map(GoalEvidenceRef::new).collect();
            self.validate_goal_evidence_authorized(authz, &evidence)
                .await?;
            Some(evidence)
        };
        self.modify_goal_with_evidence(
            &permit,
            req,
            replacement,
            evidence,
            author_self_perspective_id,
        )
        .await
    }

    async fn modify_goal_with_evidence(
        &self,
        permit: &WritePermit,
        req: &GoalModifyRequest,
        replacement: crate::verbs::goal_write::GoalPayloadWrite,
        evidence: Option<Vec<GoalEvidenceRef>>,
        author_self_perspective_id: Option<MemoryId>,
    ) -> Result<GoalWriteOutcome, ProtocolError> {
        let embedding_client = self.embed_client();
        let context =
            self.goal_atomic_context(embedding_client.as_ref(), author_self_perspective_id);
        self.storage()
            .goal_command
            .goal_write
            .modify_goal_atomic(
                &ModifyGoalAtomicRequest {
                    owner: *permit.owner(),
                    prior_goal_id: req.prior_goal_id,
                    replacement,
                    wake: req.wake.clone(),
                    authorship: req.authorship.clone(),
                    request_id: req.request_id.clone(),
                    context,
                    evidence,
                },
                permit.owner_write_permit(),
            )
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
            .authorize_write(authz, &req.owner, Relation::Editor)
            .await?;
        self.validate_goal_topology_authorized(authz, permit.owner(), &req.topology)
            .await?;
        let mut children = Vec::with_capacity(req.children.len());
        for child in &req.children {
            children.push(self.child_goal_draft_for_write(authz, child).await?);
        }
        let author_self_perspective_id = self
            .author_self_perspective_authorized(authz, req.author_self_perspective_id)
            .await?;
        let embedding_client = self.embed_client();
        let context =
            self.goal_atomic_context(embedding_client.as_ref(), author_self_perspective_id);
        self.storage()
            .goal_command
            .goal_write
            .decompose_goal_atomic(
                &DecomposeGoalAtomicRequest {
                    owner: *permit.owner(),
                    parent_goal_id: req.parent_goal_id,
                    authorship: req.authorship.clone(),
                    context,
                    topology: req.topology.clone(),
                    children,
                },
                permit.owner_write_permit(),
            )
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
            owner: _,
            topology,
            wake,
            title,
            text,
            payload,
            request_id,
            authorship,
            author_self_perspective_id,
        } = request;

        // Registry-local schema validation runs before the storage-backed
        // topology/wake checks, matching `create_goal_from_payload_write`:
        // both create entry points answer a bad-schema + bad-topology
        // request with `UnknownSchema` first.
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

        self.validate_goal_topology_authorized(authz, permit.owner(), &topology)
            .await?;
        let author_self_perspective_id = self
            .author_self_perspective_authorized(authz, author_self_perspective_id)
            .await?;
        self.validate_wake_config_for_write(authz, wake.as_ref())
            .await?;

        let embedding_client = self.embed_client();
        let embedding_model_id = embedding_client.as_ref().map(|client| client.model_id());
        let draft = GoalDraft::active_from_payload_write(
            *permit.owner(),
            payload_write,
            topology,
            wake,
            authorship,
            request_id,
        );
        let outcome = self
            .storage()
            .goal_command
            .goal_write
            .create_goal_atomic(
                &CreateGoalAtomicRequest {
                    draft,
                    context: GoalAtomicContext {
                        registry: self.registry(),
                        embedding_model_id,
                        author_self_perspective_id,
                    },
                    write_act_t: None,
                },
                permit.owner_write_permit(),
            )
            .await
            .map_err(map_goal_storage_error)?;
        Ok(outcome)
    }

    pub(in crate::engine) fn goal_atomic_context<'a>(
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

    pub(in crate::engine) fn normalize_payload_write(
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

    async fn validate_goal_topology_authorized(
        &self,
        authz: &AuthzContext,
        goal_owner: &crate::Owner,
        topology: &GoalTopologyWrite,
    ) -> Result<(), ProtocolError> {
        self.validate_goal_topology_authorized_visible(authz, goal_owner, topology, &[])
            .await
    }

    pub(in crate::engine) async fn validate_goal_topology_authorized_visible(
        &self,
        authz: &AuthzContext,
        goal_owner: &crate::Owner,
        topology: &GoalTopologyWrite,
        written: &[MemoryId],
    ) -> Result<(), ProtocolError> {
        let assignment = topology.assignment().perspective_id();
        if !written.contains(&assignment) {
            self.target_perspective_authorized(authz, goal_owner, assignment)
                .await?;
        }
        for item in topology.evidence() {
            if written.contains(&item.memory_id()) {
                continue;
            }
            self.authorize_entry_read(authz, crate::access::EntityId::Memory(item.memory_id()))
                .await?;
        }
        Ok(())
    }

    async fn child_goal_draft_for_write(
        &self,
        authz: &AuthzContext,
        child: &ChildGoalDraft,
    ) -> Result<ChildGoalDraft, ProtocolError> {
        self.validate_wake_config_for_write(authz, child.wake.as_ref())
            .await?;
        self.validate_goal_evidence_authorized(authz, &child.evidence)
            .await?;
        Ok(ChildGoalDraft {
            payload: self.normalize_payload_write(child.payload.clone())?,
            evidence: child.evidence.clone(),
            wake: child.wake.clone(),
            request_id: child.request_id.clone(),
        })
    }

    async fn target_perspective_authorized(
        &self,
        authz: &AuthzContext,
        goal_owner: &crate::Owner,
        memory_id: MemoryId,
    ) -> Result<MemoryId, ProtocolError> {
        let home_owner = self
            .storage()
            .goal_command
            .owner_access_read
            .home_owner(EntityId::Memory(memory_id))
            .await
            .map_err(|err| ProtocolError::internal(format!("home_owner: {err}")))?
            .ok_or_else(|| ProtocolError::forbidden("entry not found"))?;
        if &home_owner != goal_owner {
            return Err(ProtocolError::forbidden("entry not found"));
        }
        self.authorize_write(authz, &home_owner, Relation::Editor)
            .await?;
        self.require_perspective_kind(&home_owner, memory_id, "target_perspective")
            .await?;
        Ok(memory_id)
    }

    pub(in crate::engine) async fn author_self_perspective_authorized(
        &self,
        authz: &AuthzContext,
        memory_id: Option<MemoryId>,
    ) -> Result<Option<MemoryId>, ProtocolError> {
        let Some(memory_id) = memory_id else {
            return Ok(None);
        };
        let home_owner = self
            .storage()
            .goal_command
            .owner_access_read
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

    pub(in crate::engine) async fn validate_wake_config_for_write(
        &self,
        authz: &AuthzContext,
        wake: Option<&GoalWakeConfigWrite>,
    ) -> Result<(), ProtocolError> {
        let Some(wake) = wake else {
            return Ok(());
        };
        match wake.trigger() {
            GoalWakeTrigger::FactSchema {
                schema_id,
                schema_version,
            } => {
                self.registry()
                    .lookup_payload(schema_id, *schema_version, PayloadKind::Fact)
                    .ok_or_else(|| {
                        ProtocolError::unknown_schema(
                            schema_id.as_str(),
                            schema_version.into_inner(),
                        )
                    })?;
            }
            GoalWakeTrigger::FactMemory { memory_id } => {
                self.require_readable_memory_kind(
                    authz,
                    *memory_id,
                    "wake_trigger",
                    EntityKind::Fact,
                )
                .await?;
            }
        }
        for memory_id in wake.hard_memory_ids() {
            let permit = self
                .authorize_entry_read(authz, EntityId::Memory(*memory_id))
                .await?;
            let _kind = self
                .load_required_memory_kind(permit.owner(), *memory_id)
                .await?;
        }
        Ok(())
    }

    /// Read-admit each evidence target. Kind ∈ {F, A} is the in-tx
    /// TOCTOU (`validate_evidence_in_owner`); a second pre-tx kind
    /// load here is the overlapping walk.
    async fn validate_goal_evidence_authorized(
        &self,
        authz: &AuthzContext,
        evidence: &[GoalEvidenceRef],
    ) -> Result<(), ProtocolError> {
        for item in evidence {
            self.authorize_entry_read(authz, EntityId::Memory(item.memory_id()))
                .await?;
        }
        Ok(())
    }

    async fn require_readable_memory_kind(
        &self,
        authz: &AuthzContext,
        memory_id: MemoryId,
        field: &'static str,
        expected: EntityKind,
    ) -> Result<(), ProtocolError> {
        let permit = self
            .authorize_entry_read(authz, EntityId::Memory(memory_id))
            .await?;
        let kind = self
            .load_required_memory_kind(permit.owner(), memory_id)
            .await?;
        if kind != expected {
            return Err(ProtocolError::invalid_argument(
                field,
                format!("target must be {expected:?}"),
            ));
        }
        Ok(())
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
                "target must be a Perspective",
            ));
        }
        Ok(())
    }
}

fn map_goal_build_error(err: GoalWriteBuildError) -> ProtocolError {
    match err {
        GoalWriteBuildError::InvalidTitle(_) => {
            ProtocolError::invalid_argument("title", err.to_string())
        }
        GoalWriteBuildError::InvalidText(_) => {
            ProtocolError::invalid_argument("text", err.to_string())
        }
    }
}

pub(in crate::engine) async fn transition_goal_authorized(
    ports: &GoalCommandStoragePorts,
    req: &TransitionGoalAtomicRequest<'_>,
    permit: &OwnerWritePermit,
) -> Result<GoalWriteOutcome, ProtocolError> {
    ports
        .goal_write
        .transition_goal_atomic(req, permit)
        .await
        .map_err(map_goal_storage_error)
}

fn map_goal_storage_error(err: StorageError) -> ProtocolError {
    super::errors::map_write_storage_error(err, "goal", "goal write referenced row not found")
}

#[cfg(test)]
mod tests {
    use super::*;

    use super::super::access_sets::tests::MembershipStorage;
    use crate::error::ErrorCode;
    use crate::verbs::goal_write::{
        GoalAssignmentTarget, GoalEvidenceRef, GoalTopologyWrite, SystemOrigin,
    };
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
        Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests())
    }

    #[test]
    fn evidence_admit_does_not_reload_kind() {
        let src = include_str!("goal_write.rs");
        let start = src
            .find("async fn validate_goal_evidence_authorized")
            .expect("validate_goal_evidence_authorized");
        let rest = &src[start..];
        let end = rest
            .find("async fn require_readable_memory_kind")
            .expect("next fn");
        let reload = format!("{}{}", "load_required_memory_", "kind");
        assert!(
            !rest[..end].contains(&reload),
            "evidence kind is the in-tx TOCTOU, not a second pre-tx walk"
        );
        assert!(
            rest[..end].contains("authorize_entry_read"),
            "read-admit stays; only the kind walk is dropped"
        );
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
            goal_evidence: None,
            observed_fact_writes: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            observed_modify_evidence: std::sync::Arc::new(std::sync::Mutex::new(None)),
            observed_goal_authorship: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    fn engine_with_ports(storage: MembershipStorage) -> Engine {
        Engine::compose_or_panic_for_tests(storage.storage_ports(), |registry| {
            registry.add_goal_schema_or_panic_for_tests::<TestGoalPayload>();
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

    fn topology(target: MemoryId) -> GoalTopologyWrite {
        GoalTopologyWrite::new(
            GoalAssignmentTarget::perspective(target),
            Vec::new(),
            Vec::new(),
        )
        .expect("test topology")
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
        let engine = engine_with_ports(storage_with_memory(
            owner,
            foreign,
            true,
            EntityKind::Perspective,
        ));

        let err = engine
            .author_self_perspective_authorized(
                &AuthzContext::single_owner(&owner, AuthPath::HostBearer),
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
        let engine = engine_with_ports(storage_with_memory(
            owner,
            owner,
            true,
            EntityKind::Perspective,
        ));

        let authorized = engine
            .author_self_perspective_authorized(
                &AuthzContext::single_owner(&owner, AuthPath::HostBearer),
                Some(memory_id),
            )
            .await
            .expect("writable Perspective should authorize");

        assert_eq!(authorized, Some(memory_id));
    }

    #[tokio::test]
    async fn create_goal_from_payload_write_rejects_foreign_perspective_target() {
        let owner = owner();
        let foreign = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
        let target = memory_id();
        let engine = engine_with_ports(storage_with_memory(
            owner,
            foreign,
            false,
            EntityKind::Perspective,
        ));
        let req = GoalCreatePayloadWriteRequest {
            owner,
            topology: topology(target),
            wake: None,
            payload: payload_write(),
            request_id: request_id("create-unreadable-target"),
            authorship: tool_authorship(),
            author_self_perspective_id: None,
        };

        let err = engine
            .create_goal_from_payload_write(
                &AuthzContext::single_owner(&owner, AuthPath::HostBearer),
                &req,
            )
            .await
            .expect_err("foreign target Perspective must be rejected before write");

        assert_eq!(err.code, ErrorCode::Forbidden);
    }

    #[tokio::test]
    async fn decompose_goal_rejects_foreign_perspective_target() {
        let owner = owner();
        let foreign = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
        let target = memory_id();
        let engine = engine_with_ports(storage_with_memory(
            owner,
            foreign,
            false,
            EntityKind::Perspective,
        ));
        let req = GoalDecomposeRequest {
            owner,
            parent_goal_id: goal_id(),
            authorship: tool_authorship(),
            topology: topology(target),
            children: vec![ChildGoalDraft {
                payload: payload_write(),
                evidence: Vec::new(),
                wake: None,
                request_id: request_id("decompose-unreadable-target"),
            }],
            author_self_perspective_id: None,
        };

        let err = engine
            .decompose_goal(
                &AuthzContext::single_owner(&owner, AuthPath::HostBearer),
                &req,
            )
            .await
            .expect_err("foreign target Perspective must be rejected before write");

        assert_eq!(err.code, ErrorCode::Forbidden);
    }

    #[tokio::test]
    async fn target_perspective_allows_readable_perspective() {
        let owner = owner();
        let target = memory_id();
        let engine = engine_with_ports(storage_with_memory(
            owner,
            owner,
            true,
            EntityKind::Perspective,
        ));
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        let permit = engine
            .authorize_write(&authz, &owner, Relation::Editor)
            .await
            .expect("owner write should authorize");

        let authorized = engine
            .target_perspective_authorized(&authz, permit.owner(), target)
            .await
            .expect("readable Perspective should authorize");

        assert_eq!(authorized, target);
    }

    #[tokio::test]
    async fn create_goal_from_payload_write_denies_denied_context() {
        let owner = owner();
        let req = GoalCreatePayloadWriteRequest {
            owner,
            topology: topology(memory_id()),
            wake: None,
            payload: payload_write(),
            request_id: request_id("create"),
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
    async fn transition_goal_rejects_achieved_target_pre_storage() {
        // Transitioning to Achieved is rejected at the engine boundary,
        // after authz but before any storage transaction. The membership
        // storage authorizes the self-owner write; the guard fires first, so
        // the goal write port is never reached.
        let owner = owner();
        let engine = engine_with_ports(storage_with_memory(
            owner,
            owner,
            true,
            EntityKind::Perspective,
        ));
        let req = GoalTransitionRequest {
            owner,
            prior_goal_id: goal_id(),
            next_state: GoalState::Achieved,
            authorship: GoalAuthorship::User,
            request_id: request_id("transition-achieved"),
            author_self_perspective_id: None,
        };
        let err = engine
            .transition_goal(
                &AuthzContext::single_owner(&owner, AuthPath::HostBearer),
                &req,
            )
            .await
            .expect_err("Achieved is not a valid plain-transition target");
        assert_eq!(err.code, ErrorCode::InvalidArgument);
    }

    #[tokio::test]
    async fn transition_goal_denies_denied_context() {
        let owner = owner();
        let req = GoalTransitionRequest {
            owner,
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
            owner,
            prior_goal_id: goal_id(),
            replacement: payload_write(),
            wake: None,
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
    async fn modify_goal_carries_exact_stored_evidence_when_omitted() {
        let owner = owner();
        let evidence = vec![memory_id(), memory_id()];
        let observed = std::sync::Arc::new(std::sync::Mutex::new(None));
        let mut storage = storage_with_memory(owner, owner, true, EntityKind::Abstraction);
        storage.goal_evidence = Some(evidence.clone());
        storage.observed_modify_evidence = observed.clone();
        let engine = engine_with_ports(storage);
        let req = GoalModifyRequest {
            owner,
            prior_goal_id: goal_id(),
            replacement: payload_write(),
            wake: None,
            authorship: GoalAuthorship::User,
            request_id: request_id("modify-carry-evidence"),
            evidence: None,
            author_self_perspective_id: None,
        };

        let err = engine
            .modify_goal(
                &AuthzContext::single_owner(&owner, AuthPath::HostBearer),
                &req,
            )
            .await
            .expect_err("test storage rejects writes after recording the request");
        assert_eq!(err.code, ErrorCode::Internal);
        assert_eq!(
            *observed.lock().expect("modify evidence recorder lock"),
            Some(evidence),
            "omitted evidence must be carried exactly, not rebuilt from visible joins"
        );
    }

    #[tokio::test]
    async fn mark_goal_achieved_denies_denied_context() {
        let owner = owner();
        let req = GoalMarkAchievedRequest {
            owner,
            prior_goal_id: goal_id(),
            authorship: tool_authorship(),
            request_id: request_id("achieved"),
            evidence: vec![GoalEvidenceRef::new(memory_id())],
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
            owner,
            parent_goal_id: goal_id(),
            authorship: tool_authorship(),
            topology: topology(memory_id()),
            children: vec![ChildGoalDraft {
                payload: payload_write(),
                evidence: Vec::new(),
                wake: None,
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

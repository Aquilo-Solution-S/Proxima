use super::{Engine, pipeline::WritePermit};
use crate::access::{AccessKind, EntityId};
use crate::authz::{AuthzContext, EngineAuthority};
use crate::error::ProtocolError;
use crate::publication::PublicationStamp;
use crate::storage::StorageError;
use crate::storage_ports::GoalCommandStoragePorts;
use crate::storage_ports::OwnerWritePermit;
use crate::verbs::goal_write::{
    AchieveGoalAtomicRequest, ChildGoalDraft, CreateGoalAtomicRequest, DecomposeGoalAtomicRequest,
    DecomposeGoalOutcome, GoalAssignmentTarget, GoalAtomicContext, GoalAuthorship,
    GoalCreateRequest, GoalDraft, GoalEvidenceRef, GoalPayloadWrite, GoalReplayOutcome,
    GoalReplayRequest, GoalTopologyWrite, GoalTransitionKind, GoalWriteBuildError,
    GoalWriteOutcome, IdempotencyKey, ModifyGoalAtomicRequest, TransitionGoalAtomicRequest,
};
use crate::verbs::schema::PayloadKind;
use crate::{EntityKind, FactPayload, GoalChangeV1, GoalPayload, MemoryId, SchemaVersion};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalCreatePayloadWriteRequest {
    pub owner: crate::OwnerId,
    pub topology: GoalTopologyWrite,
    pub payload: GoalPayloadWrite,
    pub request_id: IdempotencyKey,
    pub authorship: GoalAuthorship,
    pub author_self_perspective_id: Option<MemoryId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalTransitionRequest {
    pub owner: crate::OwnerId,
    pub prior_goal_id: crate::GoalId,
    pub transition: GoalTransitionKind,
    pub authorship: GoalAuthorship,
    pub request_id: IdempotencyKey,
    pub author_self_perspective_id: Option<MemoryId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalMarkAchievedRequest {
    pub owner: crate::OwnerId,
    pub prior_goal_id: crate::GoalId,
    pub authorship: GoalAuthorship,
    pub request_id: IdempotencyKey,
    pub evidence: Vec<crate::verbs::goal_write::GoalEvidenceRef>,
    pub author_self_perspective_id: Option<MemoryId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalModifyRequest {
    pub owner: crate::OwnerId,
    pub prior_goal_id: crate::GoalId,
    pub replacement: GoalPayloadWrite,
    pub authorship: GoalAuthorship,
    pub request_id: IdempotencyKey,
    pub evidence: Option<Vec<crate::verbs::goal_write::GoalEvidenceRef>>,
    pub author_self_perspective_id: Option<MemoryId>,
}

#[derive(Debug)]
pub struct GoalDecomposeRequest {
    pub owner: crate::OwnerId,
    pub parent_goal_id: crate::GoalId,
    pub authorship: GoalAuthorship,
    pub topology: GoalTopologyWrite,
    pub children: Vec<ChildGoalDraft>,
    pub author_self_perspective_id: Option<MemoryId>,
}

impl Engine {
    pub(in crate::engine) fn normalize_goal_request<P: GoalPayload>(
        &self,
        request: GoalCreateRequest<P>,
    ) -> Result<GoalCreatePayloadWriteRequest, ProtocolError> {
        let payload = GoalPayloadWrite::from_payload(request.title, request.text, request.payload)
            .map_err(map_goal_build_error)?;
        Ok(GoalCreatePayloadWriteRequest {
            owner: request.owner,
            topology: request.topology,
            payload: self.normalize_payload_write(payload)?,
            request_id: request.request_id,
            authorship: request.authorship,
            author_self_perspective_id: request.author_self_perspective_id,
        })
    }

    /// Create an Active typed Goal for an embedded host or protocol
    /// caller without exposing `proxima_core.goal` storage shape.
    ///
    /// The request may name a target Perspective. An unassigned Goal belongs
    /// to its Owner alone.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when `authz` cannot access the request Owner or
    /// lacks Goal write on the owner;
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
            .authorize_write(authz, &request.owner, AccessKind::Goal)
            .await?;
        let request = self.normalize_goal_request(request)?;
        self.create_goal_payload_authorized(authz, &permit, &request)
            .await
    }

    /// Create an Active Goal from a dynamic protocol payload.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when `authz` cannot access the request Owner or
    /// lacks Goal write on the owner;
    /// `UnknownSchema` when the payload schema is not registered as a Goal;
    /// `InvalidArgument` for malformed target/evidence/parent references; or
    /// `Internal` for storage failures.
    pub async fn create_goal_from_payload_write(
        &self,
        authz: &AuthzContext,
        req: &GoalCreatePayloadWriteRequest,
    ) -> Result<GoalWriteOutcome, ProtocolError> {
        let permit = self
            .authorize_write(authz, &req.owner, AccessKind::Goal)
            .await?;
        let req = GoalCreatePayloadWriteRequest {
            payload: self.normalize_payload_write(req.payload.clone())?,
            ..req.clone()
        };
        self.create_goal_payload_authorized(authz, &permit, &req)
            .await
    }

    async fn create_goal_payload_authorized(
        &self,
        authz: &AuthzContext,
        permit: &WritePermit,
        req: &GoalCreatePayloadWriteRequest,
    ) -> Result<GoalWriteOutcome, ProtocolError> {
        let draft = GoalDraft::active_from_payload_write(
            *permit.owner(),
            req.payload.clone(),
            req.topology.clone(),
            req.authorship.clone(),
            req.request_id.clone(),
        );
        let context = self.goal_atomic_context(authz, req.author_self_perspective_id)?;
        let atomic = CreateGoalAtomicRequest {
            draft,
            context,
            write_act_t: None,
        };
        if let Some(replay) = resolve_single_goal_replay(
            &self.storage().goal_command,
            GoalReplayRequest::Create(&atomic),
            permit.owner_write_permit(),
        )
        .await?
        {
            return Ok(replay);
        }
        self.validate_goal_topology_authorized(authz, permit.owner(), &req.topology)
            .await?;
        self.author_self_perspective_authorized(authz, req.author_self_perspective_id)
            .await?;
        self.storage()
            .goal_command
            .goal_write
            .create_goal_atomic(&atomic, permit.owner_write_permit())
            .await
            .map_err(map_goal_storage_error)
    }

    /// Transition a Goal head to Paused, Active, or Abandoned.
    ///
    /// Abandon takes an Active or Paused goal in one write; it is refused
    /// while any decompose child's current head is Active or Paused: an
    /// open goal never sits under a closed one. Pause and resume are not
    /// affected.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when `authz` cannot access the request Owner or
    /// lacks Goal write; `InvalidArgument` (field `goal`) when abandon meets an
    /// open child (`goal has open children: <handle>, ...`, naming each open
    /// child's current head); `InvalidArgument`
    /// or `NotFound` for rejected goal references; or `Internal` for storage
    /// failures.
    pub async fn transition_goal(
        &self,
        authz: &AuthzContext,
        req: &GoalTransitionRequest,
    ) -> Result<GoalWriteOutcome, ProtocolError> {
        let permit = self
            .authorize_write(authz, &req.owner, AccessKind::Goal)
            .await?;
        let context = self.goal_atomic_context(authz, req.author_self_perspective_id)?;
        // `Achieved` is not a `GoalTransitionKind`: achievement has
        // its dedicated path through `mark_goal_achieved`. The
        // authoritative legality matrix is `GoalState::may_transition_to`,
        // enforced storage-side.
        let atomic = TransitionGoalAtomicRequest {
            owner: *permit.owner(),
            prior_goal_id: req.prior_goal_id,
            transition: req.transition,
            authorship: req.authorship.clone(),
            request_id: req.request_id.clone(),
            context,
        };
        if let Some(replay) = resolve_single_goal_replay(
            &self.storage().goal_command,
            GoalReplayRequest::Transition(&atomic),
            permit.owner_write_permit(),
        )
        .await?
        {
            return Ok(replay);
        }
        self.author_self_perspective_authorized(authz, req.author_self_perspective_id)
            .await?;
        transition_goal_authorized(
            &self.storage().goal_command,
            &atomic,
            permit.owner_write_permit(),
        )
        .await
    }

    /// Mark a Goal head Achieved with optional evidence.
    ///
    /// A goal with decompose children is Achieved only when no child's
    /// current head is Active or Paused and at least one is Achieved (#514).
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when `authz` cannot access the request Owner or
    /// lacks Goal write; `InvalidArgument` (field `goal`) when a child is
    /// still open (`goal has open children: <handle>, ...`, naming each open
    /// child's current head) or every child is Abandoned; `InvalidArgument`
    /// or `NotFound` for rejected references; or `Internal` for storage
    /// failures.
    pub async fn mark_goal_achieved(
        &self,
        authz: &AuthzContext,
        req: &GoalMarkAchievedRequest,
    ) -> Result<GoalWriteOutcome, ProtocolError> {
        let permit = self
            .authorize_write(authz, &req.owner, AccessKind::Goal)
            .await?;
        let context = self.goal_atomic_context(authz, req.author_self_perspective_id)?;
        let atomic = AchieveGoalAtomicRequest {
            owner: *permit.owner(),
            prior_goal_id: req.prior_goal_id,
            authorship: req.authorship.clone(),
            request_id: req.request_id.clone(),
            context,
            evidence: req.evidence.clone(),
        };
        if let Some(replay) = resolve_single_goal_replay(
            &self.storage().goal_command,
            GoalReplayRequest::Achieve(&atomic),
            permit.owner_write_permit(),
        )
        .await?
        {
            return Ok(replay);
        }
        self.author_self_perspective_authorized(authz, req.author_self_perspective_id)
            .await?;
        self.validate_goal_evidence_authorized(authz, &req.evidence)
            .await?;
        self.storage()
            .goal_command
            .goal_write
            .achieve_goal_atomic(&atomic, permit.owner_write_permit())
            .await
            .map_err(map_goal_storage_error)
    }

    /// Replace an Active Goal head's content.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when `authz` cannot access the request Owner or
    /// lacks Goal write; `UnknownSchema`
    /// when the replacement payload schema is not registered as a Goal;
    /// `InvalidArgument` or `NotFound` for rejected references; or `Internal`
    /// for storage failures.
    pub async fn modify_goal(
        &self,
        authz: &AuthzContext,
        req: &GoalModifyRequest,
    ) -> Result<GoalWriteOutcome, ProtocolError> {
        let permit = self
            .authorize_write(authz, &req.owner, AccessKind::Goal)
            .await?;
        let replacement = self.normalize_payload_write(req.replacement.clone())?;
        let context = self.goal_atomic_context(authz, req.author_self_perspective_id)?;
        let atomic = ModifyGoalAtomicRequest {
            owner: *permit.owner(),
            prior_goal_id: req.prior_goal_id,
            replacement,
            authorship: req.authorship.clone(),
            request_id: req.request_id.clone(),
            context,
            evidence: req.evidence.clone(),
        };
        if let Some(replay) = resolve_single_goal_replay(
            &self.storage().goal_command,
            GoalReplayRequest::Modify(&atomic),
            permit.owner_write_permit(),
        )
        .await?
        {
            return Ok(replay);
        }
        self.author_self_perspective_authorized(authz, req.author_self_perspective_id)
            .await?;
        if let Some(evidence) = &req.evidence {
            self.validate_goal_evidence_authorized(authz, evidence)
                .await?;
        } else {
            let carried = self
                .storage()
                .read_verb
                .goal_read
                .load_goal_evidence(permit.owner_scope(), permit.owner(), req.prior_goal_id)
                .await
                .map_err(|err| super::errors::internal_storage_error("load_goal_evidence", &err))?;
            if let Some(ids) = carried {
                // A carried id of another store was read when it was named;
                // this store neither holds nor can re-authorize it.
                let evidence: Vec<_> = ids
                    .into_iter()
                    .filter(|id| crate::memory_store_number(id.into_inner()) == self.store_number())
                    .map(GoalEvidenceRef::new)
                    .collect();
                self.validate_goal_evidence_authorized(authz, &evidence)
                    .await?;
            }
        }
        self.storage()
            .goal_command
            .goal_write
            .modify_goal_atomic(&atomic, permit.owner_write_permit())
            .await
            .map_err(map_goal_storage_error)
    }

    /// Create Active child Goals under a parent Goal.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when `authz` cannot access the request Owner or
    /// lacks Goal write; `UnknownSchema`
    /// when any child payload schema is not registered as a Goal;
    /// `InvalidArgument` or `NotFound` for rejected references; or `Internal`
    /// for storage failures.
    pub async fn decompose_goal(
        &self,
        authz: &AuthzContext,
        req: &GoalDecomposeRequest,
    ) -> Result<DecomposeGoalOutcome, ProtocolError> {
        let permit = self
            .authorize_write(authz, &req.owner, AccessKind::Goal)
            .await?;
        let mut children = Vec::with_capacity(req.children.len());
        for child in &req.children {
            children.push(self.normalize_child_goal_draft(child)?);
        }
        let context = self.goal_atomic_context(authz, req.author_self_perspective_id)?;
        let atomic = DecomposeGoalAtomicRequest {
            owner: *permit.owner(),
            parent_goal_id: req.parent_goal_id,
            authorship: req.authorship.clone(),
            context,
            topology: req.topology.clone(),
            children,
        };
        if let Some(replay) = resolve_decompose_goal_replay(
            &self.storage().goal_command,
            GoalReplayRequest::Decompose(&atomic),
            permit.owner_write_permit(),
        )
        .await?
        {
            return Ok(replay);
        }
        self.validate_goal_topology_authorized(authz, permit.owner(), &req.topology)
            .await?;
        for child in &atomic.children {
            self.validate_child_goal_draft_authorized(authz, child)
                .await?;
        }
        self.author_self_perspective_authorized(authz, req.author_self_perspective_id)
            .await?;
        self.storage()
            .goal_command
            .goal_write
            .decompose_goal_atomic(&atomic, permit.owner_write_permit())
            .await
            .map_err(map_goal_storage_error)
    }

    /// The context every goal write runs under: the frozen registry, the
    /// author's self-perspective and, while `core/goal-change-v1` is
    /// listenable, the stamp storage captures its change Fact with.
    ///
    /// The stamp binds what [`Self::resolve_publication`] binds for any other
    /// listenable Fact: the configured source, the token-certified model id
    /// (never the caller's label) and the host's extension attributes, under
    /// this engine's limits. Storage composes the payload itself, because the
    /// version's `t` and the series handle are minted inside its transaction.
    ///
    /// # Errors
    ///
    /// `Internal` when the schema is listenable and no publication source is
    /// configured: boot validation refuses that composition, so only an
    /// unvalidated one reaches here.
    pub(in crate::engine) fn goal_atomic_context<'a, A>(
        &'a self,
        authority: &'a A,
        author_self_perspective_id: Option<MemoryId>,
    ) -> Result<GoalAtomicContext<'a>, ProtocolError>
    where
        A: EngineAuthority + ?Sized,
    {
        let listenable = self
            .registry()
            .lookup_payload(
                &GoalChangeV1::schema_id(),
                SchemaVersion::new(GoalChangeV1::SCHEMA_VERSION),
                PayloadKind::Fact,
            )
            .is_some_and(|schema| schema.listenable);
        let publication = if listenable {
            let Some(source) = self.publication.source.as_ref() else {
                return Err(super::errors::map_publication_error(
                    &crate::publication::PublicationError::SourceUnbound {
                        schema_id: GoalChangeV1::SCHEMA_ID.to_owned(),
                    },
                ));
            };
            let authz = self.operation_authority(authority)?.authz();
            Some(PublicationStamp::new(
                source,
                authz.trusted_model_id(),
                authz.publication_extensions(),
                self.publication.limits,
            ))
        } else {
            None
        };
        Ok(GoalAtomicContext {
            registry: self.registry(),
            author_self_perspective_id,
            publication,
        })
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
        goal_owner: &crate::OwnerId,
        topology: &GoalTopologyWrite,
    ) -> Result<(), ProtocolError> {
        self.validate_goal_topology_authorized_visible(authz, goal_owner, topology, &[])
            .await
    }

    pub(in crate::engine) async fn validate_goal_topology_authorized_visible(
        &self,
        authz: &AuthzContext,
        goal_owner: &crate::OwnerId,
        topology: &GoalTopologyWrite,
        written: &[MemoryId],
    ) -> Result<(), ProtocolError> {
        if let Some(assignment) = topology
            .assignment()
            .map(GoalAssignmentTarget::perspective_id)
            && !written.contains(&assignment)
        {
            self.target_perspective_authorized(authz, goal_owner, assignment)
                .await?;
        }
        for item in topology.evidence() {
            if item.remote_origin().is_some() || written.contains(&item.memory_id()) {
                continue;
            }
            self.authorize_entry_read(authz, crate::access::EntityId::Memory(item.memory_id()))
                .await?;
        }
        Ok(())
    }

    fn normalize_child_goal_draft(
        &self,
        child: &ChildGoalDraft,
    ) -> Result<ChildGoalDraft, ProtocolError> {
        Ok(ChildGoalDraft {
            payload: self.normalize_payload_write(child.payload.clone())?,
            evidence: child.evidence.clone(),
            request_id: child.request_id.clone(),
        })
    }

    async fn validate_child_goal_draft_authorized(
        &self,
        authz: &AuthzContext,
        child: &ChildGoalDraft,
    ) -> Result<(), ProtocolError> {
        self.validate_goal_evidence_authorized(authz, &child.evidence)
            .await?;
        Ok(())
    }

    async fn target_perspective_authorized(
        &self,
        authz: &AuthzContext,
        goal_owner: &crate::OwnerId,
        memory_id: MemoryId,
    ) -> Result<MemoryId, ProtocolError> {
        let home_owner = self
            .storage()
            .goal_command
            .owner_access_read
            .home_owner(authz.owner_scope(), EntityId::Memory(memory_id))
            .await
            .map_err(|err| ProtocolError::internal(format!("home_owner: {err}")))?
            .ok_or_else(|| ProtocolError::forbidden("entry not found"))?;
        if &home_owner != goal_owner {
            return Err(ProtocolError::forbidden("entry not found"));
        }
        self.authorize_owner_read(authz, &home_owner, AccessKind::Perspective)
            .await?;
        self.require_perspective_kind(authz, &home_owner, memory_id, "target_perspective")
            .await?;
        Ok(memory_id)
    }

    pub(in crate::engine) async fn author_self_perspective_authorized(
        &self,
        authz: &AuthzContext,
        memory_id: Option<MemoryId>,
    ) -> Result<(), ProtocolError> {
        // Callers build `GoalAtomicContext` from the raw request value before
        // the replay probe, so this admits rather than normalizes. Returning
        // `()` keeps a future normalization from being silently discarded.
        let Some(memory_id) = memory_id else {
            return Ok(());
        };
        let home_owner = self
            .storage()
            .goal_command
            .owner_access_read
            .home_owner(authz.owner_scope(), EntityId::Memory(memory_id))
            .await
            .map_err(|err| ProtocolError::internal(format!("home_owner: {err}")))?
            .ok_or_else(|| ProtocolError::forbidden("entry not found"))?;
        self.authorize_owner_read(authz, &home_owner, AccessKind::Perspective)
            .await?;
        self.require_perspective_kind(authz, &home_owner, memory_id, "author_self_perspective_id")
            .await?;
        Ok(())
    }

    /// Read-admit each evidence target held by this store; one held by
    /// another store carries the other-stores read that proved the caller
    /// may see it. Kind ∈ {F, A} is the in-tx TOCTOU
    /// (`validate_evidence_in_owner`); a second pre-tx kind load here is the
    /// overlapping walk.
    async fn validate_goal_evidence_authorized(
        &self,
        authz: &AuthzContext,
        evidence: &[GoalEvidenceRef],
    ) -> Result<(), ProtocolError> {
        for item in evidence
            .iter()
            .filter(|item| item.remote_origin().is_none())
        {
            self.authorize_entry_read(authz, EntityId::Memory(item.memory_id()))
                .await?;
        }
        Ok(())
    }

    async fn require_perspective_kind(
        &self,
        authz: &AuthzContext,
        owner: &crate::OwnerId,
        memory_id: MemoryId,
        field: &'static str,
    ) -> Result<(), ProtocolError> {
        let kind = self
            .load_required_memory_kind(authz.owner_scope(), owner, memory_id)
            .await?;
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

async fn resolve_single_goal_replay(
    ports: &GoalCommandStoragePorts,
    req: GoalReplayRequest<'_, '_>,
    permit: &OwnerWritePermit,
) -> Result<Option<GoalWriteOutcome>, ProtocolError> {
    resolve_goal_replay_shaped(ports, req, permit, GoalReplayOutcome::into_goal).await
}

async fn resolve_decompose_goal_replay(
    ports: &GoalCommandStoragePorts,
    req: GoalReplayRequest<'_, '_>,
    permit: &OwnerWritePermit,
) -> Result<Option<DecomposeGoalOutcome>, ProtocolError> {
    resolve_goal_replay_shaped(ports, req, permit, GoalReplayOutcome::into_decompose).await
}

async fn resolve_goal_replay_shaped<T>(
    ports: &GoalCommandStoragePorts,
    req: GoalReplayRequest<'_, '_>,
    permit: &OwnerWritePermit,
    shape: fn(GoalReplayOutcome) -> Result<T, &'static str>,
) -> Result<Option<T>, ProtocolError> {
    let Some(outcome) = ports
        .goal_write
        .resolve_goal_replay(req, permit)
        .await
        .map_err(map_goal_storage_error)?
    else {
        return Ok(None);
    };
    shape(outcome).map(Some).map_err(ProtocolError::internal)
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

/// Goal storage errors for the standalone and unit-of-work paths alike.
pub(in crate::engine) fn map_goal_storage_error(err: StorageError) -> ProtocolError {
    // Evidence passed the entry gate, which resolves a home in either state
    // (or, in a unit of work, named a slot that replayed a forgotten memory);
    // a row the owner transaction cannot see is forgotten (or went away), and
    // the caller gets the gate's answer for an unknown id.
    if let StorageError::ConstraintViolation(message) = &err
        && message == crate::verbs::goal_write::EVIDENCE_DOES_NOT_EXIST
    {
        return ProtocolError::forbidden(super::pipeline::ENTRY_NOT_FOUND_MESSAGE);
    }
    super::errors::map_write_storage_error(err, "goal", "goal write referenced row not found")
}

#[cfg(test)]
mod tests {
    use super::*;

    use super::super::MembershipStorage;
    use crate::access::Relation;
    use crate::error::ErrorCode;
    use crate::verbs::goal_write::{
        GoalAssignmentTarget, GoalEvidenceRef, GoalTopologyWrite, SystemOrigin,
    };
    use crate::{
        AuthPath, Engine, FlavorRegistry, GoalId, GoalPayload, GroupId, OwnerId, SchemaId,
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

    /// #514 F10: the goal-tree refusals reach the protocol as
    /// `InvalidArgument` on `goal` with stable message prefixes.
    #[test]
    fn goal_tree_refusals_map_to_invalid_argument() {
        let a = GoalId::new(uuid::Uuid::from_u128(0x0001));
        let b = GoalId::new(uuid::Uuid::from_u128(0x0002));
        let err = map_goal_storage_error(StorageError::GoalChildrenOpen { open: vec![a, b] });
        assert_eq!(err.code, ErrorCode::InvalidArgument);
        assert_eq!(
            err.message,
            "invalid argument goal: goal has open children: \
             00000000-0000-0000-0000-000000000001, 00000000-0000-0000-0000-000000000002"
        );
        let err = map_goal_storage_error(StorageError::GoalChildrenAllAbandoned);
        assert_eq!(err.code, ErrorCode::InvalidArgument);
        assert_eq!(
            err.message,
            "invalid argument goal: goal children are all abandoned; achieve at least one child first"
        );
    }

    #[test]
    fn evidence_admit_does_not_reload_kind() {
        let src = include_str!("goal_write.rs");
        let start = src
            .find("async fn validate_goal_evidence_authorized")
            .expect("validate_goal_evidence_authorized");
        let rest = &src[start..];
        let end = rest
            .find("async fn require_perspective_kind")
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

    fn owner() -> crate::OwnerId {
        OwnerId::from(crate::UserId::new(uuid::Uuid::now_v7()))
    }

    fn storage_with_memory(
        member: OwnerId,
        home_owner: OwnerId,
        entity_readable: bool,
        memory_kind: EntityKind,
    ) -> MembershipStorage {
        MembershipStorage {
            observed_entity_reads: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            observed_kind_loads: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
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
        MemoryId::new(crate::new_memory_uuid(0))
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
            Some(GoalAssignmentTarget::perspective(target)),
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
        let foreign = OwnerId::from(UserId::new(uuid::Uuid::now_v7()));
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

        engine
            .author_self_perspective_authorized(
                &AuthzContext::single_owner(&owner, AuthPath::HostBearer),
                Some(memory_id),
            )
            .await
            .expect("writable Perspective should authorize");
    }

    #[tokio::test]
    async fn create_goal_from_payload_write_rejects_foreign_perspective_target() {
        let owner = owner();
        let foreign = OwnerId::from(UserId::new(uuid::Uuid::now_v7()));
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
        let foreign = OwnerId::from(UserId::new(uuid::Uuid::now_v7()));
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
    async fn create_goal_from_payload_write_denies_denied_context() {
        let owner = owner();
        let req = GoalCreatePayloadWriteRequest {
            owner,
            topology: topology(memory_id()),
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
    async fn transition_goal_denies_denied_context() {
        let owner = owner();
        let req = GoalTransitionRequest {
            owner,
            prior_goal_id: goal_id(),
            transition: GoalTransitionKind::Pause {
                pending: crate::PendingWork::Keep,
            },
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
    async fn modify_goal_authorizes_stored_evidence_without_erasing_carry_intent() {
        let owner = owner();
        let evidence = vec![memory_id(), memory_id()];
        let observed = std::sync::Arc::new(std::sync::Mutex::new(None));
        let mut storage = storage_with_memory(owner, owner, true, EntityKind::Abstraction);
        storage.goal_evidence = Some(evidence.clone());
        storage.observed_modify_evidence = observed.clone();
        let observed_reads = storage.observed_entity_reads.clone();
        let engine = engine_with_ports(storage);
        let req = GoalModifyRequest {
            owner,
            prior_goal_id: goal_id(),
            replacement: payload_write(),
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
            None,
            "the storage command must preserve omitted-evidence carry intent"
        );
        assert_eq!(
            *observed_reads.lock().expect("entity read recorder lock"),
            evidence
                .into_iter()
                .map(EntityId::Memory)
                .collect::<Vec<_>>(),
            "the Engine must still read-admit the exact stored evidence vector"
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

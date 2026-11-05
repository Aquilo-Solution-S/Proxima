//! Typed contracts for Goal-scoped collaboration.
//!
//! Goal routes are additive. They never become an owner role and therefore
//! cannot mint an [`OwnerWritePermit`].

use std::time::SystemTime;
use uuid::Uuid;

use crate::engine::{MemoryTarget, SeriesHandle};
use crate::storage_ports::OwnerWritePermit;
use crate::{EdgeEndpoint, GoalId, MemoryId, OwnerId, OwnerScope, Role, StorageError, UserId};

/// Stable handle for a Goal collaboration route, distinct from an exact
/// [`GoalId`] revision identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[repr(transparent)]
pub struct GoalHandle(Uuid);

impl GoalHandle {
    #[must_use]
    pub const fn new(id: Uuid) -> Self {
        Self(id)
    }
    #[must_use]
    pub const fn into_inner(self) -> Uuid {
        self.0
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub enum PublicationUse {
    Read,
    Fork,
}

impl PublicationUse {
    #[must_use]
    pub const fn fork_allowed(self) -> bool {
        matches!(self, Self::Fork)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GoalAudienceGrantInput {
    pub goal: GoalHandle,
    pub subject: UserId,
    pub role: Role,
    pub expires_at: Option<SystemTime>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GoalPublicationInput {
    pub goal: GoalHandle,
    pub memory: MemoryId,
    pub permitted_use: PublicationUse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GoalWorkBindingInput {
    pub goal: GoalHandle,
    pub series: SeriesHandle,
}

/// Where independent knowledge will be maintained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum MemoryDestination {
    Owner(OwnerId),
    WorkingGoal(GoalHandle),
}

impl From<OwnerId> for MemoryDestination {
    fn from(owner: OwnerId) -> Self {
        Self::Owner(owner)
    }
}

/// Fork one exact A/P version. The idempotency key identifies this copy,
/// independently of later edits to the destination series.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkMemoryRequest {
    pub source: MemoryId,
    pub destination: MemoryDestination,
    pub request_id: crate::verbs::idempotency::IdempotencyKey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkMemoryOutcome {
    pub memory_id: MemoryId,
    pub handle: SeriesHandle,
    pub forked_from: MemoryId,
    pub idempotent_replay: bool,
}

/// Narrow work authority. It has no conversion to `OwnerWritePermit` and
/// cannot reach owner erase, transfer, or membership ports.
///
/// ```compile_fail
/// fn forge(command: proxiima_core::GoalCommandPermit, target: proxiima_core::MemoryTarget) {
///     let _ = proxiima_core::GoalWorkPermit::new(command, proxiima_core::AccessKind::Perspective, target);
/// }
/// ```
#[derive(Debug)]
pub struct GoalWorkPermit {
    command: GoalCommandPermit,
    kind: crate::AccessKind,
    target: MemoryTarget,
}

impl GoalWorkPermit {
    pub(crate) const fn new(
        command: GoalCommandPermit,
        kind: crate::AccessKind,
        target: MemoryTarget,
    ) -> Self {
        Self {
            command,
            kind,
            target,
        }
    }
    #[must_use]
    pub const fn command(&self) -> &GoalCommandPermit {
        &self.command
    }
    #[must_use]
    pub const fn owner(&self) -> OwnerId {
        self.command.controller
    }
    #[must_use]
    pub const fn kind(&self) -> crate::AccessKind {
        self.kind
    }
    #[must_use]
    pub const fn target(&self) -> MemoryTarget {
        self.target
    }
}

/// The destination's independently admitted authority.
#[derive(Debug)]
pub enum ForkDestinationPermit {
    Owner(OwnerWritePermit),
    WorkingGoal(GoalCommandPermit),
}

/// Exact source plus a destination permit, sealed by the Engine after host
/// vetoes. Storage rechecks source-use consent atomically with the copy.
///
/// ```compile_fail
/// fn forge(scope: proxiima_core::OwnerScope, source: proxiima_core::MemoryId,
///          owner: proxiima_core::OwnerId, destination: proxiima_core::ForkDestinationPermit) {
///     let _ = proxiima_core::ForkCommandPermit::new(scope, source, owner, destination, Vec::new());
/// }
/// ```
#[derive(Debug)]
pub struct ForkCommandPermit {
    scope: crate::OwnerScope,
    source: MemoryId,
    source_owner: OwnerId,
    destination: ForkDestinationPermit,
    queued_spaces: Vec<crate::EmbeddingSpace>,
}

/// The exact pin declarations read from a forked revision. This is a storage
/// DTO: it contains identifiers and their declared kinds, never ancestor
/// bodies or sidecar payloads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevisionPinSnapshot {
    pub prior: MemoryId,
    pub owner: OwnerId,
    pub forked_from: MemoryId,
    pub kind: crate::EntityKind,
    pub origins: Vec<EdgeEndpoint>,
    pub references: Vec<EdgeEndpoint>,
    pub goal_references: Vec<GoalId>,
}

impl RevisionPinSnapshot {
    #[must_use]
    pub fn new(
        prior: MemoryId,
        owner: OwnerId,
        forked_from: MemoryId,
        kind: crate::EntityKind,
        origins: Vec<EdgeEndpoint>,
        references: Vec<EdgeEndpoint>,
        goal_references: Vec<GoalId>,
    ) -> Self {
        Self {
            prior,
            owner,
            forked_from,
            kind,
            origins,
            references,
            goal_references,
        }
    }
}

/// A proof that the caller's original read scope admitted one exact forked
/// revision. Only the destination metadata and exact prior are carried to the
/// backend; pin identifiers are returned separately by `revision_pins`.
///
/// ```compile_fail
/// fn forge(scope: proxiima_core::OwnerScope, prior: proxiima_core::MemoryId,
///          owner: proxiima_core::OwnerId) {
///     let _ = proxiima_core::RevisionCopyPermit::new(scope, prior, owner, proxiima_core::EntityKind::Perspective);
/// }
/// ```
#[derive(Debug, Clone)]
pub struct RevisionCopyPermit {
    scope: crate::OwnerScope,
    prior: MemoryId,
    destination_owner: OwnerId,
    kind: crate::EntityKind,
}

impl RevisionCopyPermit {
    pub(crate) const fn new(
        scope: crate::OwnerScope,
        prior: MemoryId,
        destination_owner: OwnerId,
        kind: crate::EntityKind,
    ) -> Self {
        Self {
            scope,
            prior,
            destination_owner,
            kind,
        }
    }
    #[must_use]
    pub const fn scope(&self) -> &crate::OwnerScope {
        &self.scope
    }
    #[must_use]
    pub const fn prior(&self) -> MemoryId {
        self.prior
    }
    #[must_use]
    pub const fn destination_owner(&self) -> OwnerId {
        self.destination_owner
    }
    #[must_use]
    pub const fn kind(&self) -> crate::EntityKind {
        self.kind
    }
}

/// Sealed retained pins used when revising a fork without re-reading hidden
/// source rows. Only the Engine can seal a backend snapshot.
///
/// ```compile_fail
/// fn forge(snapshot: proxiima_core::RevisionPinSnapshot) {
///     let _ = proxiima_core::RetainedPins::seal(snapshot);
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetainedPins {
    prior: MemoryId,
    owner: OwnerId,
    forked_from: MemoryId,
    kind: crate::EntityKind,
    origins: Vec<EdgeEndpoint>,
    references: Vec<EdgeEndpoint>,
    goal_references: Vec<GoalId>,
}

impl RetainedPins {
    pub(crate) fn seal(snapshot: RevisionPinSnapshot) -> Self {
        Self {
            prior: snapshot.prior,
            owner: snapshot.owner,
            forked_from: snapshot.forked_from,
            kind: snapshot.kind,
            origins: snapshot.origins,
            references: snapshot.references,
            goal_references: snapshot.goal_references,
        }
    }
    #[must_use]
    pub const fn prior(&self) -> MemoryId {
        self.prior
    }
    #[must_use]
    pub const fn owner(&self) -> OwnerId {
        self.owner
    }
    #[must_use]
    pub const fn forked_from(&self) -> MemoryId {
        self.forked_from
    }
    #[must_use]
    pub const fn kind(&self) -> crate::EntityKind {
        self.kind
    }
    #[must_use]
    pub fn origins(&self) -> &[EdgeEndpoint] {
        &self.origins
    }
    #[must_use]
    pub fn references(&self) -> &[EdgeEndpoint] {
        &self.references
    }
    #[must_use]
    pub fn goal_references(&self) -> &[GoalId] {
        &self.goal_references
    }
}

impl ForkCommandPermit {
    pub(crate) const fn new(
        scope: crate::OwnerScope,
        source: MemoryId,
        source_owner: OwnerId,
        destination: ForkDestinationPermit,
        queued_spaces: Vec<crate::EmbeddingSpace>,
    ) -> Self {
        Self {
            scope,
            source,
            source_owner,
            destination,
            queued_spaces,
        }
    }
    #[must_use]
    pub const fn scope(&self) -> &crate::OwnerScope {
        &self.scope
    }
    #[must_use]
    pub const fn source(&self) -> MemoryId {
        self.source
    }
    #[must_use]
    pub const fn source_owner(&self) -> OwnerId {
        self.source_owner
    }
    #[must_use]
    pub const fn destination(&self) -> &ForkDestinationPermit {
        &self.destination
    }
    #[must_use]
    pub fn queued_spaces(&self) -> &[crate::EmbeddingSpace] {
        &self.queued_spaces
    }
}

/// Sealed proof that a command is running in a verified Goal principal lane.
/// Construction is crate-private; database state is re-read by storage when
/// the command commits.
///
/// ```compile_fail
/// fn forge(scope: proxiima_core::OwnerScope, goal: proxiima_core::GoalHandle,
///          owner: proxiima_core::OwnerId) {
///     let _ = proxiima_core::GoalCommandPermit::new(scope, goal, owner);
/// }
/// ```
#[derive(Debug)]
pub struct GoalCommandPermit {
    scope: crate::OwnerScope,
    goal: GoalHandle,
    controller: OwnerId,
}

/// Authority accepted by the three existing Goal revision commands.
///
/// The owner route keeps its existing permit; a Goal audience route carries
/// only the sealed Goal command permit.  The enum is deliberately borrowed so
/// it cannot outlive the authorization decision that minted it.
#[derive(Debug, Clone, Copy)]
pub enum GoalEditPermit<'a> {
    Owner(&'a OwnerWritePermit),
    Goal(&'a GoalCommandPermit),
}

impl GoalEditPermit<'_> {
    #[must_use]
    pub const fn owner(&self) -> OwnerId {
        match self {
            Self::Owner(permit) => *permit.owner(),
            Self::Goal(permit) => permit.controller(),
        }
    }

    #[must_use]
    pub const fn goal(&self) -> Option<GoalHandle> {
        match self {
            Self::Owner(_) => None,
            Self::Goal(permit) => Some(permit.goal()),
        }
    }

    #[must_use]
    pub fn scope(&self) -> Option<&OwnerScope> {
        match self {
            Self::Owner(permit) => permit.owner_scope(),
            Self::Goal(permit) => Some(permit.scope()),
        }
    }

    #[must_use]
    pub const fn owner_write(&self) -> Option<&OwnerWritePermit> {
        match self {
            Self::Owner(permit) => Some(permit),
            Self::Goal(_) => None,
        }
    }
}

/// Authentication-only marker used by [`crate::OwnerScope`]. It cannot be
/// constructed by storage callers and is deliberately absent from owner-role
/// collections.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GoalPrincipalScope {
    pub(crate) subject: UserId,
}

impl GoalPrincipalScope {
    pub(crate) const fn new(subject: UserId) -> Self {
        Self { subject }
    }
}

impl GoalPrincipalScope {
    #[must_use]
    pub fn subject(&self) -> UserId {
        self.subject
    }
}

impl GoalCommandPermit {
    pub(crate) const fn new(
        scope: crate::OwnerScope,
        goal: GoalHandle,
        controller: OwnerId,
    ) -> Self {
        Self {
            scope,
            goal,
            controller,
        }
    }
    #[must_use]
    pub const fn goal(&self) -> GoalHandle {
        self.goal
    }
    #[must_use]
    pub const fn controller(&self) -> OwnerId {
        self.controller
    }
    #[must_use]
    pub fn subject(&self) -> UserId {
        self.scope.subject()
    }
    #[must_use]
    pub fn scope(&self) -> &crate::OwnerScope {
        &self.scope
    }
}

#[allow(unused_variables)]
#[async_trait::async_trait]
pub trait GoalCollaborationPort: Send + Sync {
    async fn revision_pins(
        &self,
        _permit: &RevisionCopyPermit,
    ) -> Result<Option<RevisionPinSnapshot>, StorageError> {
        Ok(None)
    }
    async fn author_work_memory(
        &self,
        _request: &crate::AuthorDerivedRequest<'_>,
        _permit: &GoalWorkPermit,
        _proof: crate::storage_ports::OperatorWriteProof,
    ) -> Result<crate::AuthorDerivedOutcome, StorageError> {
        Err(StorageError::Internal(
            "Goal work is not supported by this backend".into(),
        ))
    }

    async fn fork_memory(
        &self,
        _request: &ForkMemoryRequest,
        _permit: &ForkCommandPermit,
    ) -> Result<ForkMemoryOutcome, StorageError> {
        Err(StorageError::Internal(
            "Memory forks are not supported by this backend".into(),
        ))
    }
    async fn read_owner_candidates(
        &self,
        _scope: &crate::OwnerScope,
    ) -> Result<Vec<crate::OwnerId>, StorageError> {
        Ok(Vec::new())
    }
    async fn controller_owner(
        &self,
        scope: &crate::OwnerScope,
        goal: GoalHandle,
    ) -> Result<Option<OwnerId>, StorageError> {
        Ok(None)
    }
    /// Resolve one exact Goal revision to its stable collaboration handle
    /// under the caller's current read scope.
    async fn goal_handle_for_version(
        &self,
        _scope: &crate::OwnerScope,
        _goal: GoalId,
    ) -> Result<Option<GoalHandle>, StorageError> {
        Ok(None)
    }
    async fn grant_access(
        &self,
        input: &GoalAudienceGrantInput,
        permit: &GoalCommandPermit,
    ) -> Result<(), StorageError> {
        Err(StorageError::Internal(
            "Goal collaboration command unavailable".into(),
        ))
    }
    async fn revoke_access(
        &self,
        goal: GoalHandle,
        subject: UserId,
        permit: &GoalCommandPermit,
    ) -> Result<(), StorageError> {
        Err(StorageError::Internal(
            "Goal collaboration command unavailable".into(),
        ))
    }
    async fn publish(
        &self,
        input: &GoalPublicationInput,
        permit: &GoalCommandPermit,
        source: &OwnerWritePermit,
    ) -> Result<(), StorageError> {
        Err(StorageError::Internal(
            "Goal collaboration command unavailable".into(),
        ))
    }
    async fn unpublish(
        &self,
        goal: GoalHandle,
        memory: MemoryId,
        permit: &GoalCommandPermit,
    ) -> Result<(), StorageError> {
        Err(StorageError::Internal(
            "Goal collaboration command unavailable".into(),
        ))
    }
    async fn bind_work(
        &self,
        input: &GoalWorkBindingInput,
        permit: &GoalCommandPermit,
        source: &OwnerWritePermit,
    ) -> Result<(), StorageError> {
        Err(StorageError::Internal(
            "Goal collaboration command unavailable".into(),
        ))
    }
    async fn unbind_work(
        &self,
        input: &GoalWorkBindingInput,
        permit: &GoalCommandPermit,
    ) -> Result<(), StorageError> {
        Err(StorageError::Internal(
            "Goal collaboration command unavailable".into(),
        ))
    }
}

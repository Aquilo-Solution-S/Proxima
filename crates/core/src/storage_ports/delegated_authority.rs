//! Durable, bearer-bounded authority for registered background commands.
//!
//! Queue rows persist only [`DelegationId`]. Redemption checks the exact queued
//! command, current deployment profile, grant revocation, current membership,
//! and the authenticator epoch when the host implements epoch revocation. It
//! returns a non-cloneable [`DelegatedPhase`](crate::DelegatedPhase), never a
//! reusable serialized [`AuthzContext`]. Engine/blob calls then enforce the
//! same-runtime binding, exact owner, recorded role ceiling, and finite expiry.
//!
//! Registered linked workers remain trusted in-process to choose among those
//! allowed operations: command scope binds queue routing/redemption, not an
//! in-process operation sandbox. Revocation, epoch bumps, or membership changes
//! deny the next redemption; they do not cancel an already-redeemed phase.

use std::sync::Arc;
use std::time::SystemTime;

use async_trait::async_trait;
use thiserror::Error;
use uuid::Uuid;

use crate::authz::DelegationRuntimeBinding;
use crate::{
    AccessError, AuthPath, Authenticator, AuthzContext, DelegatedPhase, DelegationRuntimeAuthority,
    FlavorRegistryFrozen, GoalWakeToolId, OwnerAccessPort, OwnerRef, OwnerRoles, ProtocolError,
    Role, StorageError, ToolScope, UserId,
};

/// Opaque `UUIDv7` handle persisted by a queued job.
#[derive(Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct DelegationId(Uuid);

impl DelegationId {
    #[doc(hidden)]
    #[must_use]
    pub const fn from_uuid(inner: Uuid) -> Self {
        Self(inner)
    }

    #[must_use]
    pub const fn into_uuid(self) -> Uuid {
        self.0
    }
}

impl std::fmt::Display for DelegationId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("delegation:<redacted>")
    }
}

impl std::fmt::Debug for DelegationId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("DelegationId(<redacted>)")
    }
}

/// One canonical registered flat tool or exact dispatcher action.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize)]
pub struct DelegatedCommand(String);

impl DelegatedCommand {
    /// Parse through the same registry-aware grammar as Goal wake tool ids.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` for a non-canonical, unregistered, grouped
    /// tool without an action, or action not present in `registry`.
    pub fn parse(
        raw: impl Into<String>,
        registry: &FlavorRegistryFrozen,
    ) -> Result<Self, ProtocolError> {
        GoalWakeToolId::parse(raw, registry).map(Self::from)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn tool(&self) -> &str {
        self.0
            .split_once(':')
            .map_or(self.0.as_str(), |(tool, _)| tool)
    }

    #[must_use]
    pub fn action(&self) -> Option<&str> {
        self.0.split_once(':').map(|(_, action)| action)
    }

    fn from_storage_parts(tool: &str, action: Option<&str>) -> Result<Self, StorageError> {
        let raw = action.map_or_else(|| tool.to_owned(), |action| format!("{tool}:{action}"));
        let valid_shape = !raw.is_empty()
            && raw.trim() == raw
            && raw.chars().count() <= crate::verbs::goal_write::MAX_WAKE_TOOL_ID_CHARS
            && !raw.contains('/')
            && match raw.split_once(':') {
                Some((tool, action)) => {
                    !action.contains(':')
                        && crate::provider_safe_tool_name(tool) == tool
                        && crate::provider_safe_tool_name(action) == action
                }
                None => crate::provider_safe_tool_name(&raw) == raw,
            };
        if !valid_shape {
            return Err(StorageError::ConstraintViolation(
                "stored delegation command is not canonical".into(),
            ));
        }
        Ok(Self(raw))
    }

    #[must_use]
    fn tool_scope(&self) -> ToolScope {
        ToolScope::Palette(vec![self.0.clone()])
    }
}

impl From<GoalWakeToolId> for DelegatedCommand {
    fn from(value: GoalWakeToolId) -> Self {
        Self(value.into_string())
    }
}

/// Supported issue response. The queue persists `id`; `expires_at` lets the
/// scheduler avoid claiming work that cannot be redeemed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DelegationIssued {
    pub id: DelegationId,
    pub expires_at: SystemTime,
}

/// Result of an authorized, idempotent revocation request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DelegationRevocation {
    Revoked,
    AlreadyRevoked,
}

/// Typed delegated-authority failure.
#[derive(Debug, Error)]
pub enum DelegatedAuthorityError {
    #[error("only a host bearer may issue or revoke delegated authority")]
    HostBearerRequired,
    #[error("authenticated subject is missing")]
    MissingSubject,
    #[error("the source bearer has no finite expiry")]
    MissingBearerExpiry,
    #[error("the source bearer or delegated grant has expired")]
    Expired,
    #[error("delegated role ceilings cannot include owner management")]
    ManagementNotDelegable,
    #[error("requested role ceiling exceeds current owner membership")]
    RoleCeilingExceeded,
    #[error("delegated command `{0}` is outside the current caller or deployment tool scope")]
    ToolScopeDenied(String),
    #[error("delegated command is not registered in the current runtime: {0}")]
    CommandUnavailable(String),
    #[error("delegation grant was not found for the expected owner")]
    NotFound,
    #[error("delegation grant is revoked")]
    Revoked,
    #[error("delegation command does not match the queued job")]
    CommandMismatch,
    #[error("the subject no longer has membership on the delegated owner")]
    MembershipRevoked,
    #[error("the subject's current owner role no longer holds the delegated role ceiling")]
    RoleCeilingNoLongerHeld,
    #[error("the source identity was revoked after this grant was issued")]
    AuthEpochRevoked,
    #[error("only the issuer or a current owner manager may revoke this grant")]
    RevocationDenied,
    #[error(transparent)]
    Access(#[from] AccessError),
    #[error(transparent)]
    Storage(#[from] StorageError),
}

/// Nonconstructible witness sealing delegation-table mutations to the
/// validated service path.
#[doc(hidden)]
#[derive(Debug)]
pub struct DelegationMutationPermit {
    _private: (),
}

impl DelegationMutationPermit {
    const fn new() -> Self {
        Self { _private: () }
    }
}

/// Persistence DTO used only by a delegation store adapter.
#[doc(hidden)]
#[derive(Debug)]
pub struct DelegationGrantStorage {
    pub delegation_id: DelegationId,
    pub subject: UserId,
    pub owner: OwnerRef,
    pub tool: String,
    pub action: Option<String>,
    pub role_ceiling: Role,
    pub expires_at: SystemTime,
    pub auth_epoch: u64,
    pub issued_at: SystemTime,
    pub revoked_at: Option<SystemTime>,
    pub revoked_by: Option<UserId>,
}

/// Validated durable row. Unsupported persistence detail; workers retain only
/// [`DelegationId`].
#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelegationGrant {
    delegation_id: DelegationId,
    subject: UserId,
    owner: OwnerRef,
    command: DelegatedCommand,
    role_ceiling: Role,
    expires_at: SystemTime,
    auth_epoch: u64,
    issued_at: SystemTime,
    revoked_at: Option<SystemTime>,
    revoked_by: Option<UserId>,
}

impl DelegationGrant {
    fn issued(
        subject: UserId,
        owner: OwnerRef,
        command: DelegatedCommand,
        role_ceiling: Role,
        expires_at: SystemTime,
        auth_epoch: u64,
        issued_at: SystemTime,
    ) -> Self {
        Self {
            delegation_id: DelegationId::from_uuid(Uuid::now_v7()),
            subject,
            owner,
            command,
            role_ceiling,
            expires_at,
            auth_epoch,
            issued_at,
            revoked_at: None,
            revoked_by: None,
        }
    }

    /// Validate one row decoded by the backend adapter.
    #[doc(hidden)]
    pub fn from_storage(
        _permit: &DelegationMutationPermit,
        stored: &DelegationGrantStorage,
    ) -> Result<Self, StorageError> {
        if stored.role_ceiling.manages() {
            return Err(StorageError::ConstraintViolation(
                "stored delegation role ceiling includes management".into(),
            ));
        }
        if stored.expires_at <= stored.issued_at {
            return Err(StorageError::ConstraintViolation(
                "stored delegation expiry is not after issue time".into(),
            ));
        }
        if stored.revoked_at.is_some() != stored.revoked_by.is_some() {
            return Err(StorageError::ConstraintViolation(
                "stored delegation revocation shape is invalid".into(),
            ));
        }
        let command = DelegatedCommand::from_storage_parts(&stored.tool, stored.action.as_deref())?;
        Ok(Self {
            delegation_id: stored.delegation_id,
            subject: stored.subject,
            owner: stored.owner,
            command,
            role_ceiling: stored.role_ceiling,
            expires_at: stored.expires_at,
            auth_epoch: stored.auth_epoch,
            issued_at: stored.issued_at,
            revoked_at: stored.revoked_at,
            revoked_by: stored.revoked_by,
        })
    }

    #[doc(hidden)]
    #[must_use]
    pub const fn delegation_id(&self) -> DelegationId {
        self.delegation_id
    }

    #[doc(hidden)]
    #[must_use]
    pub const fn subject(&self) -> UserId {
        self.subject
    }

    #[doc(hidden)]
    #[must_use]
    pub const fn owner(&self) -> OwnerRef {
        self.owner
    }

    #[doc(hidden)]
    #[must_use]
    pub fn command(&self) -> &DelegatedCommand {
        &self.command
    }

    #[doc(hidden)]
    #[must_use]
    pub const fn role_ceiling(&self) -> Role {
        self.role_ceiling
    }

    #[doc(hidden)]
    #[must_use]
    pub const fn expires_at(&self) -> SystemTime {
        self.expires_at
    }

    #[doc(hidden)]
    #[must_use]
    pub const fn auth_epoch(&self) -> u64 {
        self.auth_epoch
    }

    #[doc(hidden)]
    #[must_use]
    pub const fn issued_at(&self) -> SystemTime {
        self.issued_at
    }

    #[doc(hidden)]
    #[must_use]
    pub const fn revoked_at(&self) -> Option<SystemTime> {
        self.revoked_at
    }

    #[doc(hidden)]
    #[must_use]
    pub const fn revoked_by(&self) -> Option<UserId> {
        self.revoked_by
    }
}

/// Unsupported persistence port sealed by [`DelegationMutationPermit`].
#[doc(hidden)]
#[async_trait]
pub trait DelegationStorePort: Send + Sync {
    async fn insert(
        &self,
        permit: &DelegationMutationPermit,
        grant: &DelegationGrant,
    ) -> Result<(), StorageError>;

    async fn load(
        &self,
        permit: &DelegationMutationPermit,
        delegation_id: DelegationId,
        expected_owner: OwnerRef,
    ) -> Result<Option<DelegationGrant>, StorageError>;

    async fn revoke(
        &self,
        permit: &DelegationMutationPermit,
        delegation_id: DelegationId,
        expected_owner: OwnerRef,
        revoked_at: SystemTime,
        revoked_by: UserId,
    ) -> Result<bool, StorageError>;
}

/// Runtime-composed issue/redeem/revoke service shared by tools and workers.
///
/// Exact command registration and the current deployment profile are checked
/// both at issue and redemption. The opaque phase bounds what Engine accepts;
/// the registered linked worker implementation itself is trusted in-process.
#[derive(Clone)]
pub struct DelegatedAuthorityService {
    store: Arc<dyn DelegationStorePort>,
    mutation_permit: Arc<DelegationMutationPermit>,
    owner_access: Arc<dyn OwnerAccessPort>,
    authenticator: Arc<dyn Authenticator>,
    registry: Arc<FlavorRegistryFrozen>,
    deployment_tool_scope: ToolScope,
    runtime_binding: DelegationRuntimeBinding,
}

impl std::fmt::Debug for DelegatedAuthorityService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DelegatedAuthorityService")
            .finish_non_exhaustive()
    }
}

impl DelegatedAuthorityService {
    /// Runtime-only composition seam. Applications obtain the shared service
    /// from `FlavorServices` rather than constructing a second instance.
    #[doc(hidden)]
    #[must_use]
    pub fn new(
        store: Arc<dyn DelegationStorePort>,
        owner_access: Arc<dyn OwnerAccessPort>,
        authenticator: Arc<dyn Authenticator>,
        registry: Arc<FlavorRegistryFrozen>,
        deployment_tool_scope: ToolScope,
        runtime_authority: &DelegationRuntimeAuthority,
    ) -> Self {
        Self {
            store,
            mutation_permit: Arc::new(DelegationMutationPermit::new()),
            owner_access,
            authenticator,
            registry,
            deployment_tool_scope,
            runtime_binding: runtime_authority.binding(),
        }
    }

    /// Issue one finite bearer-bounded grant after current epoch, membership,
    /// registry, caller-scope, and deployment-profile checks.
    ///
    /// # Errors
    ///
    /// No row is inserted unless all checks succeed.
    pub async fn issue(
        &self,
        source: &AuthzContext,
        owner: OwnerRef,
        command: DelegatedCommand,
        role_ceiling: Role,
    ) -> Result<DelegationIssued, DelegatedAuthorityError> {
        let (subject, expires_at) = Self::validate_host_bearer(source)?;
        let issued_at = SystemTime::now();
        if expires_at <= issued_at {
            return Err(DelegatedAuthorityError::Expired);
        }
        if role_ceiling.manages() {
            return Err(DelegatedAuthorityError::ManagementNotDelegable);
        }
        self.revalidate_epoch(subject, source.auth_epoch()).await?;
        let current_roles = self.current_roles(subject).await?;
        let current_role = current_roles
            .role_for(&owner)
            .ok_or(DelegatedAuthorityError::RoleCeilingExceeded)?;
        if !current_role.dominates(role_ceiling) {
            return Err(DelegatedAuthorityError::RoleCeilingExceeded);
        }
        self.validate_current_command(&command)?;
        self.validate_scopes(source.tool_scope(), &command)?;

        let grant = DelegationGrant::issued(
            subject,
            owner,
            command,
            role_ceiling,
            expires_at,
            source.auth_epoch(),
            issued_at,
        );
        let issued = DelegationIssued {
            id: grant.delegation_id(),
            expires_at,
        };
        self.store.insert(&self.mutation_permit, &grant).await?;
        Ok(issued)
    }

    /// Redeem one queued phase against current owner, command, registry,
    /// deployment profile, membership, authentication epoch, and expiry.
    ///
    /// # Errors
    ///
    /// Returns no reusable [`AuthzContext`]; Engine expiry is checked again at
    /// the start of every delegated-capable operation.
    pub async fn redeem_phase(
        &self,
        delegation_id: DelegationId,
        expected_owner: OwnerRef,
        expected_command: &DelegatedCommand,
    ) -> Result<DelegatedPhase, DelegatedAuthorityError> {
        let grant = self
            .store
            .load(&self.mutation_permit, delegation_id, expected_owner)
            .await?
            .ok_or(DelegatedAuthorityError::NotFound)?;
        if grant.delegation_id() != delegation_id || grant.owner() != expected_owner {
            return Err(DelegatedAuthorityError::NotFound);
        }
        if grant.command() != expected_command {
            return Err(DelegatedAuthorityError::CommandMismatch);
        }
        if grant.revoked_at().is_some() {
            return Err(DelegatedAuthorityError::Revoked);
        }
        if grant.expires_at() <= SystemTime::now() {
            return Err(DelegatedAuthorityError::Expired);
        }
        self.validate_current_command(grant.command())?;
        self.validate_scopes(&ToolScope::All, grant.command())?;
        self.revalidate_epoch(grant.subject(), grant.auth_epoch())
            .await?;
        let current_roles = self.current_roles(grant.subject()).await?;
        let current_role = current_roles
            .role_for(&expected_owner)
            .ok_or(DelegatedAuthorityError::MembershipRevoked)?;
        if !current_role.dominates(grant.role_ceiling()) {
            return Err(DelegatedAuthorityError::RoleCeilingNoLongerHeld);
        }
        let effective_role = current_role.meet(grant.role_ceiling());

        let authz = AuthzContext::server_resolved(
            OwnerRoles::scoped_to(grant.subject(), expected_owner, effective_role),
            AuthPath::Delegated,
        )
        .with_expires_at(Some(grant.expires_at()))
        .with_auth_epoch(grant.auth_epoch())
        .with_tool_scope(grant.command().tool_scope());
        Ok(DelegatedPhase::new(
            authz,
            grant.expires_at(),
            self.runtime_binding.clone(),
        ))
    }

    /// Revoke as the issuer or a current owner manager. Both paths require a
    /// current host-bearer epoch and current membership on `expected_owner`.
    ///
    /// # Errors
    ///
    /// The owner is part of both load and update predicates. Revocation is
    /// idempotent under concurrent callers.
    pub async fn revoke(
        &self,
        caller: &AuthzContext,
        delegation_id: DelegationId,
        expected_owner: OwnerRef,
    ) -> Result<DelegationRevocation, DelegatedAuthorityError> {
        let (caller_subject, _) = Self::validate_host_bearer(caller)?;
        self.revalidate_epoch(caller_subject, caller.auth_epoch())
            .await?;
        let grant = self
            .store
            .load(&self.mutation_permit, delegation_id, expected_owner)
            .await?
            .ok_or(DelegatedAuthorityError::NotFound)?;
        if grant.delegation_id() != delegation_id || grant.owner() != expected_owner {
            return Err(DelegatedAuthorityError::NotFound);
        }
        let current_roles = self.current_roles(caller_subject).await?;
        let current_role = current_roles
            .role_for(&expected_owner)
            .ok_or(DelegatedAuthorityError::RevocationDenied)?;
        if caller_subject != grant.subject() && !current_role.manages() {
            return Err(DelegatedAuthorityError::RevocationDenied);
        }
        if grant.revoked_at().is_some() {
            return Ok(DelegationRevocation::AlreadyRevoked);
        }
        if self
            .store
            .revoke(
                &self.mutation_permit,
                delegation_id,
                expected_owner,
                SystemTime::now(),
                caller_subject,
            )
            .await?
        {
            Ok(DelegationRevocation::Revoked)
        } else {
            Ok(DelegationRevocation::AlreadyRevoked)
        }
    }

    fn validate_host_bearer(
        source: &AuthzContext,
    ) -> Result<(UserId, SystemTime), DelegatedAuthorityError> {
        if source.auth_path() != AuthPath::HostBearer {
            return Err(DelegatedAuthorityError::HostBearerRequired);
        }
        let subject = source
            .subject()
            .ok_or(DelegatedAuthorityError::MissingSubject)?;
        let expires_at = source
            .expires_at()
            .ok_or(DelegatedAuthorityError::MissingBearerExpiry)?;
        if expires_at <= SystemTime::now() {
            return Err(DelegatedAuthorityError::Expired);
        }
        Ok((subject, expires_at))
    }

    async fn revalidate_epoch(
        &self,
        subject: UserId,
        auth_epoch: u64,
    ) -> Result<(), DelegatedAuthorityError> {
        let principal = OwnerRef::Personal(subject);
        if self.authenticator.current_auth_epoch(&principal).await > auth_epoch {
            return Err(DelegatedAuthorityError::AuthEpochRevoked);
        }
        Ok(())
    }

    async fn current_roles(&self, subject: UserId) -> Result<OwnerRoles, DelegatedAuthorityError> {
        let roles = self.owner_access.resolve_roles_for_subject(subject).await?;
        if roles.subject() != subject {
            return Err(AccessError::Resolution(
                "owner access resolver returned roles for a different subject".into(),
            )
            .into());
        }
        Ok(roles)
    }

    fn validate_current_command(
        &self,
        command: &DelegatedCommand,
    ) -> Result<(), DelegatedAuthorityError> {
        let current = DelegatedCommand::parse(command.as_str(), &self.registry)
            .map_err(|error| DelegatedAuthorityError::CommandUnavailable(error.message))?;
        if current != *command {
            return Err(DelegatedAuthorityError::CommandUnavailable(
                command.as_str().to_owned(),
            ));
        }
        Ok(())
    }

    fn validate_scopes(
        &self,
        caller_scope: &ToolScope,
        command: &DelegatedCommand,
    ) -> Result<(), DelegatedAuthorityError> {
        let allows = |scope: &ToolScope| match command.action() {
            Some(action) => {
                scope.allows(command.tool()) || scope.allows_action(command.tool(), action)
            }
            None => scope.allows(command.tool()),
        };
        if !allows(caller_scope) || !allows(&self.deployment_tool_scope) {
            return Err(DelegatedAuthorityError::ToolScopeDenied(
                command.as_str().to_owned(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::time::Duration;

    use futures::future::BoxFuture;
    use serde::Deserialize;

    use super::*;
    use crate::access::AccessKind;
    use crate::auth::{AuthError, Credentials};
    use crate::error::ErrorCode;
    use crate::mcp::{
        McpActionArgSpec, McpTool, McpToolAnnotations, McpToolAudience, McpToolCtx, McpToolError,
    };
    use crate::query::QueryRequest;
    use crate::{FactPayload, FlavorRegistry, GroupId, PayloadKeyBuilder, Relation};

    const TOOL_NAME: &str = "test-delegation_worker";
    const DISPATCHER_TOOL_NAME: &str = "test-delegation_dispatcher";

    #[derive(schemars::JsonSchema, Deserialize)]
    struct WorkerArgs {}

    struct WorkerTool;

    impl McpTool for WorkerTool {
        const NAME: &'static str = TOOL_NAME;
        const DESCRIPTION: &'static str = "delegation test worker";
        const ANNOTATIONS: Option<McpToolAnnotations> =
            Some(McpToolAnnotations::new().read_only(false).open_world(false));
        type Args = WorkerArgs;
        type Output = ();

        fn call(
            _ctx: McpToolCtx,
            _args: WorkerArgs,
        ) -> BoxFuture<'static, Result<(), McpToolError>> {
            Box::pin(async { Ok(()) })
        }
    }

    #[derive(schemars::JsonSchema, Deserialize)]
    #[serde(tag = "action", rename_all = "snake_case")]
    enum DispatcherArgs {
        Run {},
        Other {},
    }

    struct DispatcherTool;

    impl McpTool for DispatcherTool {
        const NAME: &'static str = DISPATCHER_TOOL_NAME;
        const DESCRIPTION: &'static str = "delegation dispatcher test worker";
        const ACTION_ARG_SPECS: &'static [McpActionArgSpec] = &[
            McpActionArgSpec {
                action: "run",
                allowed_fields: &[],
                required_fields: &[],
                annotations: Some(McpToolAnnotations::new().read_only(false).open_world(false)),
                audience: McpToolAudience::Shared,
            },
            McpActionArgSpec {
                action: "other",
                allowed_fields: &[],
                required_fields: &[],
                annotations: Some(McpToolAnnotations::new().read_only(false).open_world(false)),
                audience: McpToolAudience::Shared,
            },
        ];
        type Args = DispatcherArgs;
        type Output = ();

        fn call(
            _ctx: McpToolCtx,
            _args: DispatcherArgs,
        ) -> BoxFuture<'static, Result<(), McpToolError>> {
            Box::pin(async { Ok(()) })
        }
    }

    #[derive(Debug, serde::Serialize, Deserialize)]
    struct TestFact {
        value: String,
    }

    impl FactPayload for TestFact {
        const SCHEMA_ID: &'static str = "test/delegation-fact";
        const SCHEMA_VERSION: u32 = 1;

        fn receipt_key(&self) -> Vec<u8> {
            let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
            key.field_str("value", &self.value);
            key.finish()
        }

        fn render(&self) -> String {
            self.value.clone()
        }
    }

    #[derive(Debug, Default)]
    struct MemoryStore {
        grant: Mutex<Option<DelegationGrant>>,
        inserts: AtomicUsize,
        loads: AtomicUsize,
        revokes: AtomicUsize,
    }

    #[async_trait]
    impl DelegationStorePort for MemoryStore {
        async fn insert(
            &self,
            _permit: &DelegationMutationPermit,
            grant: &DelegationGrant,
        ) -> Result<(), StorageError> {
            self.inserts.fetch_add(1, Ordering::SeqCst);
            *self.grant.lock().expect("grant mutex") = Some(grant.clone());
            Ok(())
        }

        async fn load(
            &self,
            _permit: &DelegationMutationPermit,
            _delegation_id: DelegationId,
            _expected_owner: OwnerRef,
        ) -> Result<Option<DelegationGrant>, StorageError> {
            self.loads.fetch_add(1, Ordering::SeqCst);
            Ok(self.grant.lock().expect("grant mutex").clone())
        }

        async fn revoke(
            &self,
            _permit: &DelegationMutationPermit,
            _delegation_id: DelegationId,
            _expected_owner: OwnerRef,
            _revoked_at: SystemTime,
            _revoked_by: UserId,
        ) -> Result<bool, StorageError> {
            self.revokes.fetch_add(1, Ordering::SeqCst);
            Ok(true)
        }
    }

    #[derive(Debug)]
    struct TestOwnerAccess {
        requested_subject: UserId,
        returned_subject: Mutex<UserId>,
        owner: OwnerRef,
        role: Mutex<Option<Role>>,
    }

    #[async_trait]
    impl OwnerAccessPort for TestOwnerAccess {
        async fn resolve_roles_for_subject(
            &self,
            subject: UserId,
        ) -> Result<OwnerRoles, AccessError> {
            assert_eq!(subject, self.requested_subject);
            let returned_subject = *self.returned_subject.lock().expect("subject mutex");
            let role = *self.role.lock().expect("role mutex");
            OwnerRoles::for_subject(
                returned_subject,
                role.into_iter().map(|role| (self.owner, role)),
            )
        }
    }

    #[derive(Debug, Default)]
    struct EpochAuthenticator(AtomicU64);

    #[async_trait]
    impl Authenticator for EpochAuthenticator {
        async fn authenticate(&self, _creds: &Credentials) -> Result<AuthzContext, AuthError> {
            Err(AuthError::AuthRequired)
        }

        async fn current_auth_epoch(&self, _principal: &OwnerRef) -> u64 {
            self.0.load(Ordering::SeqCst)
        }
    }

    fn registry(with_tool: bool) -> FlavorRegistryFrozen {
        let mut registry = FlavorRegistry::new();
        registry.add_fact_schema_or_panic_for_tests::<TestFact>();
        if with_tool {
            registry.add_mcp_tool_or_panic_for_tests::<WorkerTool>("test-delegation");
        }
        registry.freeze_or_panic_for_tests()
    }

    fn dispatcher_registry() -> FlavorRegistryFrozen {
        let mut registry = FlavorRegistry::new();
        registry.add_mcp_tool_or_panic_for_tests::<DispatcherTool>("test-delegation");
        registry.freeze_or_panic_for_tests()
    }

    fn fact() -> crate::FactWriteCommand {
        crate::FactWriteCommand::from_payload(
            "test/source",
            &TestFact {
                value: "one".into(),
            },
            time::OffsetDateTime::now_utc(),
        )
    }

    fn source(
        subject: UserId,
        owner: OwnerRef,
        expires_at: SystemTime,
        scope: ToolScope,
    ) -> AuthzContext {
        AuthzContext::for_subject_with_role(
            subject,
            [(owner, Role::editor())],
            AuthPath::HostBearer,
        )
        .with_expires_at(Some(expires_at))
        .with_tool_scope(scope)
    }

    fn runtime(
        frozen: FlavorRegistryFrozen,
        store: Arc<MemoryStore>,
        owner_access: Arc<TestOwnerAccess>,
        authenticator: Arc<EpochAuthenticator>,
        deployment_scope: ToolScope,
    ) -> (crate::Engine, DelegatedAuthorityService, DelegatedCommand) {
        let (engine, _system, delegation_runtime) =
            crate::Engine::new(frozen).into_runtime_authorities();
        let registry = Arc::new(engine.registry().clone());
        let command = DelegatedCommand::parse(TOOL_NAME, &registry).expect("registered command");
        let service = DelegatedAuthorityService::new(
            store,
            owner_access,
            authenticator,
            registry,
            deployment_scope,
            &delegation_runtime,
        );
        (engine, service, command)
    }

    fn fixtures() -> (
        UserId,
        OwnerRef,
        Arc<MemoryStore>,
        Arc<TestOwnerAccess>,
        Arc<EpochAuthenticator>,
    ) {
        let subject = UserId::new(Uuid::now_v7());
        let owner = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
        (
            subject,
            owner,
            Arc::new(MemoryStore::default()),
            Arc::new(TestOwnerAccess {
                requested_subject: subject,
                returned_subject: Mutex::new(subject),
                owner,
                role: Mutex::new(Some(Role::editor())),
            }),
            Arc::new(EpochAuthenticator::default()),
        )
    }

    #[tokio::test]
    async fn phase_allows_bounded_fact_authorization_and_raw_delegated_is_denied() {
        let (subject, owner, store, access, authenticator) = fixtures();
        let (engine, service, command) =
            runtime(registry(true), store, access, authenticator, ToolScope::All);
        let caller = source(
            subject,
            owner,
            SystemTime::now() + Duration::from_secs(30),
            ToolScope::Palette(vec![TOOL_NAME.into()]),
        );
        let issued = service
            .issue(&caller, owner, command.clone(), Role::editor())
            .await
            .expect("issue");
        let phase = service
            .redeem_phase(issued.id, owner, &command)
            .await
            .expect("redeem");
        engine
            .authorize_fact_ingest(&phase, Relation::Ingest, fact(), &[])
            .await
            .expect("phase may authorize configured Fact write");

        let raw = AuthzContext::for_subject_with_role(
            subject,
            [(owner, Role::editor())],
            AuthPath::Delegated,
        );
        let write_err = engine
            .authorize_owner_write(&raw, &owner, AccessKind::Fact)
            .await
            .expect_err("raw delegated context must not mint a permit");
        assert_eq!(write_err.code, ErrorCode::Forbidden);
        let query_err = engine
            .query(&raw, &QueryRequest::for_owner(owner))
            .await
            .expect_err("unconverted query must reject raw delegated context");
        assert_eq!(query_err.code, ErrorCode::Forbidden);
        let transfer_err = engine
            .transfer_to_owner(
                &raw,
                crate::access::EntityId::Memory(crate::MemoryId::new(Uuid::now_v7())),
                crate::OwnerRef::Group(crate::GroupId::new(Uuid::now_v7())),
            )
            .await
            .expect_err("raw delegated context must be denied before owner lookup");
        assert_eq!(transfer_err.code, ErrorCode::Forbidden);
        let wake_err = engine
            .read_goal_wake_configs(&raw, &[])
            .await
            .expect_err("empty reads still reject raw delegated context");
        assert_eq!(wake_err.code, ErrorCode::Forbidden);
        let backfill_err = engine
            .backfill_missing_embeddings(&raw, &owner, 1)
            .await
            .expect_err("missing embedder still rejects raw delegated context");
        assert_eq!(backfill_err.code, ErrorCode::Forbidden);
    }

    #[tokio::test]
    async fn split_fact_witness_rechecks_expiry_before_commit() {
        let (subject, owner, store, access, authenticator) = fixtures();
        let (engine, service, command) =
            runtime(registry(true), store, access, authenticator, ToolScope::All);
        let caller = source(
            subject,
            owner,
            SystemTime::now() + Duration::from_secs(30),
            ToolScope::All,
        );
        let issued = service
            .issue(&caller, owner, command.clone(), Role::editor())
            .await
            .expect("issue");
        let mut phase = service
            .redeem_phase(issued.id, owner, &command)
            .await
            .expect("redeem");
        let mut authorized = engine
            .authorize_fact_ingest(&phase, Relation::Ingest, fact(), &[])
            .await
            .expect("authorize before expiry");
        authorized.expire_delegated_write_for_test();
        let err = engine
            .ingest_fact_with_typed_sidecar(&authorized, &[], None)
            .await
            .expect_err("expired split witness must fail before storage");
        assert_eq!(err.code, ErrorCode::Forbidden);
        phase.expire_for_test();
        let err = engine
            .authorize_fact_ingest(&phase, Relation::Ingest, fact(), &[])
            .await
            .expect_err("phase itself expires at every operation start");
        assert_eq!(err.code, ErrorCode::Forbidden);
    }

    #[tokio::test]
    async fn foreign_runtime_phase_is_denied_before_engine_work() {
        let (subject, owner, store, access, authenticator) = fixtures();
        let (foreign_engine, service, command) =
            runtime(registry(true), store, access, authenticator, ToolScope::All);
        let target_engine = crate::Engine::new(foreign_engine.registry().clone());
        let caller = source(
            subject,
            owner,
            SystemTime::now() + Duration::from_secs(30),
            ToolScope::All,
        );
        let issued = service
            .issue(&caller, owner, command.clone(), Role::editor())
            .await
            .expect("issue");
        let phase = service
            .redeem_phase(issued.id, owner, &command)
            .await
            .expect("redeem");
        let err = target_engine
            .authorize_fact_ingest(&phase, Relation::Ingest, fact(), &[])
            .await
            .expect_err("foreign phase must fail before schema/storage work");
        assert_eq!(err.code, ErrorCode::Forbidden);
    }

    #[tokio::test]
    async fn denials_precede_store_mutation_and_adapter_subject_is_rechecked() {
        let (subject, owner, store, access, authenticator) = fixtures();
        let (_engine, service, command) = runtime(
            registry(true),
            store.clone(),
            access.clone(),
            authenticator.clone(),
            ToolScope::All,
        );
        let caller = source(
            subject,
            owner,
            SystemTime::now() + Duration::from_secs(30),
            ToolScope::All,
        )
        .with_auth_epoch(0);
        authenticator.0.store(1, Ordering::SeqCst);
        assert!(matches!(
            service
                .issue(&caller, owner, command.clone(), Role::editor())
                .await,
            Err(DelegatedAuthorityError::AuthEpochRevoked)
        ));
        assert_eq!(store.inserts.load(Ordering::SeqCst), 0);

        authenticator.0.store(0, Ordering::SeqCst);
        *access.returned_subject.lock().expect("subject mutex") = UserId::new(Uuid::now_v7());
        assert!(matches!(
            service.issue(&caller, owner, command, Role::editor()).await,
            Err(DelegatedAuthorityError::Access(AccessError::Resolution(_)))
        ));
        assert_eq!(store.inserts.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn redemption_rechecks_owner_id_profile_registry_and_role() {
        let (subject, owner, store, access, authenticator) = fixtures();
        let (_engine, service, command) = runtime(
            registry(true),
            store.clone(),
            access.clone(),
            authenticator.clone(),
            ToolScope::All,
        );
        let caller = source(
            subject,
            owner,
            SystemTime::now() + Duration::from_secs(30),
            ToolScope::All,
        );
        let issued = service
            .issue(&caller, owner, command.clone(), Role::editor())
            .await
            .expect("issue");
        let foreign_owner = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
        assert!(matches!(
            service
                .redeem_phase(issued.id, foreign_owner, &command)
                .await,
            Err(DelegatedAuthorityError::NotFound)
        ));
        assert!(matches!(
            service
                .redeem_phase(DelegationId::from_uuid(Uuid::now_v7()), owner, &command)
                .await,
            Err(DelegatedAuthorityError::NotFound)
        ));

        let (_contracted_engine, contracted, _) = runtime(
            registry(true),
            store.clone(),
            access.clone(),
            authenticator.clone(),
            ToolScope::Palette(Vec::new()),
        );
        assert!(matches!(
            contracted.redeem_phase(issued.id, owner, &command).await,
            Err(DelegatedAuthorityError::ToolScopeDenied(_))
        ));

        let (no_tool_engine, _system, no_tool_runtime) =
            crate::Engine::new(registry(false)).into_runtime_authorities();
        let no_tool_service = DelegatedAuthorityService::new(
            store,
            access.clone(),
            authenticator,
            Arc::new(no_tool_engine.registry().clone()),
            ToolScope::All,
            &no_tool_runtime,
        );
        assert!(matches!(
            no_tool_service
                .redeem_phase(issued.id, owner, &command)
                .await,
            Err(DelegatedAuthorityError::CommandUnavailable(_))
        ));

        *access.role.lock().expect("role mutex") = Some(Role::ingest());
        assert!(matches!(
            service.redeem_phase(issued.id, owner, &command).await,
            Err(DelegatedAuthorityError::RoleCeilingNoLongerHeld)
        ));
        *access.role.lock().expect("role mutex") = None;
        assert!(matches!(
            service.redeem_phase(issued.id, owner, &command).await,
            Err(DelegatedAuthorityError::MembershipRevoked)
        ));
    }

    #[tokio::test]
    async fn expired_bearer_cannot_revoke_or_touch_store() {
        let (subject, owner, store, access, authenticator) = fixtures();
        let (_engine, service, _command) = runtime(
            registry(true),
            store.clone(),
            access,
            authenticator,
            ToolScope::All,
        );
        let expired = source(
            subject,
            owner,
            SystemTime::now() - Duration::from_secs(1),
            ToolScope::All,
        );
        assert!(matches!(
            service
                .revoke(&expired, DelegationId::from_uuid(Uuid::now_v7()), owner,)
                .await,
            Err(DelegatedAuthorityError::Expired)
        ));
        assert_eq!(store.loads.load(Ordering::SeqCst), 0);
        assert_eq!(store.revokes.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn issue_rejects_non_host_authority_before_store_access() {
        let (subject, owner, store, access, authenticator) = fixtures();
        let (_engine, service, command) = runtime(
            registry(true),
            store.clone(),
            access,
            authenticator,
            ToolScope::All,
        );
        let wake =
            AuthzContext::for_subject_with_role(subject, [(owner, Role::editor())], AuthPath::Wake)
                .with_expires_at(Some(SystemTime::now() + Duration::from_secs(30)))
                .with_tool_scope(ToolScope::All);

        assert!(matches!(
            service.issue(&wake, owner, command, Role::editor()).await,
            Err(DelegatedAuthorityError::HostBearerRequired)
        ));
        assert_eq!(store.inserts.load(Ordering::SeqCst), 0);
        assert_eq!(store.loads.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn dispatcher_command_accepts_whole_or_exact_leaf_scope_only() {
        let (subject, owner, store, access, authenticator) = fixtures();
        let frozen = dispatcher_registry();
        let (engine, _system, runtime_authority) =
            crate::Engine::new(frozen).into_runtime_authorities();
        let registry = Arc::new(engine.registry().clone());
        let command =
            DelegatedCommand::parse(format!("{DISPATCHER_TOOL_NAME}:run"), registry.as_ref())
                .expect("registered dispatcher action");

        for scope in [
            ToolScope::Palette(vec![DISPATCHER_TOOL_NAME.into()]),
            ToolScope::Palette(vec![format!("{DISPATCHER_TOOL_NAME}:run")]),
        ] {
            let service = DelegatedAuthorityService::new(
                store.clone(),
                access.clone(),
                authenticator.clone(),
                registry.clone(),
                scope.clone(),
                &runtime_authority,
            );
            let caller = source(
                subject,
                owner,
                SystemTime::now() + Duration::from_secs(30),
                scope,
            );
            let issued = service
                .issue(&caller, owner, command.clone(), Role::editor())
                .await
                .expect("whole dispatcher and exact leaf palettes permit issuance");
            let _phase = service
                .redeem_phase(issued.id, owner, &command)
                .await
                .expect("current dispatcher scope permits redemption");
        }

        assert!(
            DelegatedCommand::parse(format!("{DISPATCHER_TOOL_NAME}:bogus"), registry.as_ref(),)
                .is_err()
        );
        let service = DelegatedAuthorityService::new(
            store,
            access,
            authenticator,
            registry,
            ToolScope::All,
            &runtime_authority,
        );
        let other_action = source(
            subject,
            owner,
            SystemTime::now() + Duration::from_secs(30),
            ToolScope::Palette(vec![format!("{DISPATCHER_TOOL_NAME}:other")]),
        );
        assert!(matches!(
            service
                .issue(&other_action, owner, command, Role::editor())
                .await,
            Err(DelegatedAuthorityError::ToolScopeDenied(_))
        ));
    }
}

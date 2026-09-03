//! Edge-auth contracts: WHO (`Identity`) vs WHAT (`CapabilitySet`).
//!
//! Transports authenticate once at the edge via a host-provided
//! [`Authenticator`]; the engine performs only pure checks against the
//! resulting [`AuthzContext`]. Companion to `auth.rs`, which now only
//! carries credential material and auth failures.

pub mod hooks;

use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use futures_util::Stream;
use tokio::time::{Instant, Interval, Sleep};

use crate::access::{AccessKind, OwnerRoles};
use crate::auth::{AuthError, Credentials};
use crate::error::ProtocolError;
use crate::{Owner, OwnerRef, UserId};

pub use hooks::{
    AuthorizationHook, AuthzInput, AuthzOperation, AuthzOutcome, AuthzVeto, MembershipChange,
    OwnerResolver,
};

/// WHO: the authorization currency for owner scoping.
#[derive(Debug, Clone, PartialEq)]
pub struct Identity {
    subject: Option<UserId>,
    principal: OwnerRef,
    accessible_principals: HashSet<OwnerRef>,
    /// Streams terminate past this; `None` = no expiry.
    expires_at: Option<SystemTime>,
    /// Revocation generation; hosts bump it to force re-auth.
    auth_epoch: u64,
    /// Model/runner identity the authenticator bound to this principal.
    ///
    /// Authenticated provenance, on the same footing as `subject`: it
    /// travels with the identity through narrowing and revalidation, and no
    /// transport payload can set it. `None` means the deployment binds no
    /// model identity to this principal.
    trusted_model_id: Option<String>,
}

impl Identity {
    #[must_use]
    pub fn can_access_principal(&self, principal: &OwnerRef) -> bool {
        self.accessible_principals.contains(principal)
    }
}

/// Rejected input to [`AuthzContext::with_trusted_model_id`].
///
/// A configuration fault in the authenticator, never a caller fault: the
/// value it names came from a credential mapping the deployment wrote.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TrustedModelIdError {
    #[error("trusted model id must not be blank")]
    Blank,
    #[error("trusted model id must be at most {max} characters, got {chars}")]
    TooLong { chars: usize, max: usize },
}

/// WHAT: tool palette + access scope, separate from identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolScope {
    All,
    Palette(Vec<String>),
}

impl ToolScope {
    #[must_use]
    pub fn allows(&self, name: &str) -> bool {
        match self {
            Self::All => true,
            Self::Palette(allowed) => allowed.iter().any(|tool| tool == name),
        }
    }

    #[must_use]
    pub fn allows_action(&self, tool: &str, action: &str) -> bool {
        self.allows(&format!("{tool}:{action}"))
    }

    #[must_use]
    pub fn allows_group_advertisement(&self, tool: &str) -> bool {
        match self {
            Self::All => true,
            Self::Palette(allowed) => {
                let action_prefix = format!("{tool}:");
                allowed
                    .iter()
                    .any(|entry| entry == tool || entry.starts_with(&action_prefix))
            }
        }
    }

    /// Whether a descriptor may be advertised from this palette.
    ///
    /// Flat tools require their exact id. Dispatchers may also be visible
    /// through one admitted `tool:action` leaf. Treating a flat tool as a
    /// dispatcher would let a bogus `flat:unknown` entry advertise a tool
    /// that the invocation gate correctly denies.
    #[must_use]
    pub fn allows_tool_advertisement(&self, tool: &str, has_actions: bool) -> bool {
        if has_actions {
            self.allows_group_advertisement(tool)
        } else {
            self.allows(tool)
        }
    }

    /// Narrow `self` by `other`, never widening it. Used to layer a
    /// deployment-wide surface over a caller's own scope: `All` is the
    /// identity element, and two palettes intersect to the ids both allow.
    /// A deployment palette applied to an `All` caller yields the palette;
    /// applied to an already-narrower caller it keeps only the common ids.
    #[must_use]
    pub fn intersect(&self, other: &ToolScope) -> ToolScope {
        match (self, other) {
            (Self::All, scope) | (scope, Self::All) => scope.clone(),
            (Self::Palette(a), Self::Palette(b)) => {
                let permitted: std::collections::HashSet<&str> =
                    b.iter().map(String::as_str).collect();
                Self::Palette(
                    a.iter()
                        .filter(|id| permitted.contains(id.as_str()))
                        .cloned()
                        .collect(),
                )
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CapabilitySet {
    pub tool_scope: ToolScope,
}

impl CapabilitySet {
    #[must_use]
    pub fn all() -> Self {
        Self {
            tool_scope: ToolScope::All,
        }
    }
}

/// Provenance of an authenticated context — recorded for audit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthPath {
    HostBearer,
    /// A durable, narrowly scoped grant redeemed by a background worker.
    Delegated,
    Wake,
    System,
    /// Fail-closed sentinel for a context that carries no real
    /// credentials (see [`AuthzContext::denied_for_owner`]).
    Denied,
}

/// Runtime-held witness for issuing owner-write permits from
/// [`AuthPath::System`] contexts.
///
/// Public `AuthzContext` constructors intentionally remain available for
/// tests and trusted host adapters. Permit issuance is the containment line:
/// a caller-shaped `System` context is not enough without this witness.
#[derive(Debug)]
pub struct SystemAuthority {
    binding: SystemAuthorityBinding,
}

impl SystemAuthority {
    #[must_use]
    pub(crate) const fn new(binding: SystemAuthorityBinding) -> Self {
        Self { binding }
    }

    /// Opaque boot-instance binding for backend services assembled with this
    /// engine. The identifier is not authority on its own; only the
    /// uncloneable `SystemAuthority` can validate it.
    #[doc(hidden)]
    #[must_use]
    pub fn binding(&self) -> SystemAuthorityBinding {
        self.binding.clone()
    }

    #[doc(hidden)]
    #[must_use]
    pub fn authorizes(&self, binding: &SystemAuthorityBinding) -> bool {
        self.binding == *binding
    }
}

/// Opaque identity shared by one booted engine and its host-owned backends.
#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemAuthorityBinding(uuid::Uuid);

impl SystemAuthorityBinding {
    pub(crate) fn fresh() -> Self {
        Self(uuid::Uuid::now_v7())
    }
}

/// Uncloneable boot witness used only while composing the one delegated
/// authority service and the worker-facing backend services for an Engine.
/// It is never published through `FlavorServices` or `FlavorWorkerContext`.
#[doc(hidden)]
pub struct DelegationRuntimeAuthority {
    binding: DelegationRuntimeBinding,
}

impl std::fmt::Debug for DelegationRuntimeAuthority {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DelegationRuntimeAuthority")
            .finish_non_exhaustive()
    }
}

impl DelegationRuntimeAuthority {
    pub(crate) const fn new(binding: DelegationRuntimeBinding) -> Self {
        Self { binding }
    }

    pub(crate) fn binding(&self) -> DelegationRuntimeBinding {
        self.binding.clone()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DelegationRuntimeBinding(uuid::Uuid);

impl DelegationRuntimeBinding {
    pub(crate) fn fresh() -> Self {
        Self(uuid::Uuid::now_v7())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AuthzContext {
    identity: Identity,
    capabilities: CapabilitySet,
    auth_path: AuthPath,
    owner_roles: Option<OwnerRoles>,
}

/// Opaque authority for one redeemed durable-worker phase.
///
/// The phase is intentionally neither cloneable nor convertible to
/// [`AuthzContext`]. Linked worker implementations are trusted in-process,
/// but every delegated-capable Engine operation rechecks the finite bearer
/// deadline before extracting the exact-owner, role-ceiling context.
///
/// ```compile_fail
/// # use proxima_core::DelegatedPhase;
/// fn duplicate(phase: DelegatedPhase) {
///     let _copy = phase.clone();
/// }
/// ```
///
/// ```compile_fail
/// # use proxima_core::{AuthzContext, DelegatedPhase};
/// fn extract(phase: &DelegatedPhase) -> &AuthzContext {
///     phase.authz()
/// }
/// ```
#[must_use]
pub struct DelegatedPhase {
    authz: AuthzContext,
    expires_at: SystemTime,
    runtime_binding: DelegationRuntimeBinding,
}

impl DelegatedPhase {
    pub(crate) fn new(
        authz: AuthzContext,
        expires_at: SystemTime,
        runtime_binding: DelegationRuntimeBinding,
    ) -> Self {
        debug_assert_eq!(authz.auth_path(), AuthPath::Delegated);
        debug_assert_eq!(authz.expires_at(), Some(expires_at));
        Self {
            authz,
            expires_at,
            runtime_binding,
        }
    }

    #[cfg(test)]
    pub(crate) fn expire_for_test(&mut self) {
        self.expires_at = SystemTime::UNIX_EPOCH;
    }
}

impl std::fmt::Debug for DelegatedPhase {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DelegatedPhase")
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

mod engine_authority_seal {
    use super::{
        AuthPath, AuthzContext, DelegatedPhase, EngineOperationAuthority, ProtocolError, SystemTime,
    };

    pub(crate) trait Sealed {
        fn context_for_engine_operation(
            &self,
        ) -> Result<EngineOperationAuthority<'_>, ProtocolError>;
    }

    impl Sealed for AuthzContext {
        fn context_for_engine_operation(
            &self,
        ) -> Result<EngineOperationAuthority<'_>, ProtocolError> {
            if self.auth_path() == AuthPath::Delegated {
                return Err(ProtocolError::forbidden(
                    "raw delegated authorization contexts are not Engine authority",
                ));
            }
            Ok(EngineOperationAuthority {
                authz: self,
                redeemed_phase: false,
                runtime_binding: None,
            })
        }
    }

    impl Sealed for DelegatedPhase {
        fn context_for_engine_operation(
            &self,
        ) -> Result<EngineOperationAuthority<'_>, ProtocolError> {
            if self.expires_at <= SystemTime::now() {
                return Err(ProtocolError::forbidden(
                    "delegated worker phase has expired",
                ));
            }
            Ok(EngineOperationAuthority {
                authz: &self.authz,
                redeemed_phase: true,
                runtime_binding: Some(&self.runtime_binding),
            })
        }
    }
}

/// Sealed authority accepted by delegated-capable Engine operations.
///
/// Ordinary callers continue to pass [`AuthzContext`]. Durable workers pass
/// an opaque [`DelegatedPhase`] returned by
/// [`crate::DelegatedAuthorityService::redeem_phase`]. External crates cannot
/// implement this trait or extract its context.
#[allow(private_bounds)]
pub trait EngineAuthority: engine_authority_seal::Sealed {}

impl EngineAuthority for AuthzContext {}
impl EngineAuthority for DelegatedPhase {}

pub(crate) struct EngineOperationAuthority<'a> {
    authz: &'a AuthzContext,
    redeemed_phase: bool,
    runtime_binding: Option<&'a DelegationRuntimeBinding>,
}

impl<'a> EngineOperationAuthority<'a> {
    #[must_use]
    pub(crate) const fn authz(&self) -> &'a AuthzContext {
        self.authz
    }

    #[must_use]
    pub(crate) const fn redeemed_phase(&self) -> bool {
        self.redeemed_phase
    }

    pub(crate) fn validate_runtime_binding(
        &self,
        expected: Option<&DelegationRuntimeBinding>,
    ) -> Result<(), ProtocolError> {
        match (self.runtime_binding, expected) {
            (None, _) => Ok(()),
            (Some(actual), Some(expected)) if actual == expected => Ok(()),
            (Some(_), Some(_) | None) => Err(ProtocolError::forbidden(
                "delegated worker phase belongs to a different runtime",
            )),
        }
    }
}

pub(crate) fn context_for_engine_operation<A>(
    authority: &A,
) -> Result<EngineOperationAuthority<'_>, ProtocolError>
where
    A: EngineAuthority + ?Sized,
{
    engine_authority_seal::Sealed::context_for_engine_operation(authority)
}

impl AuthzContext {
    #[must_use]
    pub fn subject(&self) -> Option<UserId> {
        self.identity.subject
    }

    #[must_use]
    pub const fn auth_path(&self) -> AuthPath {
        self.auth_path
    }

    #[must_use]
    pub fn principal(&self) -> OwnerRef {
        self.identity.principal
    }

    #[must_use]
    pub fn expires_at(&self) -> Option<SystemTime> {
        self.identity.expires_at
    }

    #[must_use]
    pub const fn auth_epoch(&self) -> u64 {
        self.identity.auth_epoch
    }

    /// Model/runner identity certified by the authenticating token, if the
    /// deployment binds one to this principal.
    ///
    /// This is the only trustworthy answer to "which model is calling".
    /// Flavors may build policy on it; the caller-supplied `model_id` label
    /// is a claim, not a credential.
    #[must_use]
    pub fn trusted_model_id(&self) -> Option<&str> {
        self.identity.trusted_model_id.as_deref()
    }

    #[must_use]
    pub fn identity_for_revalidation(&self) -> Identity {
        self.identity.clone()
    }

    #[must_use]
    pub fn tool_scope(&self) -> &ToolScope {
        &self.capabilities.tool_scope
    }

    #[must_use]
    pub fn can_access_owner(&self, owner: &OwnerRef) -> bool {
        self.identity.can_access_principal(owner)
    }

    pub(crate) fn accessible_owners(&self) -> impl Iterator<Item = OwnerRef> + '_ {
        self.identity.accessible_principals.iter().copied()
    }

    #[must_use]
    pub(crate) fn is_server_resolved(&self) -> bool {
        self.owner_roles.is_some()
    }

    #[must_use]
    pub fn scoped_owner(&self, owner: OwnerRef) -> Owner {
        owner
    }

    #[must_use]
    pub fn may_read(&self, owner: &OwnerRef, kind: AccessKind) -> bool {
        self.owner_roles
            .as_ref()
            .is_some_and(|roles| roles.may_read(owner, kind))
    }

    #[must_use]
    pub fn may_write(&self, owner: &OwnerRef, kind: AccessKind) -> bool {
        self.owner_roles
            .as_ref()
            .is_some_and(|roles| roles.may_write(owner, kind))
    }

    #[must_use]
    pub fn may_manage(&self, owner: &OwnerRef) -> bool {
        self.owner_roles
            .as_ref()
            .is_some_and(|roles| roles.may_manage(owner))
    }

    /// Server-resolved role for one owner, if the authenticated subject has
    /// one. Durable delegation records this role only as an upper bound and
    /// resolves it again at every redemption.
    #[must_use]
    pub fn role_for_owner(&self, owner: &OwnerRef) -> Option<crate::access::Role> {
        self.owner_roles.as_ref()?.role_for(owner)
    }

    #[must_use]
    pub fn readable_owners(&self, kind: AccessKind) -> Vec<OwnerRef> {
        self.owner_roles
            .as_ref()
            .map_or_else(Vec::new, |roles| roles.readable_owners(kind))
    }

    #[must_use]
    pub fn writable_owners(&self, kind: AccessKind) -> Vec<OwnerRef> {
        self.owner_roles
            .as_ref()
            .map_or_else(Vec::new, |roles| roles.writable_owners(kind))
    }

    #[must_use]
    pub fn server_resolved(owner_roles: OwnerRoles, auth_path: AuthPath) -> Self {
        let subject = owner_roles.subject();
        let accessible_principals = owner_roles
            .readable_owners(AccessKind::Goal)
            .into_iter()
            .collect();
        Self {
            identity: Identity {
                subject: Some(subject),
                principal: OwnerRef::Personal(subject),
                accessible_principals,
                expires_at: None,
                auth_epoch: 0,
                trusted_model_id: None,
            },
            capabilities: CapabilitySet::all(),
            auth_path,
            owner_roles: Some(owner_roles),
        }
    }

    #[must_use]
    pub fn with_tool_scope(mut self, tool_scope: ToolScope) -> Self {
        self.capabilities.tool_scope = tool_scope;
        self
    }

    /// Bind the model/runner identity this credential certifies.
    ///
    /// **For authenticators only.** The value must come from the credential
    /// itself — an OIDC subject binding, or the equivalent in a custom host
    /// — never from a tool argument, request header, MCP `clientInfo`, or
    /// any other caller-controlled payload. A transport that lets a caller
    /// reach this builder has published a forgeable provenance field.
    ///
    /// Takes a `String`, deliberately not an `Option`: there is no "clear
    /// it" call, so no later step in a builder chain can quietly drop
    /// provenance an authenticator already established. The value is
    /// trimmed here, once, so the stored label, the conflict comparison and
    /// any idempotency key derived from it are the same string.
    ///
    /// Bounded by [`MAX_OPERATOR_LABEL_CHARS`](crate::MAX_OPERATOR_LABEL_CHARS) at the point of binding
    /// rather than at the point of use: an id a tool would later refuse as
    /// over-long is a deployment that authenticates and cannot write, and
    /// the refusal would surface on a write instead of at the boundary that
    /// produced it.
    ///
    /// # Errors
    ///
    /// [`TrustedModelIdError`] when the value is blank after trimming or
    /// longer than [`MAX_OPERATOR_LABEL_CHARS`](crate::MAX_OPERATOR_LABEL_CHARS).
    pub fn with_trusted_model_id(
        mut self,
        trusted_model_id: impl Into<String>,
    ) -> Result<Self, TrustedModelIdError> {
        self.identity.trusted_model_id =
            Some(crate::tool::validate_trusted_model_id(trusted_model_id)?);
        Ok(self)
    }

    #[must_use]
    pub fn narrowed_to_owner(mut self, owner: OwnerRef) -> Option<Self> {
        let roles = self.owner_roles.as_ref()?;
        let subject = roles.subject();
        let narrowed_roles = match owner {
            OwnerRef::Personal(user) if user == subject => {
                OwnerRoles::scoped_to(subject, owner, crate::access::Role::personal())
            }
            OwnerRef::Group(_) => {
                let role = roles.role_for(&owner)?;
                OwnerRoles::scoped_to(subject, owner, role)
            }
            OwnerRef::Personal(_) => return None,
        };
        let accessible_principals = narrowed_roles
            .readable_owners(AccessKind::Goal)
            .into_iter()
            .collect();
        self.owner_roles = Some(narrowed_roles);
        self.identity.accessible_principals = accessible_principals;
        Some(self)
    }

    #[must_use]
    pub fn with_expires_at(mut self, expires_at: Option<SystemTime>) -> Self {
        self.identity.expires_at = expires_at;
        self
    }

    #[must_use]
    pub fn with_auth_epoch(mut self, auth_epoch: u64) -> Self {
        self.identity.auth_epoch = auth_epoch;
        self
    }

    /// # Panics
    ///
    /// Panics only if constructing the empty subject role set fails.
    #[must_use]
    pub fn for_subject(subject: UserId, auth_path: AuthPath) -> Self {
        Self::server_resolved(OwnerRoles::for_subject(subject, []).unwrap(), auth_path)
    }

    /// # Panics
    ///
    /// Panics if `roles` contains invalid owner-role overrides.
    #[must_use]
    pub fn for_subject_with_role<I>(subject: UserId, roles: I, auth_path: AuthPath) -> Self
    where
        I: IntoIterator<Item = (OwnerRef, crate::access::Role)>,
    {
        Self::server_resolved(OwnerRoles::for_subject(subject, roles).unwrap(), auth_path)
    }

    /// Compatibility helper for already-owner-scoped trusted surfaces. It is
    /// server-resolved for personal owners; group access must be represented by
    /// an explicit subject role and cannot be minted from a bare group owner.
    #[must_use]
    pub fn single_owner(owner: &Owner, auth_path: AuthPath) -> Self {
        match *owner {
            OwnerRef::Personal(subject) => Self::for_subject(subject, auth_path),
            OwnerRef::Group(_) => Self::denied_for_owner(owner),
        }
    }

    /// Fail-closed, zero-capability context. Carries `owner`'s identity for
    /// audit but grants no roles, an empty tool palette, and no accessible
    /// principals — every capability check and every owner-scope check
    /// denies.
    ///
    /// There is no owner-less `denied()`: a denied context still names the
    /// principal the denial was about, and no owner kind exists to stand in
    /// as a placeholder.
    #[must_use]
    pub fn denied_for_owner(owner: &Owner) -> Self {
        Self {
            identity: Identity {
                subject: None,
                principal: *owner,
                accessible_principals: HashSet::new(),
                expires_at: None,
                auth_epoch: 0,
                trusted_model_id: None,
            },
            capabilities: CapabilitySet {
                tool_scope: ToolScope::Palette(Vec::new()),
            },
            auth_path: AuthPath::Denied,
            owner_roles: None,
        }
    }
}

/// The host contract: validate credential material, return an
/// authenticated context.
#[async_trait]
pub trait Authenticator: Send + Sync {
    /// # Errors
    ///
    /// `AuthError::AuthRequired` when credentials are missing,
    /// `AuthError::InvalidCredentials` when they are malformed,
    /// expired, or revoked.
    async fn authenticate(&self, creds: &Credentials) -> Result<AuthzContext, AuthError>;

    /// Current revocation epoch for a principal. A host bumps the returned
    /// value to force active streams authenticated at a lower epoch to
    /// terminate. The default never revokes.
    async fn current_auth_epoch(&self, _principal: &OwnerRef) -> u64 {
        0
    }
}

/// Revalidation cadence for long-lived authenticated streams.
///
/// Streams terminate silently at `Identity::expires_at` when present, after
/// `max_stream_lifetime` regardless of identity state, or when the host
/// authenticator reports `current_auth_epoch(principal) > identity.auth_epoch`.
/// Epoch checks run every `epoch_check_interval`; an absent authenticator means
/// only expiry and max-lifetime deadlines apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RevalidationConfig {
    pub max_stream_lifetime: Duration,
    pub epoch_check_interval: Duration,
}

impl Default for RevalidationConfig {
    fn default() -> Self {
        Self {
            max_stream_lifetime: Duration::from_hours(1),
            epoch_check_interval: Duration::from_secs(30),
        }
    }
}

type EpochCheck = Pin<Box<dyn Future<Output = u64> + Send>>;

struct RevalidatedStream<I> {
    inner: Pin<Box<dyn Stream<Item = I> + Send>>,
    identity: Identity,
    authenticator: Option<Arc<dyn Authenticator>>,
    expires_at: Option<Pin<Box<Sleep>>>,
    max_lifetime: Pin<Box<Sleep>>,
    epoch_interval: Option<Interval>,
    epoch_check: Option<EpochCheck>,
    closing: bool,
    closed: bool,
}

impl<I> RevalidatedStream<I> {
    fn observe_termination(&mut self, cx: &mut Context<'_>) -> bool {
        if self.max_lifetime.as_mut().poll(cx).is_ready() {
            return true;
        }

        if self
            .expires_at
            .as_mut()
            .is_some_and(|expires_at| expires_at.as_mut().poll(cx).is_ready())
        {
            return true;
        }

        if let Some(interval) = self.epoch_interval.as_mut()
            && interval.poll_tick(cx).is_ready()
            && self.epoch_check.is_none()
            && let Some(authenticator) = self.authenticator.clone()
        {
            let principal = self.identity.principal;
            self.epoch_check = Some(Box::pin(async move {
                authenticator.current_auth_epoch(&principal).await
            }));
        }

        if let Some(check) = self.epoch_check.as_mut()
            && let Poll::Ready(epoch) = check.as_mut().poll(cx)
        {
            self.epoch_check = None;
            return epoch > self.identity.auth_epoch;
        }

        false
    }

    fn poll_inner(&mut self, cx: &mut Context<'_>) -> Poll<Option<I>> {
        self.inner.as_mut().poll_next(cx)
    }
}

impl<I> Stream for RevalidatedStream<I> {
    type Item = I;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.closed {
            return Poll::Ready(None);
        }

        if self.closing {
            return match self.poll_inner(cx) {
                Poll::Ready(Some(item)) => Poll::Ready(Some(item)),
                Poll::Ready(None) | Poll::Pending => {
                    self.closed = true;
                    Poll::Ready(None)
                }
            };
        }

        match self.poll_inner(cx) {
            Poll::Ready(Some(item)) => {
                self.closing = self.observe_termination(cx);
                Poll::Ready(Some(item))
            }
            Poll::Ready(None) => {
                self.closed = true;
                Poll::Ready(None)
            }
            Poll::Pending => {
                if self.observe_termination(cx) {
                    self.closed = true;
                    Poll::Ready(None)
                } else {
                    Poll::Pending
                }
            }
        }
    }
}

/// Wrap a long-lived stream with expiry, revocation-epoch, and max-lifetime
/// termination. Termination is a silent stream end.
#[must_use]
pub fn revalidate_stream<S>(
    stream: S,
    identity: Identity,
    authenticator: Option<Arc<dyn Authenticator>>,
    config: RevalidationConfig,
) -> Pin<Box<dyn Stream<Item = S::Item> + Send>>
where
    S: Stream + Send + 'static,
    S::Item: Send + 'static,
{
    let expires_at = identity.expires_at.map(system_time_sleep);
    let max_lifetime = Box::pin(tokio::time::sleep(config.max_stream_lifetime));
    let epoch_interval = authenticator.as_ref().map(|_| {
        let interval = non_zero_duration(config.epoch_check_interval);
        tokio::time::interval_at(Instant::now() + interval, interval)
    });

    Box::pin(RevalidatedStream {
        inner: Box::pin(stream),
        identity,
        authenticator,
        expires_at,
        max_lifetime,
        epoch_interval,
        epoch_check: None,
        closing: false,
        closed: false,
    })
}

fn system_time_sleep(deadline: SystemTime) -> Pin<Box<Sleep>> {
    let until_deadline = deadline
        .duration_since(SystemTime::now())
        .unwrap_or(Duration::ZERO);
    Box::pin(tokio::time::sleep_until(Instant::now() + until_deadline))
}

const fn non_zero_duration(duration: Duration) -> Duration {
    if duration.is_zero() {
        Duration::from_nanos(1)
    } else {
        duration
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MAX_OPERATOR_LABEL_CHARS;
    use crate::access::Role;
    use crate::protocol::tool as protocol_tool;
    use crate::protocol::{action as protocol_action, resource as protocol_resource};
    use crate::{GroupId, UserId};
    use futures_util::StreamExt;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tokio::sync::mpsc;
    use tokio::time;

    fn owner() -> Owner {
        OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()))
    }

    fn identity(expires_at: Option<SystemTime>, auth_epoch: u64) -> Identity {
        let principal = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
        let mut accessible_principals = HashSet::with_capacity(1);
        accessible_principals.insert(principal);
        Identity {
            subject: match principal {
                OwnerRef::Personal(user) => Some(user),
                OwnerRef::Group(_) => None,
            },
            principal,
            accessible_principals,
            expires_at,
            auth_epoch,
            trusted_model_id: None,
        }
    }

    fn receiver_stream<T: Send + 'static>(
        receiver: mpsc::UnboundedReceiver<T>,
    ) -> impl Stream<Item = T> + Send {
        futures_util::stream::unfold(receiver, |mut receiver| async {
            receiver.recv().await.map(|item| (item, receiver))
        })
    }

    #[derive(Debug, Default)]
    struct EpochAuthenticator {
        epoch: AtomicU64,
    }

    #[async_trait]
    impl Authenticator for EpochAuthenticator {
        async fn authenticate(&self, _creds: &Credentials) -> Result<AuthzContext, AuthError> {
            Err(AuthError::AuthRequired)
        }

        async fn current_auth_epoch(&self, _principal: &OwnerRef) -> u64 {
            self.epoch.load(Ordering::SeqCst)
        }
    }

    #[derive(Debug)]
    struct DefaultEpochAuthenticator;

    #[async_trait]
    impl Authenticator for DefaultEpochAuthenticator {
        async fn authenticate(&self, _creds: &Credentials) -> Result<AuthzContext, AuthError> {
            Err(AuthError::AuthRequired)
        }
    }

    #[test]
    fn single_owner_context_is_server_resolved_and_owner_accessible() {
        let o = owner();
        let ctx = AuthzContext::single_owner(&o, AuthPath::System);

        assert_eq!(ctx.auth_path, AuthPath::System);
        assert!(ctx.is_server_resolved());
        assert!(ctx.may_write(&o, AccessKind::Fact));
        assert!(ctx.identity.can_access_principal(&o));
        assert!(ctx.identity.expires_at.is_none());
    }

    #[test]
    fn denied_context_has_denied_path_and_no_accessible_principals() {
        let o = owner();
        let ctx = AuthzContext::denied_for_owner(&o);

        assert_eq!(ctx.auth_path, AuthPath::Denied);
        assert!(ctx.identity.accessible_principals.is_empty());
        assert!(!ctx.identity.can_access_principal(&o));
        assert!(
            !ctx.capabilities
                .tool_scope
                .allows(protocol_resource::MEMORY)
        );
    }

    #[test]
    fn unauthenticated_denied_context_grants_no_owner_access() {
        let owner = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
        let ctx = AuthzContext::denied_for_owner(&owner);

        assert_eq!(ctx.auth_path(), AuthPath::Denied);
        assert_eq!(ctx.subject(), None);
        assert!(!ctx.may_read(&owner, AccessKind::Fact));
        assert!(!ctx.may_write(&owner, AccessKind::Fact));
        assert!(!ctx.may_manage(&owner));
        assert!(ctx.readable_owners(AccessKind::Goal).is_empty());
        assert!(ctx.writable_owners(AccessKind::Goal).is_empty());
    }

    #[test]
    fn resolved_subject_context_gets_role_ceilings() {
        let subject = UserId::new(uuid::Uuid::now_v7());
        let group = OwnerRef::Group(GroupId::new(uuid::Uuid::now_v7()));
        let roles = OwnerRoles::for_subject(subject, [(group, Role::editor())]).unwrap();

        let ctx = AuthzContext::server_resolved(roles, AuthPath::HostBearer);

        assert_eq!(ctx.subject(), Some(subject));
        assert!(ctx.may_write(&OwnerRef::Personal(subject), AccessKind::Goal));
        assert!(ctx.may_write(&group, AccessKind::Perspective));
        assert!(!ctx.may_write(&group, AccessKind::Goal));
        assert!(!ctx.may_manage(&group));

        let readable = ctx.readable_owners(AccessKind::Goal);
        assert_eq!(readable.len(), 2, "own personal owner plus the one group");
        assert!(readable.contains(&OwnerRef::Personal(subject)));
        assert!(readable.contains(&group));

        let writable = ctx.writable_owners(AccessKind::Goal);
        assert!(writable.contains(&OwnerRef::Personal(subject)));
        assert!(!writable.contains(&group));
    }

    /// `trusted_model_id` is identity, not a capability: it must survive
    /// every narrowing the edge performs between authentication and the
    /// tool call, exactly like `subject`.
    #[test]
    fn a_trusted_model_id_survives_owner_narrowing_and_revalidation() {
        let subject = UserId::new(uuid::Uuid::now_v7());
        let group = OwnerRef::Group(GroupId::new(uuid::Uuid::now_v7()));
        let roles = OwnerRoles::for_subject(subject, [(group, Role::editor())]).unwrap();

        let ctx = AuthzContext::server_resolved(roles, AuthPath::HostBearer)
            .with_trusted_model_id("runner/pinned")
            .expect("a well-formed runner id binds");

        assert_eq!(ctx.trusted_model_id(), Some("runner/pinned"));
        assert_eq!(
            ctx.identity_for_revalidation().trusted_model_id.as_deref(),
            Some("runner/pinned"),
            "a revalidated stream keeps the bound model identity"
        );

        let narrowed = ctx
            .clone()
            .narrowed_to_owner(group)
            .expect("editor narrows to the group owner");
        assert_eq!(narrowed.trusted_model_id(), Some("runner/pinned"));
        assert_eq!(narrowed.subject(), Some(subject));

        let personal = ctx
            .narrowed_to_owner(OwnerRef::Personal(subject))
            .expect("own personal owner narrows");
        assert_eq!(personal.trusted_model_id(), Some("runner/pinned"));
    }

    /// Nothing binds a model identity unless an authenticator says so: the
    /// test constructors and the fail-closed context all leave it absent.
    #[test]
    fn contexts_carry_no_trusted_model_id_by_default() {
        let owner = owner();
        assert_eq!(
            AuthzContext::single_owner(&owner, AuthPath::HostBearer).trusted_model_id(),
            None
        );
        assert_eq!(
            AuthzContext::for_subject(UserId::new(uuid::Uuid::now_v7()), AuthPath::HostBearer)
                .trusted_model_id(),
            None
        );
        assert_eq!(
            AuthzContext::denied_for_owner(&owner).trusted_model_id(),
            None
        );
    }

    /// The bound is applied where the value is bound, not where it is
    /// later used: an authenticator that binds an unusable id learns at the
    /// boundary that produced it, not on some later write.
    #[test]
    fn a_bound_trusted_model_id_is_trimmed_and_bounded() {
        let owner = owner();
        let base = || AuthzContext::single_owner(&owner, AuthPath::HostBearer);

        assert_eq!(
            base()
                .with_trusted_model_id("  runner/pinned  ")
                .expect("trims")
                .trusted_model_id(),
            Some("runner/pinned"),
            "one trim, at the boundary, so every later comparison is on the same string"
        );

        for blank in ["", "   "] {
            assert_eq!(
                base().with_trusted_model_id(blank).unwrap_err(),
                TrustedModelIdError::Blank
            );
        }

        assert!(
            base()
                .with_trusted_model_id("m".repeat(MAX_OPERATOR_LABEL_CHARS))
                .is_ok(),
            "the bound itself fits"
        );
        assert_eq!(
            base()
                .with_trusted_model_id("m".repeat(MAX_OPERATOR_LABEL_CHARS + 1))
                .unwrap_err(),
            TrustedModelIdError::TooLong {
                chars: MAX_OPERATOR_LABEL_CHARS + 1,
                max: MAX_OPERATOR_LABEL_CHARS,
            }
        );
    }

    /// There is no way to spell "clear it": the builder takes a `String`,
    /// so no later step in a chain can drop provenance an authenticator
    /// established.
    #[test]
    fn a_bound_trusted_model_id_cannot_be_cleared_by_a_later_builder_step() {
        let owner = owner();
        let ctx = AuthzContext::single_owner(&owner, AuthPath::HostBearer)
            .with_trusted_model_id("runner/pinned")
            .expect("binds")
            .with_tool_scope(ToolScope::Palette(Vec::new()))
            .with_expires_at(Some(SystemTime::UNIX_EPOCH))
            .with_auth_epoch(7);

        assert_eq!(ctx.trusted_model_id(), Some("runner/pinned"));
    }

    #[test]
    fn palette_scope_allows_and_denies() {
        let scope = ToolScope::Palette(vec![protocol_resource::MEMORY.to_string()]);

        assert!(scope.allows(protocol_resource::MEMORY));
        assert!(!scope.allows(protocol_action::CORE_GOAL_SET));
    }

    #[test]
    fn allows_action_requires_leaf_scope_key() {
        let scope = ToolScope::Palette(vec![protocol_action::CORE_GOAL_SET.to_string()]);

        assert!(scope.allows_action(protocol_tool::CORE_GOAL, "set"));
        assert!(!scope.allows_action(protocol_tool::CORE_GOAL, "transition"));
        assert!(!scope.allows(protocol_tool::CORE_GOAL));
    }

    #[test]
    fn allows_group_advertisement_accepts_flat_or_leaf_key() {
        let leaf = ToolScope::Palette(vec![protocol_action::CORE_GOAL_SET.to_string()]);
        let flat = ToolScope::Palette(vec![protocol_tool::CORE_GOAL.to_string()]);
        let unrelated = ToolScope::Palette(vec![protocol_resource::MEMORY.to_string()]);

        assert!(ToolScope::All.allows_group_advertisement(protocol_tool::CORE_GOAL));
        assert!(leaf.allows_group_advertisement(protocol_tool::CORE_GOAL));
        assert!(flat.allows_group_advertisement(protocol_tool::CORE_GOAL));
        assert!(!unrelated.allows_group_advertisement(protocol_tool::CORE_GOAL));
    }

    #[test]
    fn flat_tool_advertisement_requires_the_exact_tool_id() {
        let exact = ToolScope::Palette(vec![protocol_tool::CORE_SEARCH_MEMORIES.to_string()]);
        let bogus_leaf = ToolScope::Palette(vec![format!(
            "{}:unknown",
            protocol_tool::CORE_SEARCH_MEMORIES
        )]);
        let dispatcher_leaf = ToolScope::Palette(vec![protocol_action::CORE_GOAL_SET.to_string()]);

        assert!(exact.allows_tool_advertisement(protocol_tool::CORE_SEARCH_MEMORIES, false));
        assert!(!bogus_leaf.allows_tool_advertisement(protocol_tool::CORE_SEARCH_MEMORIES, false));
        assert!(dispatcher_leaf.allows_tool_advertisement(protocol_tool::CORE_GOAL, true));
    }

    #[test]
    fn tool_scope_intersect_only_narrows_never_widens() {
        let palette =
            |ids: &[&str]| ToolScope::Palette(ids.iter().map(|id| (*id).to_string()).collect());
        let mem = palette(&[
            protocol_resource::MEMORY,
            protocol_tool::CORE_SEARCH_MEMORIES,
        ]);

        // `All` is the identity element in both positions.
        assert_eq!(ToolScope::All.intersect(&mem), mem);
        assert_eq!(mem.intersect(&ToolScope::All), mem);
        assert_eq!(ToolScope::All.intersect(&ToolScope::All), ToolScope::All);

        // Two palettes intersect to only the ids both allow — a deployment
        // scope can never re-add an id the caller's scope omitted.
        let caller = palette(&[protocol_resource::MEMORY, protocol_action::CORE_GOAL_SET]);
        let result = mem.intersect(&caller);
        assert!(result.allows(protocol_resource::MEMORY));
        assert!(!result.allows(protocol_tool::CORE_SEARCH_MEMORIES)); // caller lacked it
        assert!(!result.allows(protocol_action::CORE_GOAL_SET)); // deployment lacked it

        // Disjoint palettes intersect to empty (deny-all), never widening.
        assert_eq!(
            palette(&["a"]).intersect(&palette(&["b"])),
            ToolScope::Palette(Vec::new())
        );
    }

    #[tokio::test(start_paused = true)]
    async fn expired_identity_terminates_at_deadline_but_non_expired_keeps_flowing() {
        let config = RevalidationConfig {
            max_stream_lifetime: Duration::from_mins(1),
            epoch_check_interval: Duration::from_secs(5),
        };
        let now = SystemTime::now();
        let (expired_sender, expired_receiver) = mpsc::unbounded_channel();
        let (live_sender, live_receiver) = mpsc::unbounded_channel();
        let mut expired_stream = revalidate_stream(
            receiver_stream(expired_receiver),
            identity(Some(now + Duration::from_secs(10)), 0),
            None,
            config,
        );
        let mut live_stream = revalidate_stream(
            receiver_stream(live_receiver),
            identity(Some(now + Duration::from_mins(1)), 0),
            None,
            config,
        );

        expired_sender.send(1).expect("receiver is alive");
        live_sender.send(10).expect("receiver is alive");
        assert_eq!(expired_stream.next().await, Some(1));
        assert_eq!(live_stream.next().await, Some(10));

        time::advance(Duration::from_secs(10)).await;
        live_sender.send(11).expect("receiver is alive");

        assert_eq!(expired_stream.next().await, None);
        assert_eq!(live_stream.next().await, Some(11));
    }

    #[tokio::test(start_paused = true)]
    async fn epoch_bump_terminates_within_one_check_interval() {
        let config = RevalidationConfig {
            max_stream_lifetime: Duration::from_mins(1),
            epoch_check_interval: Duration::from_secs(5),
        };
        let authenticator = Arc::new(EpochAuthenticator::default());
        let (sender, receiver) = mpsc::unbounded_channel();
        let mut stream = revalidate_stream(
            receiver_stream(receiver),
            identity(None, 0),
            Some(authenticator.clone()),
            config,
        );

        time::advance(Duration::from_secs(5)).await;
        sender.send(1).expect("receiver is alive");
        assert_eq!(stream.next().await, Some(1));

        authenticator.epoch.store(1, Ordering::SeqCst);
        time::advance(Duration::from_secs(5)).await;

        assert_eq!(stream.next().await, None);
    }

    #[tokio::test(start_paused = true)]
    async fn max_lifetime_terminates_otherwise_valid_stream() {
        let config = RevalidationConfig {
            max_stream_lifetime: Duration::from_secs(10),
            epoch_check_interval: Duration::from_secs(5),
        };
        let (_sender, receiver) = mpsc::unbounded_channel::<u8>();
        let mut stream =
            revalidate_stream(receiver_stream(receiver), identity(None, 0), None, config);

        time::advance(Duration::from_secs(10)).await;

        assert_eq!(stream.next().await, None);
    }

    #[tokio::test(start_paused = true)]
    async fn none_expiry_with_default_authenticator_runs_until_max_lifetime() {
        let config = RevalidationConfig {
            max_stream_lifetime: Duration::from_secs(30),
            epoch_check_interval: Duration::from_secs(5),
        };
        let (sender, receiver) = mpsc::unbounded_channel();
        let mut stream = revalidate_stream(
            receiver_stream(receiver),
            identity(None, 0),
            Some(Arc::new(DefaultEpochAuthenticator)),
            config,
        );

        time::advance(Duration::from_secs(20)).await;
        sender.send(1).expect("receiver is alive");
        assert_eq!(stream.next().await, Some(1));

        time::advance(Duration::from_secs(10)).await;
        assert_eq!(stream.next().await, None);
    }

    #[tokio::test(start_paused = true)]
    async fn queued_items_before_termination_are_delivered() {
        let config = RevalidationConfig {
            max_stream_lifetime: Duration::from_secs(10),
            epoch_check_interval: Duration::from_secs(5),
        };
        let (sender, receiver) = mpsc::unbounded_channel();
        let mut stream =
            revalidate_stream(receiver_stream(receiver), identity(None, 0), None, config);

        sender.send(1).expect("receiver is alive");
        sender.send(2).expect("receiver is alive");
        sender.send(3).expect("receiver is alive");
        time::advance(Duration::from_secs(10)).await;

        assert_eq!(stream.next().await, Some(1));
        assert_eq!(stream.next().await, Some(2));
        assert_eq!(stream.next().await, Some(3));
        assert_eq!(stream.next().await, None);
    }
}

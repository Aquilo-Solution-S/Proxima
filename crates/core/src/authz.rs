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
}

impl Identity {
    #[must_use]
    pub fn can_access_principal(&self, principal: &OwnerRef) -> bool {
        self.accessible_principals.contains(principal)
    }
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
    Wake,
    System,
    /// Fail-closed sentinel for a context that carries no real
    /// credentials (see [`AuthzContext::denied`]).
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
    _private: (),
}

impl SystemAuthority {
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self { _private: () }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AuthzContext {
    identity: Identity,
    capabilities: CapabilitySet,
    auth_path: AuthPath,
    owner_roles: Option<OwnerRoles>,
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
            OwnerRef::World if roles.role_for(&owner).is_some() => {
                OwnerRoles::scoped_to(subject, owner, crate::access::Role::viewer())
            }
            OwnerRef::World | OwnerRef::Personal(_) => return None,
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
            OwnerRef::World | OwnerRef::Group(_) => Self::denied_for_owner(owner),
        }
    }

    /// Fail-closed, zero-capability context. Carries the owner's
    /// identity for audit but grants no roles, an empty tool palette,
    /// and no accessible principals — every capability check and every
    /// owner-scope check denies.
    #[must_use]
    pub fn denied() -> Self {
        Self::denied_for_owner(&OwnerRef::World)
    }

    #[must_use]
    pub fn denied_for_owner(owner: &Owner) -> Self {
        Self {
            identity: Identity {
                subject: None,
                principal: *owner,
                accessible_principals: HashSet::new(),
                expires_at: None,
                auth_epoch: 0,
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
                OwnerRef::World | OwnerRef::Group(_) => None,
            },
            principal,
            accessible_principals,
            expires_at,
            auth_epoch,
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
        let ctx = AuthzContext::denied();

        assert_eq!(ctx.auth_path(), AuthPath::Denied);
        assert_eq!(ctx.subject(), None);
        assert!(!ctx.may_read(&owner, AccessKind::Fact));
        assert!(!ctx.may_write(&owner, AccessKind::Fact));
        assert!(!ctx.may_manage(&owner));
        assert!(ctx.readable_owners(AccessKind::Goal).is_empty());
        assert!(ctx.writable_owners(AccessKind::Goal).is_empty());
    }

    #[test]
    fn resolved_subject_context_gets_world_read_and_role_ceilings() {
        let subject = UserId::new(uuid::Uuid::now_v7());
        let group = OwnerRef::Group(GroupId::new(uuid::Uuid::now_v7()));
        let roles = OwnerRoles::for_subject(subject, [(group, Role::editor())]).unwrap();

        let ctx = AuthzContext::server_resolved(roles, AuthPath::HostBearer);

        assert_eq!(ctx.subject(), Some(subject));
        assert!(ctx.may_read(&OwnerRef::World, AccessKind::Goal));
        assert!(!ctx.may_write(&OwnerRef::World, AccessKind::Fact));
        assert!(ctx.may_write(&OwnerRef::Personal(subject), AccessKind::Goal));
        assert!(ctx.may_write(&group, AccessKind::Perspective));
        assert!(!ctx.may_write(&group, AccessKind::Goal));
        assert!(!ctx.may_manage(&group));

        let readable = ctx.readable_owners(AccessKind::Goal);
        assert!(readable.contains(&OwnerRef::World));
        assert!(readable.contains(&OwnerRef::Personal(subject)));
        assert!(readable.contains(&group));

        let writable = ctx.writable_owners(AccessKind::Goal);
        assert!(writable.contains(&OwnerRef::Personal(subject)));
        assert!(!writable.contains(&OwnerRef::World));
        assert!(!writable.contains(&group));
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

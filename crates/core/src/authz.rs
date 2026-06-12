//! Edge-auth contracts: WHO (`Identity`) vs WHAT (`CapabilitySet`).
//!
//! Transports authenticate once at the edge via a host-provided
//! [`Authenticator`]; the engine performs only pure checks against the
//! resulting [`AuthzContext`]. Companion to `auth.rs`, which now only
//! carries credential material and auth failures.

use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use futures_util::Stream;
use tokio::time::{Instant, Interval, Sleep};

use crate::auth::{AuthError, Credentials};
use crate::{Owner, Principal};

/// WHO: the authorization currency for owner scoping.
#[derive(Debug, Clone, PartialEq)]
pub struct Identity {
    pub principal: Principal,
    pub accessible_principals: HashSet<Principal>,
    /// Streams terminate past this; `None` = no expiry.
    pub expires_at: Option<SystemTime>,
    /// Revocation generation; hosts bump it to force re-auth.
    pub auth_epoch: u64,
}

impl Identity {
    #[must_use]
    pub fn can_access_owner(&self, owner: &Owner) -> bool {
        self.accessible_principals.contains(&owner.principal)
    }
}

/// WHAT: tool palette + operational roles, separate from identity.
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
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "four independent roles per docs/14; named fields keep check sites readable"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoleSet {
    pub graph_read: bool,
    pub graph_write: bool,
    pub source_ingest: bool,
    pub admin: bool,
}

/// Selector for [`RoleSet::has`] checks at verb entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    GraphRead,
    GraphWrite,
    SourceIngest,
    Admin,
}

impl Role {
    /// Stable denial message used in `Forbidden` errors.
    #[must_use]
    pub const fn denied_message(self) -> &'static str {
        match self {
            Self::GraphRead => "requires graph_read role",
            Self::GraphWrite => "requires graph_write role",
            Self::SourceIngest => "requires source_ingest role",
            Self::Admin => "requires admin role",
        }
    }
}

impl RoleSet {
    #[must_use]
    pub const fn all() -> Self {
        Self {
            graph_read: true,
            graph_write: true,
            source_ingest: true,
            admin: true,
        }
    }

    #[must_use]
    pub const fn none() -> Self {
        Self {
            graph_read: false,
            graph_write: false,
            source_ingest: false,
            admin: false,
        }
    }

    #[must_use]
    pub const fn has(self, role: Role) -> bool {
        match role {
            Role::GraphRead => self.graph_read,
            Role::GraphWrite => self.graph_write,
            Role::SourceIngest => self.source_ingest,
            Role::Admin => self.admin,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CapabilitySet {
    pub tool_scope: ToolScope,
    pub roles: RoleSet,
}

impl CapabilitySet {
    #[must_use]
    pub fn all() -> Self {
        Self {
            tool_scope: ToolScope::All,
            roles: RoleSet::all(),
        }
    }
}

/// Provenance of an authenticated context — recorded for audit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthPath {
    HostBearer,
    Wake,
    MasterDev,
    System,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AuthzContext {
    pub identity: Identity,
    pub capabilities: CapabilitySet,
    pub auth_path: AuthPath,
}

impl AuthzContext {
    /// Self-scoped, full-capability context for trusted in-process
    /// surfaces: the desktop shell's IPC commands, embedded
    /// single-owner hosts, the dev-only gRPC service, and tests.
    /// Wire transports never mint this — they authenticate real
    /// credentials at the edge, and the MCP edge rejects host
    /// authenticator output claiming [`AuthPath::System`].
    #[must_use]
    pub fn single_owner(owner: &Owner, auth_path: AuthPath) -> Self {
        let mut principals = HashSet::with_capacity(1);
        principals.insert(owner.principal.clone());
        Self {
            identity: Identity {
                principal: owner.principal.clone(),
                accessible_principals: principals,
                expires_at: None,
                auth_epoch: 0,
            },
            capabilities: CapabilitySet::all(),
            auth_path,
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
    async fn current_auth_epoch(&self, _principal: &Principal) -> u64 {
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
            let principal = self.identity.principal.clone();
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
    use crate::{OrgId, UserId};
    use futures_util::StreamExt;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tokio::sync::mpsc;
    use tokio::time;

    fn owner() -> Owner {
        Owner {
            principal: Principal::User(UserId::new(uuid::Uuid::now_v7())),
            org_id: OrgId::new(uuid::Uuid::now_v7()),
        }
    }

    fn identity(expires_at: Option<SystemTime>, auth_epoch: u64) -> Identity {
        let principal = Principal::User(UserId::new(uuid::Uuid::now_v7()));
        let mut accessible_principals = HashSet::with_capacity(1);
        accessible_principals.insert(principal.clone());
        Identity {
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

        async fn current_auth_epoch(&self, _principal: &Principal) -> u64 {
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
    fn single_owner_context_is_self_scoped_full_capability() {
        let o = owner();
        let ctx = AuthzContext::single_owner(&o, AuthPath::System);

        assert_eq!(ctx.auth_path, AuthPath::System);
        assert!(ctx.identity.can_access_owner(&o));
        assert!(ctx.capabilities.roles.has(Role::Admin));
        assert!(ctx.identity.expires_at.is_none());
    }

    #[test]
    fn roleset_has_matches_fields() {
        let roles = RoleSet {
            graph_read: true,
            graph_write: true,
            source_ingest: false,
            admin: false,
        };
        assert!(roles.has(Role::GraphRead));
        assert!(roles.has(Role::GraphWrite));
        assert!(!roles.has(Role::SourceIngest));
        assert!(!roles.has(Role::Admin));
    }

    #[test]
    fn palette_scope_allows_and_denies() {
        let scope = ToolScope::Palette(vec!["core/fetch_memory".to_string()]);

        assert!(scope.allows("core/fetch_memory"));
        assert!(!scope.allows("core/set_wake_entries"));
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

//! Edge-auth contracts: WHO (`Identity`) vs WHAT (`CapabilitySet`).
//!
//! Transports authenticate once at the edge via a host-provided
//! [`Authenticator`]; the engine performs only pure checks against the
//! resulting [`AuthzContext`]. Companion to `auth.rs`, which this
//! supersedes once the verb surface migrates (see the flavor-app
//! surface plan, rev 3).

use std::collections::HashSet;
use std::time::SystemTime;

use async_trait::async_trait;

use crate::auth::{AuthError, AuthResolver, Credentials};
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
    /// Engine-internal context for dispatcher/wake self-calls.
    /// Sealed `pub(crate)`: no transport, authenticator, or host
    /// constructs it; facade middleware additionally rejects any
    /// authenticator output claiming `AuthPath::System`.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "reserved for engine-internal wake/auth calls")
    )]
    #[must_use]
    pub(crate) fn system(owner: &Owner) -> Self {
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
            auth_path: AuthPath::System,
        }
    }
}

/// The host contract: validate credential material, return an
/// authenticated context. Replaces `AuthResolver` once the verb
/// surface migrates.
#[async_trait]
pub trait Authenticator: Send + Sync {
    /// # Errors
    ///
    /// `AuthError::AuthRequired` when credentials are missing,
    /// `AuthError::InvalidCredentials` when they are malformed,
    /// expired, or revoked.
    async fn authenticate(&self, creds: &Credentials) -> Result<AuthzContext, AuthError>;
}

/// Legacy bridge: adapts a sync `AuthResolver` to `Authenticator`,
/// granting full capabilities (the trust the legacy resolver already
/// implied). Removed together with `AuthResolver`.
pub struct ResolverAuthenticator<R: AuthResolver> {
    inner: R,
    auth_path: AuthPath,
}

impl<R: AuthResolver> std::fmt::Debug for ResolverAuthenticator<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolverAuthenticator")
            .field("auth_path", &self.auth_path)
            .finish_non_exhaustive()
    }
}

impl<R: AuthResolver> ResolverAuthenticator<R> {
    #[must_use]
    pub fn new(inner: R, auth_path: AuthPath) -> Self {
        Self { inner, auth_path }
    }
}

#[async_trait]
impl<R: AuthResolver> Authenticator for ResolverAuthenticator<R> {
    async fn authenticate(&self, creds: &Credentials) -> Result<AuthzContext, AuthError> {
        let resolved = self.inner.resolve(creds)?;
        Ok(AuthzContext {
            identity: Identity {
                principal: resolved.principal,
                accessible_principals: resolved.accessible_principals,
                expires_at: None,
                auth_epoch: 0,
            },
            capabilities: CapabilitySet::all(),
            auth_path: self.auth_path,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::NoAuth;
    use crate::{OrgId, UserId};

    fn owner() -> Owner {
        Owner {
            principal: Principal::User(UserId::new(uuid::Uuid::now_v7())),
            org_id: OrgId::new(uuid::Uuid::now_v7()),
        }
    }

    #[tokio::test]
    async fn resolver_adapter_grants_full_capabilities() {
        let o = owner();
        let auth = ResolverAuthenticator::new(
            NoAuth::new(o.principal.clone(), o.clone()),
            AuthPath::MasterDev,
        );

        let ctx = auth
            .authenticate(&Credentials::Bearer("x".to_string()))
            .await
            .unwrap();

        assert!(ctx.identity.can_access_owner(&o));
        assert_eq!(ctx.auth_path, AuthPath::MasterDev);
        assert!(ctx.capabilities.tool_scope.allows("anything"));
        assert!(ctx.capabilities.roles.admin);
        assert!(ctx.identity.expires_at.is_none());
    }

    #[test]
    fn system_context_is_owner_scoped_and_marked() {
        let o = owner();
        let ctx = AuthzContext::system(&o);

        assert_eq!(ctx.auth_path, AuthPath::System);
        assert!(ctx.identity.can_access_owner(&o));
    }

    #[test]
    fn palette_scope_allows_and_denies() {
        let scope = ToolScope::Palette(vec!["core/fetch_memory".to_string()]);

        assert!(scope.allows("core/fetch_memory"));
        assert!(!scope.allows("core/set_wake_entries"));
    }
}

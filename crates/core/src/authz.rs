//! Edge-auth contracts: WHO (`Identity`) vs WHAT (`CapabilitySet`).
//!
//! Transports authenticate once at the edge via a host-provided
//! [`Authenticator`]; the engine performs only pure checks against the
//! resulting [`AuthzContext`]. Companion to `auth.rs`, which now only
//! carries credential material and auth failures.

use std::collections::HashSet;
use std::time::SystemTime;

use async_trait::async_trait;

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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{OrgId, UserId};

    fn owner() -> Owner {
        Owner {
            principal: Principal::User(UserId::new(uuid::Uuid::now_v7())),
            org_id: OrgId::new(uuid::Uuid::now_v7()),
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
}

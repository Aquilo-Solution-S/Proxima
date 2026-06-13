//! MCP edge authentication: typed wire-token dispatch onto the core
//! authz contracts.
//!
//! The bearer string is parsed into a [`WireToken`] before any
//! resolution: reserved `pxw_`/`pxm_` namespaces route to the wake
//! store / local master map, everything else goes to the optional host
//! [`Authenticator`]. No probing: wake material never reaches host
//! auth and host material never reaches the wake store.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use proxima_core::wake::token_store::{WakeTokenContext, WakeTokenStore};
use proxima_core::{
    AuthPath, Authenticator, AuthzContext, CapabilitySet, Credentials, Identity, Owner, RoleSet,
    ToolScope,
};
use tokio::sync::RwLock;
use uuid::Uuid;

/// Reserved wire prefix for engine-minted wake tokens.
pub const WAKE_TOKEN_PREFIX: &str = "pxw_";
/// Reserved wire prefix for local dev master tokens.
pub const MASTER_TOKEN_PREFIX: &str = "pxm_";

/// Typed wire credential parsed from the bearer string before any
/// store/authenticator sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum WireToken {
    Wake(Uuid),
    Master(Uuid),
    Host(String),
    /// Reserved prefix with a malformed remainder. Fails closed:
    /// never forwarded to host auth.
    Malformed,
}

fn parse_wire_token(raw: &str) -> WireToken {
    if let Some(rest) = raw.strip_prefix(WAKE_TOKEN_PREFIX) {
        return Uuid::parse_str(rest).map_or(WireToken::Malformed, WireToken::Wake);
    }
    if let Some(rest) = raw.strip_prefix(MASTER_TOKEN_PREFIX) {
        return Uuid::parse_str(rest).map_or(WireToken::Malformed, WireToken::Master);
    }
    WireToken::Host(raw.to_string())
}

fn self_scoped_identity(owner: &Owner) -> Identity {
    let mut accessible = HashSet::with_capacity(1);
    accessible.insert(owner.principal.clone());
    Identity {
        principal: owner.principal.clone(),
        org_id: owner.org_id,
        accessible_principals: accessible,
        // Wake-store TTL / master rotation govern today; stream
        // expiry lands with the revalidation slice.
        expires_at: None,
        auth_epoch: 0,
    }
}

/// Per-request MCP auth context injected by the edge middleware.
/// `authz` is the core authorization currency; the remaining fields
/// are MCP-session specifics the tool host needs.
#[derive(Debug, Clone)]
pub struct McpAuthContext {
    pub owner: Owner,
    pub authz: AuthzContext,
    pub model_id: Option<String>,
    pub wake: Option<WakeTokenContext>,
    pub master_token_id: Option<Uuid>,
}

impl McpAuthContext {
    /// Context for a wake-minted invocation: palette-scoped tools,
    /// graph read/write, no ingest, no admin.
    #[must_use]
    pub fn for_wake(wake: WakeTokenContext) -> Self {
        Self {
            owner: wake.owner.clone(),
            authz: AuthzContext {
                identity: self_scoped_identity(&wake.owner),
                capabilities: CapabilitySet {
                    tool_scope: ToolScope::Palette(wake.palette.clone()),
                    roles: RoleSet {
                        graph_read: true,
                        graph_write: true,
                        source_ingest: false,
                        admin: false,
                    },
                },
                auth_path: AuthPath::Wake,
            },
            model_id: Some(wake.model_id.clone()),
            wake: Some(wake),
            master_token_id: None,
        }
    }

    /// Context for a local dev master token: all tools, all roles.
    #[must_use]
    pub fn for_master(token: Uuid, owner: Owner) -> Self {
        Self {
            authz: AuthzContext {
                identity: self_scoped_identity(&owner),
                capabilities: CapabilitySet::all(),
                auth_path: AuthPath::MasterDev,
            },
            owner,
            model_id: None,
            wake: None,
            master_token_id: Some(token),
        }
    }
}

/// Edge resolver replacing the probe-by-UUID auth store. Composition
/// decides which paths exist: engine-hosted gets the wake path,
/// headless does not, and a host authenticator is opt-in.
pub struct McpEdgeAuth {
    wake_tokens: Option<Arc<WakeTokenStore>>,
    master_tokens: RwLock<HashMap<Uuid, Owner>>,
    host: Option<(Arc<dyn Authenticator>, Owner)>,
}

impl std::fmt::Debug for McpEdgeAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpEdgeAuth")
            .field("wake_path", &self.wake_tokens.is_some())
            .field("host_path", &self.host.is_some())
            .finish_non_exhaustive()
    }
}

impl McpEdgeAuth {
    /// Engine-hosted composition: wake + master paths.
    #[must_use]
    pub fn engine_hosted(wake_tokens: Arc<WakeTokenStore>) -> Self {
        Self {
            wake_tokens: Some(wake_tokens),
            master_tokens: RwLock::new(HashMap::new()),
            host: None,
        }
    }

    /// Headless composition: master path only. No engine mints wake
    /// tokens here, so there is no wake path.
    #[must_use]
    pub fn headless() -> Self {
        Self {
            wake_tokens: None,
            master_tokens: RwLock::new(HashMap::new()),
            host: None,
        }
    }

    /// Attach a host authenticator for non-reserved bearer material.
    /// Host output must carry `AuthPath::HostBearer` and an identity
    /// that can access `owner`; anything else fails closed.
    #[must_use]
    pub fn with_host(mut self, authenticator: Arc<dyn Authenticator>, owner: Owner) -> Self {
        self.host = Some((authenticator, owner));
        self
    }

    pub async fn replace_local_master_token(&self, token: Uuid, owner: Owner) {
        let mut guard = self.master_tokens.write().await;
        guard.retain(|_, existing| existing != &owner);
        guard.insert(token, owner);
    }

    pub async fn resolve(&self, raw_bearer: &str) -> Option<McpAuthContext> {
        match parse_wire_token(raw_bearer) {
            WireToken::Wake(token) => self.resolve_wake(token).await,
            WireToken::Master(token) => self.resolve_master(token).await,
            WireToken::Host(material) => self.resolve_host(material).await,
            WireToken::Malformed => None,
        }
    }

    pub(crate) fn host_authenticator(&self) -> Option<Arc<dyn Authenticator>> {
        self.host
            .as_ref()
            .map(|(authenticator, _owner)| authenticator.clone())
    }

    async fn resolve_wake(&self, token: Uuid) -> Option<McpAuthContext> {
        let wake = self.wake_tokens.as_ref()?.resolve(token).await?;
        Some(McpAuthContext::for_wake(wake))
    }

    async fn resolve_master(&self, token: Uuid) -> Option<McpAuthContext> {
        let owner = self.master_tokens.read().await.get(&token).cloned()?;
        Some(McpAuthContext::for_master(token, owner))
    }

    async fn resolve_host(&self, material: String) -> Option<McpAuthContext> {
        let (authenticator, owner) = self.host.as_ref()?;
        let authz = authenticator
            .authenticate(&Credentials::Bearer(material))
            .await
            .ok()?;
        if authz.auth_path != AuthPath::HostBearer {
            return None;
        }
        if !authz.identity.can_access_principal(&owner.principal) {
            return None;
        }
        Some(McpAuthContext {
            owner: authz.scoped_owner(owner.principal.clone()),
            authz,
            model_id: None,
            wake: None,
            master_token_id: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use async_trait::async_trait;
    use proxima_core::mcp::{HandleTable, MemoryHandleClass};
    use proxima_core::personality::WakeChainDepth;
    use proxima_core::{AuthError, MemoryId, OrgId, Principal, UserId};

    fn fake_owner() -> Owner {
        Owner {
            principal: Principal::User(UserId::new(uuid::Uuid::now_v7())),
            org_id: OrgId::new(uuid::Uuid::now_v7()),
        }
    }

    fn make_wake_ctx(owner: Owner) -> WakeTokenContext {
        WakeTokenContext {
            invocation_id: Uuid::now_v7(),
            personality_instance_id: Uuid::now_v7(),
            wake_entry_id: Uuid::now_v7(),
            change_event_seq: Uuid::now_v7(),
            owner,
            palette: vec!["core/emit_abstraction".into()],
            model_id: "anthropic/claude-3-5-sonnet".into(),
            max_rounds: 4,
            current_root_perspective_memory_id: MemoryId::new(Uuid::now_v7()),
            current_root_perspective_memory_class: MemoryHandleClass::Perspective,
            triggering_event_memory_id: MemoryId::new(Uuid::now_v7()),
            triggering_event_memory_class: MemoryHandleClass::Fact,
            triggering_event_depth: WakeChainDepth::new(0),
            read_log: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            handles: Arc::new(HandleTable::new()),
        }
    }

    struct StubHostAuth {
        result: Result<AuthzContext, AuthError>,
    }

    #[async_trait]
    impl Authenticator for StubHostAuth {
        async fn authenticate(&self, _creds: &Credentials) -> Result<AuthzContext, AuthError> {
            self.result.clone()
        }
    }

    #[test]
    fn parse_routes_reserved_prefixes_and_host_material() {
        let id = Uuid::now_v7();
        assert_eq!(
            parse_wire_token(&format!("{WAKE_TOKEN_PREFIX}{id}")),
            WireToken::Wake(id)
        );
        assert_eq!(
            parse_wire_token(&format!("{MASTER_TOKEN_PREFIX}{id}")),
            WireToken::Master(id)
        );
        assert_eq!(parse_wire_token("pxw_not-a-uuid"), WireToken::Malformed);
        assert_eq!(parse_wire_token("pxm_"), WireToken::Malformed);
        assert_eq!(
            parse_wire_token(&id.to_string()),
            WireToken::Host(id.to_string())
        );
    }

    #[tokio::test]
    async fn wake_token_resolves_only_via_wake_prefix() {
        let store = Arc::new(WakeTokenStore::new(Duration::from_mins(5)));
        let owner = fake_owner();
        let token = store.mint(make_wake_ctx(owner.clone())).await;
        let auth = McpEdgeAuth::engine_hosted(store);

        let ctx = auth
            .resolve(&format!("{WAKE_TOKEN_PREFIX}{token}"))
            .await
            .expect("wake resolves");
        assert_eq!(ctx.authz.auth_path, AuthPath::Wake);
        assert!(!ctx.authz.capabilities.roles.admin);
        assert!(!ctx.authz.capabilities.roles.source_ingest);
        assert!(ctx.wake.is_some());
        assert_eq!(ctx.owner, owner);

        assert!(
            auth.resolve(&format!("{MASTER_TOKEN_PREFIX}{token}"))
                .await
                .is_none()
        );
        assert!(auth.resolve(&token.to_string()).await.is_none());
    }

    #[tokio::test]
    async fn master_token_resolves_only_via_master_prefix() {
        let auth = McpEdgeAuth::headless();
        let owner = fake_owner();
        let token = Uuid::now_v7();
        auth.replace_local_master_token(token, owner.clone()).await;

        let ctx = auth
            .resolve(&format!("{MASTER_TOKEN_PREFIX}{token}"))
            .await
            .expect("master resolves");
        assert_eq!(ctx.authz.auth_path, AuthPath::MasterDev);
        assert_eq!(ctx.master_token_id, Some(token));
        assert!(ctx.authz.capabilities.roles.admin);
        assert!(ctx.authz.capabilities.tool_scope.allows("anything"));

        assert!(
            auth.resolve(&format!("{WAKE_TOKEN_PREFIX}{token}"))
                .await
                .is_none()
        );
        assert!(auth.resolve(&token.to_string()).await.is_none());
    }

    #[tokio::test]
    async fn headless_has_no_wake_path() {
        let auth = McpEdgeAuth::headless();
        assert!(
            auth.resolve(&format!("{WAKE_TOKEN_PREFIX}{}", Uuid::now_v7()))
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn host_material_fails_closed_without_host_authenticator() {
        let auth = McpEdgeAuth::headless();
        assert!(auth.resolve("some-opaque-host-token").await.is_none());
    }

    #[tokio::test]
    async fn host_path_accepts_host_bearer_scoped_to_owner() {
        let owner = fake_owner();
        let mut accessible = HashSet::new();
        accessible.insert(owner.principal.clone());
        let authz = AuthzContext {
            identity: Identity {
                principal: owner.principal.clone(),
                org_id: owner.org_id,
                accessible_principals: accessible,
                expires_at: None,
                auth_epoch: 0,
            },
            capabilities: CapabilitySet::all(),
            auth_path: AuthPath::HostBearer,
        };
        let auth = McpEdgeAuth::headless()
            .with_host(Arc::new(StubHostAuth { result: Ok(authz) }), owner.clone());

        let ctx = auth.resolve("host-token").await.expect("host resolves");
        assert_eq!(ctx.authz.auth_path, AuthPath::HostBearer);
        assert_eq!(ctx.owner, owner);
        assert!(ctx.wake.is_none());
        assert!(ctx.master_token_id.is_none());
    }

    #[tokio::test]
    async fn host_claiming_system_or_engine_paths_is_rejected() {
        let owner = fake_owner();
        for path in [AuthPath::System, AuthPath::Wake, AuthPath::MasterDev] {
            let mut accessible = HashSet::new();
            accessible.insert(owner.principal.clone());
            let authz = AuthzContext {
                identity: Identity {
                    principal: owner.principal.clone(),
                    org_id: owner.org_id,
                    accessible_principals: accessible,
                    expires_at: None,
                    auth_epoch: 0,
                },
                capabilities: CapabilitySet::all(),
                auth_path: path,
            };
            let auth = McpEdgeAuth::headless()
                .with_host(Arc::new(StubHostAuth { result: Ok(authz) }), owner.clone());
            assert!(
                auth.resolve("host-token").await.is_none(),
                "{path:?} must be rejected"
            );
        }
    }

    #[tokio::test]
    async fn host_identity_without_owner_access_is_rejected() {
        let owner = fake_owner();
        let stranger = fake_owner();
        let mut accessible = HashSet::new();
        accessible.insert(stranger.principal.clone());
        let authz = AuthzContext {
            identity: Identity {
                principal: stranger.principal.clone(),
                org_id: stranger.org_id,
                accessible_principals: accessible,
                expires_at: None,
                auth_epoch: 0,
            },
            capabilities: CapabilitySet::all(),
            auth_path: AuthPath::HostBearer,
        };
        let auth =
            McpEdgeAuth::headless().with_host(Arc::new(StubHostAuth { result: Ok(authz) }), owner);
        assert!(auth.resolve("host-token").await.is_none());
    }

    #[tokio::test]
    async fn malformed_reserved_prefix_is_not_forwarded_to_host() {
        let owner = fake_owner();
        let mut accessible = HashSet::new();
        accessible.insert(owner.principal.clone());
        let authz = AuthzContext {
            identity: Identity {
                principal: owner.principal.clone(),
                org_id: owner.org_id,
                accessible_principals: accessible,
                expires_at: None,
                auth_epoch: 0,
            },
            capabilities: CapabilitySet::all(),
            auth_path: AuthPath::HostBearer,
        };
        let auth =
            McpEdgeAuth::headless().with_host(Arc::new(StubHostAuth { result: Ok(authz) }), owner);
        assert!(auth.resolve("pxw_not-a-uuid").await.is_none());
        assert!(auth.resolve("pxm_not-a-uuid").await.is_none());
    }
}

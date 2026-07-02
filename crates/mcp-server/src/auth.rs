//! MCP edge authentication: typed wire-token dispatch onto the core
//! authz contracts.
//!
//! The bearer string is parsed into a [`WireToken`] before any
//! resolution: the reserved `pxm_` namespace routes to the local master
//! map, former wake-token material fails closed, and everything else
//! goes to the optional host [`Authenticator`].

use std::collections::HashMap;
use std::sync::Arc;

use proxima_core::{AuthPath, Authenticator, AuthzContext, Credentials, Owner, ToolScope};
use tokio::sync::RwLock;
use uuid::Uuid;

const RESERVED_PXW_PREFIX: &str = "pxw_";
/// Reserved wire prefix for local dev master tokens.
pub const MASTER_TOKEN_PREFIX: &str = "pxm_";

/// Typed wire credential parsed from the bearer string before any
/// store/authenticator sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum WireToken {
    Master(Uuid),
    Host(String),
    /// Reserved prefix with a malformed remainder. Fails closed:
    /// never forwarded to host auth.
    Malformed,
}

fn parse_wire_token(raw: &str) -> WireToken {
    if raw.starts_with(RESERVED_PXW_PREFIX) {
        return WireToken::Malformed;
    }
    if let Some(rest) = raw.strip_prefix(MASTER_TOKEN_PREFIX) {
        return Uuid::parse_str(rest).map_or(WireToken::Malformed, WireToken::Master);
    }
    WireToken::Host(raw.to_string())
}

/// Per-request MCP auth context injected by the edge middleware.
/// `authz` is the core authorization currency; the remaining fields
/// are MCP-session specifics the tool host needs.
#[derive(Debug, Clone)]
pub struct McpAuthContext {
    pub owner: Owner,
    pub authz: AuthzContext,
    pub model_id: Option<String>,
    pub master_token_id: Option<Uuid>,
}

impl McpAuthContext {
    /// Context for a local dev master token: all tools, unrestricted access.
    #[must_use]
    pub fn for_master(token: Uuid, owner: Owner) -> Self {
        Self {
            authz: AuthzContext::single_owner(&owner, AuthPath::MasterDev),
            owner,
            model_id: None,
            master_token_id: Some(token),
        }
    }
}

/// Edge resolver replacing the probe-by-UUID auth store. Composition
/// decides which paths exist: local master is always available, and a
/// host authenticator is opt-in.
pub struct McpEdgeAuth {
    master_tokens: RwLock<HashMap<Uuid, Owner>>,
    host: Option<(Arc<dyn Authenticator>, Owner)>,
    tool_scope: ToolScope,
}

impl std::fmt::Debug for McpEdgeAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpEdgeAuth")
            .field("host_path", &self.host.is_some())
            .finish_non_exhaustive()
    }
}

impl McpEdgeAuth {
    /// Master-token composition. No engine mints wake tokens.
    #[must_use]
    pub fn headless() -> Self {
        Self {
            master_tokens: RwLock::new(HashMap::new()),
            host: None,
            tool_scope: ToolScope::All,
        }
    }

    /// Layer a deployment-wide MCP tool surface over every resolved request.
    /// The deployment scope is INTERSECTED with each caller's own scope (see
    /// [`ToolScope::intersect`]), so it can only ever narrow a caller, never
    /// widen one — a future per-actor palette stays respected.
    #[must_use]
    pub fn with_tool_scope(mut self, tool_scope: ToolScope) -> Self {
        self.tool_scope = tool_scope;
        self
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

    async fn resolve_master(&self, token: Uuid) -> Option<McpAuthContext> {
        let owner = self.master_tokens.read().await.get(&token).copied()?;
        let mut ctx = McpAuthContext::for_master(token, owner);
        let tool_scope = ctx.authz.tool_scope().intersect(&self.tool_scope);
        ctx.authz = ctx.authz.with_tool_scope(tool_scope);
        Some(ctx)
    }

    async fn resolve_host(&self, material: String) -> Option<McpAuthContext> {
        let (authenticator, owner) = self.host.as_ref()?;
        let authz = authenticator
            .authenticate(&Credentials::Bearer(material))
            .await
            .ok()?;
        if authz.auth_path() != AuthPath::HostBearer {
            return None;
        }
        let tool_scope = authz.tool_scope().intersect(&self.tool_scope);
        let authz = authz.narrowed_to_owner(*owner)?.with_tool_scope(tool_scope);
        Some(McpAuthContext {
            owner: authz.scoped_owner(*owner),
            authz,
            model_id: None,
            master_token_id: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use proxima_core::{
        AccessScope, AuthError, Engine, ErrorCode, FlavorRegistry, GetGraphReadRequest, GroupId,
        OwnerRef, Relation, Role, UserId,
    };

    fn fake_owner() -> Owner {
        OwnerRef::Personal(fake_user())
    }

    fn fake_user() -> UserId {
        UserId::new(uuid::Uuid::now_v7())
    }

    fn fake_group_owner() -> Owner {
        OwnerRef::Group(GroupId::new(uuid::Uuid::now_v7()))
    }

    type TestAuthz = AuthzContext;

    fn host_owner_context(owner: Owner) -> TestAuthz {
        AuthzContext::single_owner(&owner, AuthPath::HostBearer)
    }

    fn host_group_context(subject: UserId, group_owner: Owner, role: Role) -> TestAuthz {
        AuthzContext::for_subject_with_role(subject, [(group_owner, role)], AuthPath::HostBearer)
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
            parse_wire_token(&format!("{RESERVED_PXW_PREFIX}{id}")),
            WireToken::Malformed
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
    async fn master_token_resolves_only_via_master_prefix() {
        let auth = McpEdgeAuth::headless();
        let owner = fake_owner();
        let token = Uuid::now_v7();
        auth.replace_local_master_token(token, owner).await;

        let ctx = auth
            .resolve(&format!("{MASTER_TOKEN_PREFIX}{token}"))
            .await
            .expect("master resolves");
        assert_eq!(ctx.authz.auth_path(), AuthPath::MasterDev);
        assert_eq!(ctx.master_token_id, Some(token));
        assert_eq!(ctx.authz.access_scope(), AccessScope::Unrestricted);
        assert!(ctx.authz.tool_scope().allows("anything"));

        assert!(
            auth.resolve(&format!("{RESERVED_PXW_PREFIX}{token}"))
                .await
                .is_none()
        );
        assert!(auth.resolve(&token.to_string()).await.is_none());
    }

    #[tokio::test]
    async fn former_wake_prefix_fails_closed() {
        let auth = McpEdgeAuth::headless();
        assert!(
            auth.resolve(&format!("{RESERVED_PXW_PREFIX}{}", Uuid::now_v7()))
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
        let authz = host_owner_context(owner);
        let auth =
            McpEdgeAuth::headless().with_host(Arc::new(StubHostAuth { result: Ok(authz) }), owner);

        let ctx = auth.resolve("host-token").await.expect("host resolves");
        assert_eq!(ctx.authz.auth_path(), AuthPath::HostBearer);
        assert_eq!(ctx.owner, owner);
        assert!(ctx.master_token_id.is_none());
    }

    #[tokio::test]
    async fn host_path_accepts_resolved_host_context_for_configured_owner() {
        let owner = fake_owner();
        let authz = host_owner_context(owner);
        let auth =
            McpEdgeAuth::headless().with_host(Arc::new(StubHostAuth { result: Ok(authz) }), owner);

        let ctx = auth.resolve("host-token").await.expect("host resolves");
        assert_eq!(ctx.owner, owner);
        assert!(ctx.authz.can_access_owner(&owner));
    }

    #[tokio::test]
    async fn host_context_without_configured_owner_role_is_rejected() {
        let owner = fake_group_owner();
        let authz = AuthzContext::for_subject(fake_user(), AuthPath::HostBearer);
        let auth =
            McpEdgeAuth::headless().with_host(Arc::new(StubHostAuth { result: Ok(authz) }), owner);

        assert!(auth.resolve("host-token").await.is_none());
    }

    #[tokio::test]
    async fn owner_scoped_ops_deny_host_context_after_role_narrowing() {
        let owner = fake_group_owner();
        let other = fake_group_owner();
        let subject = fake_user();
        let authz = host_group_context(subject, other, Role::admin());
        let auth =
            McpEdgeAuth::headless().with_host(Arc::new(StubHostAuth { result: Ok(authz) }), other);
        let ctx = auth.resolve("host-token").await.expect("host resolves");
        let engine = Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests());

        let err = engine
            .get_graph(
                &ctx.authz,
                &GetGraphReadRequest {
                    owner,
                    include_tombstoned: false,
                },
            )
            .await
            .expect_err("configured owner role must not widen to another owner");

        assert_eq!(err.code, ErrorCode::Forbidden);
    }

    #[tokio::test]
    async fn host_path_restricts_host_context_to_configured_owner() {
        let owner = fake_group_owner();
        let other = fake_group_owner();
        let subject = fake_user();
        let authz = AuthzContext::for_subject_with_role(
            subject,
            [(owner, Role::admin()), (other, Role::admin())],
            AuthPath::HostBearer,
        );
        let auth =
            McpEdgeAuth::headless().with_host(Arc::new(StubHostAuth { result: Ok(authz) }), owner);

        let ctx = auth.resolve("host-token").await.expect("host resolves");
        assert!(ctx.authz.can_access_owner(&owner));
        assert!(!ctx.authz.can_access_owner(&other));
    }

    #[tokio::test]
    async fn host_claiming_system_or_engine_paths_is_rejected() {
        let owner = fake_owner();
        for path in [AuthPath::System, AuthPath::Wake, AuthPath::MasterDev] {
            let authz = AuthzContext::single_owner(&owner, path);
            let auth = McpEdgeAuth::headless()
                .with_host(Arc::new(StubHostAuth { result: Ok(authz) }), owner);
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
        let authz = host_owner_context(stranger);
        let auth =
            McpEdgeAuth::headless().with_host(Arc::new(StubHostAuth { result: Ok(authz) }), owner);
        assert!(auth.resolve("host-token").await.is_none());
    }

    /// Task-3 host-resolved boundary: a subject with no role at all on the
    /// deployment-configured owner is rejected at resolve, not later at an
    /// engine call. Same fail-closed shape as
    /// `host_context_without_configured_owner_role_is_rejected`, restated
    /// against the `server_resolved` vocabulary the default OIDC path now
    /// always produces.
    #[tokio::test]
    async fn host_resolved_user_without_role_on_configured_owner_is_rejected() {
        let owner = fake_group_owner();
        let authz = AuthzContext::for_subject(fake_user(), AuthPath::HostBearer);
        let auth =
            McpEdgeAuth::headless().with_host(Arc::new(StubHostAuth { result: Ok(authz) }), owner);

        assert!(auth.resolve("host-token").await.is_none());
    }

    /// A host-resolved subject with only `Viewer` on the configured group
    /// owner resolves (viewer is a real role), but a manage-only engine
    /// operation on that owner must still fail.
    #[tokio::test]
    async fn host_resolved_user_with_viewer_role_cannot_manage_group() {
        let owner = fake_group_owner();
        let subject = fake_user();
        let authz = host_group_context(subject, owner, Role::viewer());
        let auth =
            McpEdgeAuth::headless().with_host(Arc::new(StubHostAuth { result: Ok(authz) }), owner);

        let ctx = auth
            .resolve("host-token")
            .await
            .expect("viewer role on the configured owner still resolves");
        assert!(ctx.authz.can_access_owner(&owner));

        let OwnerRef::Group(group) = owner else {
            unreachable!("fake_group_owner is always a Group owner")
        };
        let engine = Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests());
        let err = engine
            .add_member(&ctx.authz, group, fake_user(), Relation::Editor)
            .await
            .expect_err("viewer role must not be able to manage group membership");

        assert_eq!(err.code, ErrorCode::Forbidden);
    }

    #[tokio::test]
    async fn malformed_reserved_prefix_is_not_forwarded_to_host() {
        let owner = fake_owner();
        let authz = host_owner_context(owner);
        let auth =
            McpEdgeAuth::headless().with_host(Arc::new(StubHostAuth { result: Ok(authz) }), owner);
        assert!(auth.resolve("pxw_not-a-uuid").await.is_none());
        assert!(auth.resolve("pxm_not-a-uuid").await.is_none());
    }
}

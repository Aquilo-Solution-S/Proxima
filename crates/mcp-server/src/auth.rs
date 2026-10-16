//! MCP edge authentication: typed wire-token dispatch onto the core
//! authz contracts. Bearer credentials are interpreted by the host authenticator.

use std::sync::Arc;

use proxima_core::{
    AuthPath, Authenticator, AuthzContext, Credentials, GroupId, OwnerAccessPort, OwnerId,
    ServedSurface, authenticate,
};

/// Per-request MCP auth context injected by the edge middleware.
/// `authz` is the core authorization currency; `owner` is the
/// MCP-session specific the tool host needs.
///
/// There is no model-id slot here. Trusted model provenance lives on
/// `authz` ([`AuthzContext::trusted_model_id`]) because that is the value
/// an authenticator produced; a second, edge-local copy could only ever
/// disagree with it.
#[derive(Debug, Clone)]
pub struct McpAuthContext {
    pub owner: OwnerId,
    pub authz: AuthzContext,
}

impl McpAuthContext {
    #[must_use]
    fn bound(authz: AuthzContext, owner: OwnerId) -> Self {
        Self { owner, authz }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedAuth {
    authz: AuthzContext,
}

impl ResolvedAuth {
    pub(crate) fn narrowed_to_owner(self, owner: OwnerId) -> Option<McpAuthContext> {
        let authz = self.authz.narrowed_to_owner(owner)?;
        Some(McpAuthContext::bound(authz, owner))
    }

    /// The owner the host named for a request that selects none.
    pub(crate) const fn default_owner(&self) -> Option<OwnerId> {
        self.authz.default_owner()
    }

    /// The mandatory owner carried by a verified forwarding credential.
    pub(crate) const fn verified_forwarded_owner(&self) -> Option<OwnerId> {
        self.authz.verified_forwarded_owner()
    }

    /// Whether the eager map already carries the selected owner.
    fn carries(&self, owner: OwnerId) -> bool {
        self.authz.role_for_owner(&owner).is_some()
    }

    /// The loop back through the port for one Group the eager map lacks:
    /// ask, fold the answer into the map, rejoin. There is no second way to
    /// narrow — [`Self::narrowed_to_owner`] reads the map afterwards under
    /// the same rules as for every eagerly resolved entry. An absent port,
    /// `Ok(None)` and an error all fold nothing, so the map is exactly what
    /// it was and the one path refuses as it always did.
    async fn filled_from(self, port: Option<&dyn OwnerAccessPort>, group: GroupId) -> Option<Self> {
        let (Some(port), Some(_subject)) = (port, self.authz.subject()) else {
            return Some(self);
        };
        Some(Self {
            authz: self
                .authz
                .with_authenticated_group_role(port, group)
                .await
                .or_else(|| {
                    tracing::warn!("mcp edge: per-group access resolution failed");
                    None
                })?,
        })
    }
}

/// Edge resolver replacing the probe-by-UUID auth store. Composition
/// decides whether a host authenticator exists.
pub struct McpEdgeAuth {
    host: Option<Arc<dyn Authenticator>>,
    owner_access: Option<Arc<dyn OwnerAccessPort>>,
    served_surface: ServedSurface,
}

impl std::fmt::Debug for McpEdgeAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpEdgeAuth")
            .field("host_path", &self.host.is_some())
            .field("per_owner_resolution", &self.owner_access.is_some())
            .finish_non_exhaustive()
    }
}

impl McpEdgeAuth {
    /// Headless composition. No local bearer tokens are accepted.
    #[must_use]
    pub fn headless() -> Self {
        Self {
            host: None,
            owner_access: None,
            served_surface: ServedSurface::All,
        }
    }

    /// Restrict the deployment surface independently of each caller's exact
    /// scope. Registered reads remain served without granting caller keys.
    #[must_use]
    pub fn with_served_surface(mut self, served_surface: ServedSurface) -> Self {
        self.served_surface = match self.served_surface {
            ServedSurface::All => served_surface,
            current @ ServedSurface::Palette(_) => current.intersect(&served_surface),
        };
        self
    }

    /// Attach the host authenticator for bearer credentials.
    /// Host output must carry `AuthPath::HostBearer`; owner binding is
    /// validated separately against the resolved role set.
    #[must_use]
    pub fn with_host(mut self, authenticator: Arc<dyn Authenticator>) -> Self {
        self.host = Some(authenticator);
        self
    }

    /// Attach host policy for ordinary on-demand Group roles and verified
    /// forwarding claims. A configured forwarder must carry a verified
    /// claim even when its eager role map already contains the selection.
    #[must_use]
    pub fn with_owner_access(mut self, owner_access: Arc<dyn OwnerAccessPort>) -> Self {
        self.owner_access = Some(owner_access);
        self
    }

    pub async fn resolve(&self, raw_bearer: &str, owner: OwnerId) -> Option<McpAuthContext> {
        let resolved = self.resolve_unbound(raw_bearer).await?;
        self.narrow_to_owner(resolved, owner).await
    }

    /// Bind an authenticated request to its selected owner. Verified
    /// forwarding claims use only their configured policy role; neither an
    /// eager role nor an ordinary membership can bypass that binding.
    /// Ordinary subjects retain eager and on-demand Group resolution.
    pub(crate) async fn narrow_to_owner(
        &self,
        resolved: ResolvedAuth,
        owner: OwnerId,
    ) -> Option<McpAuthContext> {
        if resolved.verified_forwarded_owner().is_some() {
            let authz = resolved
                .authz
                .with_authenticated_forwarded_owner(self.owner_access.as_deref()?, owner)
                .await?;
            return Some(McpAuthContext::bound(authz, owner));
        }
        if let (Some(port), Some(subject)) =
            (self.owner_access.as_deref(), resolved.authz.subject())
            && port
                .is_forwarder(subject)
                .inspect_err(|error| {
                    tracing::warn!(%error, "mcp edge: forwarding policy resolution failed");
                })
                .ok()?
        {
            return None;
        }
        let resolved = if resolved.carries(owner) {
            resolved
        } else {
            resolved
                .filled_from(self.owner_access.as_deref(), owner.as_group())
                .await?
        };
        resolved.narrowed_to_owner(owner)
    }

    /// Authenticate `raw_bearer` on an accepted path without narrowing to
    /// an owner. The edge middleware retains this result across owner/session
    /// selection so host authentication happens exactly once per request.
    pub(crate) async fn resolve_unbound(&self, raw_bearer: &str) -> Option<ResolvedAuth> {
        self.resolve_host(raw_bearer.to_owned()).await
    }

    pub(crate) fn host_authenticator(&self) -> Option<Arc<dyn Authenticator>> {
        self.host.clone()
    }

    async fn resolve_host(&self, material: String) -> Option<ResolvedAuth> {
        let authenticator = self.host.as_ref()?;
        let mut authz = authenticate(authenticator.as_ref(), &Credentials::Bearer(material))
            .await
            .ok()?;
        if authz.auth_path() != AuthPath::HostBearer {
            return None;
        }
        authz = authz.with_served_surface(self.served_surface.clone());
        Some(ResolvedAuth { authz })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, SystemTime};

    use async_trait::async_trait;
    use proxima_core::{
        AccessError, AccessKind, AuthError, Engine, ErrorCode, FlavorRegistry, ForwarderRole,
        GetGraphReadRequest, GroupId, OwnerId, OwnerRoles, Relation, Role, ScopeKey, ToolScope,
        UserId,
    };

    fn fake_owner() -> OwnerId {
        OwnerId::from(fake_user())
    }

    fn fake_user() -> UserId {
        UserId::new(uuid::Uuid::now_v7())
    }

    fn fake_group_owner() -> OwnerId {
        OwnerId::from(GroupId::new(uuid::Uuid::now_v7()))
    }

    type TestAuthz = AuthzContext;

    fn host_owner_context(owner: OwnerId) -> TestAuthz {
        AuthzContext::single_owner(&owner, AuthPath::HostBearer)
    }

    fn host_group_context(subject: UserId, group_owner: OwnerId, role: Role) -> TestAuthz {
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

    struct ForwardedOwnerAccess {
        subject: UserId,
        owner: OwnerId,
        role: ForwarderRole,
        target_probes: AtomicU64,
        membership_probes: AtomicU64,
    }

    impl ForwardedOwnerAccess {
        fn new(subject: UserId, owner: OwnerId) -> Self {
            Self {
                subject,
                owner,
                role: ForwarderRole::Viewer,
                target_probes: AtomicU64::new(0),
                membership_probes: AtomicU64::new(0),
            }
        }
    }

    #[async_trait]
    impl OwnerAccessPort for ForwardedOwnerAccess {
        async fn resolve_roles_for_subject(
            &self,
            subject: UserId,
        ) -> Result<OwnerRoles, AccessError> {
            OwnerRoles::for_subject(subject, [])
        }

        fn is_forwarder(&self, subject: UserId) -> Result<bool, AccessError> {
            Ok(subject == self.subject)
        }

        async fn resolve_forwarded_owner_role(
            &self,
            subject: UserId,
            owner: OwnerId,
        ) -> Result<Option<ForwarderRole>, AccessError> {
            self.target_probes.fetch_add(1, Ordering::SeqCst);
            Ok((subject == self.subject && owner == self.owner).then_some(self.role))
        }

        async fn resolve_group_role(
            &self,
            _subject: UserId,
            _group: GroupId,
        ) -> Result<Option<Role>, AccessError> {
            self.membership_probes.fetch_add(1, Ordering::SeqCst);
            Ok(Some(Role::admin()))
        }
    }

    #[tokio::test]
    async fn forwarded_owner_missing_claim_refuses_own_eager_and_membership_authority() {
        let subject = fake_user();
        let eager = fake_group_owner();
        let membership = fake_group_owner();
        let access = Arc::new(ForwardedOwnerAccess::new(subject, eager));
        let contexts = [
            (
                AuthzContext::for_subject(subject, AuthPath::HostBearer),
                OwnerId::from(subject),
            ),
            (host_group_context(subject, eager, Role::admin()), eager),
            (
                AuthzContext::for_subject(subject, AuthPath::HostBearer),
                membership,
            ),
        ];

        for (context, selected) in contexts {
            let auth = McpEdgeAuth::headless()
                .with_host(Arc::new(StubHostAuth {
                    result: Ok(context),
                }))
                .with_owner_access(access.clone());
            assert!(auth.resolve("host-token", selected).await.is_none());
        }
        assert_eq!(access.target_probes.load(Ordering::SeqCst), 0);
        assert_eq!(access.membership_probes.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn forwarded_owner_resolves_exact_policy_role_and_preserves_authenticated_identity() {
        let subject = fake_user();
        let expires_at = SystemTime::now() + Duration::from_hours(1);
        let scope = ToolScope::Palette(vec![]);
        for owner in [fake_owner(), fake_group_owner()] {
            let access = Arc::new(ForwardedOwnerAccess::new(subject, owner));
            let authz = AuthzContext::from_verified_forwarded_owner(subject, owner)
                .with_expires_at(Some(expires_at))
                .with_auth_epoch(7)
                .with_tool_scope(scope.clone())
                .with_trusted_model_id("acme/runner-v3")
                .unwrap();
            let auth = McpEdgeAuth::headless()
                .with_host(Arc::new(StubHostAuth { result: Ok(authz) }))
                .with_owner_access(access.clone());

            let ctx = auth.resolve("host-token", owner).await.unwrap();
            assert_eq!(ctx.owner, owner);
            assert_eq!(ctx.authz.subject(), Some(subject));
            assert_eq!(ctx.authz.principal(), OwnerId::from(subject));
            assert_eq!(ctx.authz.verified_forwarded_owner(), Some(owner));
            assert_eq!(ctx.authz.role_for_owner(&owner), Some(Role::viewer()));
            assert_eq!(ctx.authz.readable_owners(AccessKind::Goal), vec![owner]);
            assert!(!ctx.authz.can_access_owner(&OwnerId::from(subject)));
            assert_eq!(ctx.authz.expires_at(), Some(expires_at));
            assert_eq!(ctx.authz.auth_epoch(), 7);
            assert_eq!(ctx.authz.tool_scope(), &scope);
            assert_eq!(ctx.authz.trusted_model_id(), Some("acme/runner-v3"));
            assert_eq!(access.target_probes.load(Ordering::SeqCst), 1);
            assert_eq!(access.membership_probes.load(Ordering::SeqCst), 0);
        }
    }

    #[tokio::test]
    async fn forwarded_owner_selection_mismatch_refuses_before_target_probe() {
        let subject = fake_user();
        let claimed = fake_owner();
        let access = Arc::new(ForwardedOwnerAccess::new(subject, claimed));
        let auth = McpEdgeAuth::headless()
            .with_host(Arc::new(StubHostAuth {
                result: Ok(AuthzContext::from_verified_forwarded_owner(
                    subject, claimed,
                )),
            }))
            .with_owner_access(access.clone());

        for selected in [OwnerId::from(subject), fake_group_owner(), fake_owner()] {
            assert!(auth.resolve("host-token", selected).await.is_none());
        }
        assert_eq!(access.target_probes.load(Ordering::SeqCst), 0);
        assert_eq!(access.membership_probes.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn forwarded_owner_requires_registered_owner_and_configured_port_subject() {
        let subject = fake_user();
        let owner = fake_owner();
        let authz = AuthzContext::from_verified_forwarded_owner(subject, owner);
        let without_port = McpEdgeAuth::headless().with_host(Arc::new(StubHostAuth {
            result: Ok(authz.clone()),
        }));
        assert!(without_port.resolve("host-token", owner).await.is_none());

        for access in [
            ForwardedOwnerAccess::new(subject, fake_owner()),
            ForwardedOwnerAccess::new(fake_user(), owner),
        ] {
            let auth = McpEdgeAuth::headless()
                .with_host(Arc::new(StubHostAuth {
                    result: Ok(authz.clone()),
                }))
                .with_owner_access(Arc::new(access));
            assert!(auth.resolve("host-token", owner).await.is_none());
        }
    }

    #[tokio::test]
    async fn edge_keeps_exact_caller_keys_separate_from_the_served_surface() {
        let owner = fake_owner();
        let caller_scope = ToolScope::Palette(vec![ScopeKey::parse("host_write").unwrap()]);
        let served = ServedSurface::Palette(Vec::new());
        let auth = McpEdgeAuth::headless()
            .with_host(Arc::new(StubHostAuth {
                result: Ok(host_owner_context(owner).with_tool_scope(caller_scope.clone())),
            }))
            .with_served_surface(served.clone())
            .with_served_surface(ServedSurface::All);
        let ctx = auth.resolve("host-token", owner).await.unwrap();
        assert_eq!(ctx.authz.tool_scope(), &caller_scope);
        assert_eq!(ctx.authz.served_surface(), &served);
        assert!(ctx.authz.can_access_owner(&owner));
    }

    #[tokio::test]
    async fn edge_cannot_widen_a_hosts_existing_served_restriction() {
        let owner = fake_owner();
        let served = ServedSurface::Palette(vec![ScopeKey::parse("host_write").unwrap()]);
        let auth = McpEdgeAuth::headless().with_host(Arc::new(StubHostAuth {
            result: Ok(host_owner_context(owner).with_served_surface(served.clone())),
        }));
        let ctx = auth.resolve("host-token", owner).await.unwrap();
        assert_eq!(ctx.authz.served_surface(), &served);
        assert_eq!(ctx.authz.tool_scope(), &ToolScope::All);
    }

    #[tokio::test]
    async fn host_material_fails_closed_without_host_authenticator() {
        let auth = McpEdgeAuth::headless();
        assert!(
            auth.resolve("some-opaque-host-token", fake_owner())
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn host_path_accepts_host_bearer_scoped_to_owner() {
        let owner = fake_owner();
        let authz = host_owner_context(owner);
        assert!(
            authz.owner_scope().is_none(),
            "raw compatibility context is not sealed"
        );
        let auth = McpEdgeAuth::headless().with_host(Arc::new(StubHostAuth { result: Ok(authz) }));

        let ctx = auth
            .resolve("host-token", owner)
            .await
            .expect("host resolves");
        assert_eq!(ctx.authz.auth_path(), AuthPath::HostBearer);
        assert_eq!(ctx.owner, owner);
        assert!(
            ctx.authz.owner_scope().is_some(),
            "authenticated edge context is sealed"
        );
    }

    #[tokio::test]
    async fn host_path_accepts_resolved_host_context_for_configured_owner() {
        let owner = fake_owner();
        let authz = host_owner_context(owner);
        let auth = McpEdgeAuth::headless().with_host(Arc::new(StubHostAuth { result: Ok(authz) }));

        let ctx = auth
            .resolve("host-token", owner)
            .await
            .expect("host resolves");
        assert_eq!(ctx.owner, owner);
        assert!(ctx.authz.can_access_owner(&owner));
    }

    #[tokio::test]
    async fn host_context_without_configured_owner_role_is_rejected() {
        let owner = fake_group_owner();
        let authz = AuthzContext::for_subject(fake_user(), AuthPath::HostBearer);
        let auth = McpEdgeAuth::headless().with_host(Arc::new(StubHostAuth { result: Ok(authz) }));

        assert!(auth.resolve("host-token", owner).await.is_none());
    }

    #[tokio::test]
    async fn owner_scoped_ops_deny_host_context_after_role_narrowing() {
        let owner = fake_group_owner();
        let other = fake_group_owner();
        let subject = fake_user();
        let authz = host_group_context(subject, other, Role::admin());
        let auth = McpEdgeAuth::headless().with_host(Arc::new(StubHostAuth { result: Ok(authz) }));
        let ctx = auth
            .resolve("host-token", other)
            .await
            .expect("host resolves");
        let engine = Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests());

        let err = engine
            .get_graph(&ctx.authz, &GetGraphReadRequest { owner })
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
        let auth = McpEdgeAuth::headless().with_host(Arc::new(StubHostAuth { result: Ok(authz) }));

        let ctx = auth
            .resolve("host-token", owner)
            .await
            .expect("host resolves");
        assert!(ctx.authz.can_access_owner(&owner));
        assert!(!ctx.authz.can_access_owner(&other));
        assert!(
            ctx.authz
                .narrowed_to_owner(OwnerId::from(subject))
                .is_none(),
            "a narrowed group witness cannot restore personal authority"
        );
    }

    #[tokio::test]
    async fn host_principal_cannot_narrow_to_unauthorized_personal_or_group() {
        let authorized = fake_group_owner();
        let unauthorized_group = fake_group_owner();
        let unauthorized_personal = fake_owner();
        let subject = fake_user();
        let authz = host_group_context(subject, authorized, Role::editor());
        let auth = McpEdgeAuth::headless().with_host(Arc::new(StubHostAuth { result: Ok(authz) }));

        assert!(
            auth.resolve("host-token", unauthorized_group)
                .await
                .is_none()
        );
        assert!(
            auth.resolve("host-token", unauthorized_personal)
                .await
                .is_none()
        );
        let selected = auth
            .resolve("host-token", authorized)
            .await
            .expect("authorized group narrows");
        assert_eq!(selected.authz.subject(), Some(subject));
        assert!(
            selected
                .authz
                .clone()
                .narrowed_to_owner(authorized)
                .is_some()
        );
        assert!(
            selected
                .authz
                .narrowed_to_owner(OwnerId::from(subject))
                .is_none()
        );
    }

    #[tokio::test]
    async fn host_claiming_system_path_is_rejected() {
        let owner = fake_owner();
        let authz = AuthzContext::single_owner(&owner, AuthPath::System);
        let auth = McpEdgeAuth::headless().with_host(Arc::new(StubHostAuth { result: Ok(authz) }));
        assert!(auth.resolve("host-token", owner).await.is_none());
    }

    #[tokio::test]
    async fn host_identity_without_owner_access_is_rejected() {
        let owner = fake_owner();
        let stranger = fake_owner();
        let authz = host_owner_context(stranger);
        let auth = McpEdgeAuth::headless().with_host(Arc::new(StubHostAuth { result: Ok(authz) }));
        assert!(auth.resolve("host-token", owner).await.is_none());
    }

    /// Host-resolved boundary: a subject with no role at all on the
    /// deployment-configured owner is rejected at resolve, not later at an
    /// engine call. Same fail-closed shape as
    /// `host_context_without_configured_owner_role_is_rejected`, restated
    /// against the `server_resolved` vocabulary the default OIDC path now
    /// always produces.
    #[tokio::test]
    async fn host_resolved_user_without_role_on_configured_owner_is_rejected() {
        let owner = fake_group_owner();
        let authz = AuthzContext::for_subject(fake_user(), AuthPath::HostBearer);
        let auth = McpEdgeAuth::headless().with_host(Arc::new(StubHostAuth { result: Ok(authz) }));

        assert!(auth.resolve("host-token", owner).await.is_none());
    }

    /// A host-resolved subject with only `Viewer` on the configured group
    /// owner resolves (viewer is a real role), but a manage-only engine
    /// operation on that owner must still fail.
    #[tokio::test]
    async fn host_resolved_user_with_viewer_role_cannot_manage_group() {
        let owner = fake_group_owner();
        let subject = fake_user();
        let authz = host_group_context(subject, owner, Role::viewer());
        let auth = McpEdgeAuth::headless().with_host(Arc::new(StubHostAuth { result: Ok(authz) }));

        let ctx = auth
            .resolve("host-token", owner)
            .await
            .expect("viewer role on the configured owner still resolves");
        assert!(ctx.authz.can_access_owner(&owner));

        let group = owner.as_group();
        let engine = Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests());
        let err = engine
            .add_member(&ctx.authz, group, fake_user().into(), Relation::Editor)
            .await
            .expect_err("viewer role must not be able to manage group membership");

        assert_eq!(err.code, ErrorCode::Forbidden);
    }

    /// The edge carries authenticated model provenance through unchanged.
    /// It has no slot of its own to hold — or to contradict — it.
    #[tokio::test]
    async fn a_hosts_trusted_model_id_survives_edge_resolution() {
        let owner = fake_owner();
        let authz = host_owner_context(owner)
            .with_trusted_model_id("acme/runner-v3")
            .expect("a well-formed runner id binds");
        let auth = McpEdgeAuth::headless().with_host(Arc::new(StubHostAuth { result: Ok(authz) }));

        let ctx = auth
            .resolve("host-token", owner)
            .await
            .expect("host resolves");

        assert_eq!(ctx.authz.trusted_model_id(), Some("acme/runner-v3"));
    }

    #[tokio::test]
    async fn a_host_that_binds_no_model_identity_yields_none() {
        let owner = fake_owner();
        let auth = McpEdgeAuth::headless().with_host(Arc::new(StubHostAuth {
            result: Ok(host_owner_context(owner)),
        }));

        let ctx = auth
            .resolve("host-token", owner)
            .await
            .expect("host resolves");

        assert_eq!(ctx.authz.trusted_model_id(), None);
    }
}

//! One dependency on this repository.
//!
//! An out-of-tree host or spiice takes `proxiima` and nothing else from this
//! workspace, so its lockfile cannot pin two revisions of the same tree.
//! Every symbol below is named through `proxiima::…`, `proxiima::spiice::…`
//! or `proxiima::auth::…`; one that falls off the facade fails this file at
//! compile time instead of at a consumer's next pin bump.

use std::sync::Arc;

/// Returns a server-resolved context, the only shape `authenticate` seals.
struct FixedAuthenticator;

#[async_trait::async_trait]
impl proxiima::Authenticator for FixedAuthenticator {
    async fn authenticate(
        &self,
        _credentials: &proxiima::Credentials,
    ) -> Result<proxiima::AuthzContext, proxiima::AuthError> {
        Ok(proxiima::AuthzContext::for_subject(
            proxiima::UserId::new(uuid::Uuid::nil()),
            proxiima::AuthPath::HostBearer,
        ))
    }
}

#[tokio::test]
async fn host_tier_mints_an_owner_scope_through_authenticate() {
    let credentials = proxiima::Credentials::Bearer("token".to_owned());
    let authz = proxiima::authenticate(&FixedAuthenticator, &credentials)
        .await
        .expect("server-resolved roles seal");
    let scope: &proxiima::OwnerScope = authz.owner_scope().expect("authenticate mints the witness");
    assert_eq!(scope.subject(), proxiima::UserId::new(uuid::Uuid::nil()));

    // No ordinary constructor carries the witness.
    let owner = proxiima::company_owner(uuid::Uuid::nil());
    assert!(
        proxiima::AuthzContext::denied_for_owner(&owner)
            .owner_scope()
            .is_none()
    );
}

#[test]
fn host_tier_binds_publication_extensions() {
    let owner = proxiima::company_owner(uuid::Uuid::nil());
    let bound: Result<proxiima::AuthzContext, proxiima::PublicationExtensionsError> =
        proxiima::AuthzContext::denied_for_owner(&owner)
            .with_publication_extensions(proxiima::PublicationExtensions::new());
    let bound = bound.expect("the empty set always binds");
    let value: Option<&proxiima::ExtensionValue> = bound.publication_extensions().get("tenant");
    assert!(value.is_none());
}

/// A pass-through behavior written against the spiice tier; the host-tier
/// names below must resolve to the same trait and types.
#[derive(Debug)]
struct PassThrough;

#[async_trait::async_trait]
impl proxiima::spiice::RequestBehavior for PassThrough {
    async fn handle(
        &self,
        call: proxiima::spiice::ToolCall,
        next: proxiima::spiice::Next<'_>,
    ) -> Result<proxiima::spiice::ToolReply, proxiima::McpToolError> {
        next.run(call).await
    }
}

#[test]
fn both_tiers_name_the_request_behavior_onion() {
    fn host_behavior<T: proxiima::RequestBehavior>() {}
    host_behavior::<PassThrough>();
    let _: Option<(proxiima::ToolCall, proxiima::Next<'static>)> = None;
    // Same reply and visibility types at both tiers.
    let reply: proxiima::ToolReply =
        proxiima::spiice::ToolReply::Content(vec![proxiima::spiice::ToolContent::text(
            "same type",
        )]);
    assert_eq!(
        reply,
        proxiima::ToolReply::Content(vec![proxiima::ToolContent::text("same type")])
    );
    let _: Option<proxiima::ToolDescriptorView<'static>> =
        None::<proxiima::spiice::ToolDescriptorView<'static>>;

    let baseline = proxiima::spiice::SpiiceRegistry::new()
        .try_freeze()
        .expect("empty registry freezes")
        .request_behaviors()
        .len();
    let mut registry = proxiima::spiice::SpiiceRegistry::new();
    registry.add_request_behavior(PassThrough);
    let frozen = registry.try_freeze().expect("registry freezes");
    assert_eq!(frozen.request_behaviors().len(), baseline + 1);
}

struct OwnerOnlyTool;

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct OwnerOnlyArgs {
    #[serde(default)]
    note: Option<String>,
}

#[derive(serde::Serialize, schemars::JsonSchema)]
struct OwnerOnlyOutput {
    note: String,
}

impl proxiima::spiice::Tool for OwnerOnlyTool {
    const TITLE: &'static str = "Test tool";
    const NAME: &'static str = "facade_owner_only";
    const DESCRIPTION: &'static str = "declare tool consts through the facade";
    const ARGV_ACTION_SPECS: &'static [proxiima::spiice::McpArgvActionSpec] = &[];
    const AUDIENCE: proxiima::spiice::McpToolAudience = proxiima::spiice::McpToolAudience::Owner;

    type Args = OwnerOnlyArgs;
    type Output = OwnerOnlyOutput;

    fn call(
        _ctx: proxiima::spiice::ToolCtx,
        args: Self::Args,
    ) -> futures::future::BoxFuture<'static, Result<Self::Output, proxiima::spiice::ToolError>>
    {
        Box::pin(async move {
            Ok(OwnerOnlyOutput {
                note: args.note.unwrap_or_default(),
            })
        })
    }
}

#[test]
fn both_tiers_name_the_tool_declaration_consts() {
    use proxiima::spiice::Tool as _;

    // Host tier: the descriptor fields a host partitions surfaces on.
    fn declaration(
        descriptor: &proxiima::McpToolDescriptor,
    ) -> (
        proxiima::McpToolAudience,
        &'static [proxiima::McpArgvActionSpec],
    ) {
        (descriptor.audience, descriptor.argv_action_specs)
    }

    // Spiice tier: the consts a `Tool` declares.
    assert_eq!(
        OwnerOnlyTool::AUDIENCE,
        proxiima::spiice::McpToolAudience::Owner
    );
    assert!(OwnerOnlyTool::ARGV_ACTION_SPECS.is_empty());

    let frozen = proxiima::spiice::SpiiceRegistry::new()
        .try_freeze()
        .expect("empty registry freezes");
    assert!(
        frozen
            .list_mcp_tools()
            .iter()
            .map(declaration)
            .any(|(audience, _)| audience == proxiima::McpToolAudience::Shared)
    );
}

#[test]
fn host_tier_names_the_mcp_edge() {
    let _edge: Arc<proxiima::McpEdgeAuth> = Arc::new(
        proxiima::McpEdgeAuth::headless().with_served_surface(proxiima::ServedSurface::All),
    );
    let origins = proxiima::OriginAllowlist::parse(["https://proxiima.example.com"])
        .expect("concrete origin");
    assert!(!origins.origins().is_empty());
    let _cors = proxiima::cors_layer(origins);
}

#[test]
fn host_tier_names_the_owner_rls_boot_surface() {
    fn platform_scope(ctx: &proxiima::AppContext) -> proxiima::PgPlatformScope {
        ctx.host().platform_scope_for_host()
    }

    let _: fn(&proxiima::AppContext) -> proxiima::PgPlatformScope = platform_scope;
    let _: fn(_) -> proxiima::StorageError = proxiima::map_err;
    let _: fn(_) -> proxiima::StorageError = proxiima::spiice::map_err;
}

/// `pg_sidecar!` expands inside the invoking crate, so an absolute `sqlx`,
/// `uuid`, or column-type crate path there demands a dependency the facade
/// exists to spare. Every
/// such path goes through the backend's hidden `__private` re-export. In-repo
/// tests cannot prove this by compiling — this package depends on both — so
/// the source is read.
#[test]
fn pg_sidecar_expansion_names_sqlx_and_uuid_through_the_backend() {
    let macros = include_str!("../../storage-pg/src/sidecars/macros.rs");
    for (index, line) in macros.lines().enumerate() {
        let code = line.split("//").next().unwrap_or(line);
        for needle in [
            "::sqlx",
            "::uuid",
            "::rust_decimal",
            "::time",
            "::serde_json",
        ] {
            for (at, _) in code.match_indices(needle) {
                assert!(
                    code[..at].ends_with("$crate::__private"),
                    "macros.rs:{} expands to an absolute `{needle}` path; route it through \
                     `$crate::__private`: {}",
                    index + 1,
                    line.trim()
                );
            }
        }
    }
}

#[cfg(feature = "auth-oidc")]
mod oidc {
    use std::collections::HashMap;
    use std::sync::Arc;

    use proxiima::auth::{
        KeyResolver, OidcAuthConfig, OidcAuthenticator, OidcBinding, OidcBindingRoute,
        OidcBindingSet, OidcBindingSetError, OidcConfigError, OidcRoleShape, OidcSubjectMap,
        OidcSubjectMapError, StaticJwksResolver, SubjectBinding,
    };

    /// One route: the stock single-audience authenticator.
    fn single_route(
        config: OidcAuthConfig,
        keys: Arc<dyn KeyResolver>,
        subjects: OidcSubjectMap,
        owner_access: Arc<dyn proxiima::OwnerAccessPort>,
    ) -> Result<OidcAuthenticator, OidcConfigError> {
        OidcAuthenticator::new(config, keys, subjects, owner_access)
    }

    /// One member of a multi-route `OidcBindingSet`.
    fn binding(
        config: OidcAuthConfig,
        keys: Arc<dyn KeyResolver>,
        subjects: OidcSubjectMap,
        owner_access: Arc<dyn proxiima::OwnerAccessPort>,
    ) -> Result<OidcBinding, OidcConfigError> {
        OidcBinding::with_role_shape(
            config,
            keys,
            subjects,
            owner_access,
            OidcRoleShape::ServerResolved,
        )
    }

    #[test]
    fn auth_tier_names_the_stock_oidc_authenticators() {
        let _keys: Arc<dyn KeyResolver> = Arc::new(StaticJwksResolver::new(HashMap::new()));

        let issuer = "https://issuer.example.com";
        let user = proxiima::UserId::new(uuid::Uuid::nil());
        let mut subjects = OidcSubjectMap::new();
        subjects
            .insert_binding(issuer, "subject-1", SubjectBinding::new(user))
            .expect("fresh entry");
        let duplicate: Result<(), OidcSubjectMapError> = subjects.insert(issuer, "subject-1", user);
        assert!(duplicate.is_err());
        assert_eq!(subjects.resolve(issuer, "subject-1"), Some(user));

        std::hint::black_box(single_route);
        std::hint::black_box(binding);
        let _: fn(&OidcBinding) -> &OidcBindingRoute = OidcBinding::route;

        let empty: Result<OidcBindingSet, OidcBindingSetError> =
            OidcBindingSet::new(Vec::<OidcBinding>::new());
        assert!(matches!(empty, Err(OidcBindingSetError::Empty)));
    }
}

/// An out-of-tree host's `ServerHandler`: it wraps Proxiima's native handler
/// and delegates what the handler's own documentation says to delegate.
#[derive(Clone, Debug)]
struct HostHandler {
    inner: proxiima::DynamicHandler,
}

impl proxiima::rmcp::ServerHandler for HostHandler {
    fn get_info(&self) -> proxiima::rmcp::model::ServerConfig {
        self.inner.get_info()
    }

    fn supported_protocol_versions(
        &self,
    ) -> std::borrow::Cow<'static, [proxiima::rmcp::model::ProtocolVersion]> {
        self.inner.supported_protocol_versions()
    }

    async fn call_tool(
        &self,
        request: proxiima::rmcp::model::CallToolRequestParams,
        context: proxiima::rmcp::service::RequestContext<proxiima::rmcp::service::RoleServer>,
    ) -> Result<proxiima::rmcp::model::CallToolResponse, proxiima::rmcp::model::ErrorData> {
        let (judged, call) = self.inner.call_tool_observed(request, &context);
        let reply = call.await;
        let _key = judged.get();
        reply
    }
}

#[test]
fn host_tier_implements_the_rmcp_server_handler_around_the_dynamic_handler() {
    use proxiima::rmcp::ServerHandler as _;

    let registry = Arc::new(
        proxiima::spiice::SpiiceRegistry::new()
            .try_freeze()
            .expect("empty registry freezes"),
    );
    let host = proxiima::McpToolHost::from_parts(registry, proxiima::SpiiceServices::default());
    let wrapped = HostHandler {
        inner: proxiima::DynamicHandler::new(host),
    };
    assert_eq!(wrapped.get_info().server_info.name, "proxiima");
    assert_eq!(
        wrapped.supported_protocol_versions(),
        wrapped.inner.supported_protocol_versions()
    );
}

/// `proxiima::rmcp` is the `rmcp` the workspace resolves, not a second copy:
/// a bound written against the crate by its own name accepts a type that
/// implements the facade's trait, and the lockfile holds one version.
#[test]
fn the_facade_rmcp_is_the_workspace_rmcp() {
    fn is_workspace_server_handler<T: rmcp::ServerHandler>() {}
    is_workspace_server_handler::<HostHandler>();
    is_workspace_server_handler::<proxiima::DynamicHandler>();

    let lock = include_str!("../../../Cargo.lock");
    let versions: Vec<&str> = lock
        .split("[[package]]")
        .filter(|package| package.lines().any(|line| line.trim() == "name = \"rmcp\""))
        .collect();
    assert_eq!(
        versions.len(),
        1,
        "Cargo.lock must resolve exactly one rmcp; a second copy gives a host two unrelated \
         `ServerHandler` traits"
    );
}

/// The layer stack `McpEdge::router` applies, written with facade names only.
#[tokio::test]
async fn host_tier_builds_the_mcp_edge_layer_stack() {
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use tower::util::ServiceExt as _;

    // The cadence the edge and the runtime config carry is the facade's type.
    let _: fn(&proxiima::McpEdge) -> proxiima::RevalidationConfig = proxiima::McpEdge::revalidation;
    let _: fn(&proxiima::RuntimeConfig) -> proxiima::RevalidationConfig =
        |config| config.stream_revalidation;
    let revalidation = proxiima::RevalidationConfig::default();

    let metadata = proxiima::ResourceServerMetadata {
        public_url: "https://proxiima.example.com".to_owned(),
        authorization_servers: proxiima::AuthorizationServers::Issuers(vec![
            "https://issuer.example.com".to_owned(),
        ]),
        additional_scopes: Vec::new(),
    };
    let hosts = proxiima::HostAllowlist::new(["proxiima.example.com"]);
    let origins = proxiima::OriginAllowlist::parse(["https://proxiima.example.com"])
        .expect("concrete origin");

    let auth: proxiima::McpAuthLayer = proxiima::mcp_auth_layer_with_metadata(
        Arc::new(proxiima::McpEdgeAuth::headless()),
        revalidation,
        Some(&metadata),
    );
    let cors: proxiima::CorsLayer = proxiima::cors_layer(origins);
    let host_guard: proxiima::HostGuardLayer = proxiima::host_guard_layer(hosts);
    let body_limit: proxiima::BodyLimitLayer = proxiima::body_limit_layer(16);

    let router = axum::Router::new()
        .route("/mcp", axum::routing::post(|| async { StatusCode::OK }))
        .layer(auth)
        .layer(cors)
        .layer(host_guard)
        .layer(body_limit);
    let send = |host: &'static str, body: &'static str| {
        let request = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header(header::HOST, host)
            .header(header::CONTENT_LENGTH, body.len())
            .body(Body::from(body))
            .expect("request");
        router.clone().oneshot(request)
    };

    // Outermost: the body cap answers before anything reads the request.
    let too_large = send("proxiima.example.com", "0123456789abcdefg")
        .await
        .expect("infallible");
    assert_eq!(too_large.status(), StatusCode::PAYLOAD_TOO_LARGE);

    // Then the Host guard: an authority outside the allowlist.
    let foreign = send("elsewhere.example.org", "{}")
        .await
        .expect("infallible");
    assert_eq!(foreign.status(), StatusCode::FORBIDDEN);

    // Then bearer auth, whose challenge points at the protected-resource
    // metadata the facade's `ResourceServerMetadata` renders.
    let unauthenticated = send("proxiima.example.com", "{}")
        .await
        .expect("infallible");
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);
    let challenge = unauthenticated
        .headers()
        .get(header::WWW_AUTHENTICATE)
        .expect("challenge")
        .to_str()
        .expect("ascii");
    assert!(
        challenge.contains("https://proxiima.example.com"),
        "{challenge}"
    );

    // The default-sized variant `layered_router_mcp_only` applies.
    let _default_cap =
        axum::Router::<()>::new().layer(axum::middleware::from_fn(proxiima::enforce_body_limit));
}

/// The in-process whole-reply observation needs only the facade dependency.
#[allow(dead_code)]
async fn observed_host_reply(
    host: &proxiima::McpToolHost,
    author: proxiima::McpAuthorContext,
    auth: proxiima::McpAuthContext,
) -> Result<proxiima::ToolReply, proxiima::ToolInvocationError> {
    let (judged, call) = host.call_tool_reply_observed_in_request(
        "core_memory_spaces",
        serde_json::json!({}),
        author,
        Some(auth),
        proxiima::SpiiceServices::default(),
    );
    let reply = call.await;
    let _: Option<proxiima::ScopeKey> = judged.get();
    reply
}

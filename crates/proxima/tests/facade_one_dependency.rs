//! One dependency on this repository.
//!
//! An out-of-tree host or flavor takes `proxima` and nothing else from this
//! workspace, so its lockfile cannot pin two revisions of the same tree.
//! Every symbol below is named through `proxima::…`, `proxima::flavor::…`
//! or `proxima::auth::…`; one that falls off the facade fails this file at
//! compile time instead of at a consumer's next pin bump.

use std::sync::Arc;

/// Returns a server-resolved context, the only shape `authenticate` seals.
struct FixedAuthenticator;

#[async_trait::async_trait]
impl proxima::Authenticator for FixedAuthenticator {
    async fn authenticate(
        &self,
        _credentials: &proxima::Credentials,
    ) -> Result<proxima::AuthzContext, proxima::AuthError> {
        Ok(proxima::AuthzContext::for_subject(
            proxima::UserId::new(uuid::Uuid::nil()),
            proxima::AuthPath::HostBearer,
        ))
    }
}

#[tokio::test]
async fn host_tier_mints_an_owner_scope_through_authenticate() {
    let credentials = proxima::Credentials::Bearer("token".to_owned());
    let authz = proxima::authenticate(&FixedAuthenticator, &credentials)
        .await
        .expect("server-resolved roles seal");
    let scope: &proxima::OwnerScope = authz.owner_scope().expect("authenticate mints the witness");
    assert_eq!(scope.subject(), proxima::UserId::new(uuid::Uuid::nil()));

    // No ordinary constructor carries the witness.
    let owner = proxima::company_owner(uuid::Uuid::nil());
    assert!(
        proxima::AuthzContext::denied_for_owner(&owner)
            .owner_scope()
            .is_none()
    );
}

#[test]
fn host_tier_binds_publication_extensions() {
    let owner = proxima::company_owner(uuid::Uuid::nil());
    let bound: Result<proxima::AuthzContext, proxima::PublicationExtensionsError> =
        proxima::AuthzContext::denied_for_owner(&owner)
            .with_publication_extensions(proxima::PublicationExtensions::new());
    let bound = bound.expect("the empty set always binds");
    let value: Option<&proxima::ExtensionValue> = bound.publication_extensions().get("tenant");
    assert!(value.is_none());
}

/// A pass-through behavior written against the flavor tier; the host-tier
/// names below must resolve to the same trait and types.
#[derive(Debug)]
struct PassThrough;

#[async_trait::async_trait]
impl proxima::flavor::RequestBehavior for PassThrough {
    async fn handle(
        &self,
        call: proxima::flavor::ToolCall,
        next: proxima::flavor::Next<'_>,
    ) -> Result<serde_json::Value, proxima::McpToolError> {
        next.run(call).await
    }
}

#[test]
fn both_tiers_name_the_request_behavior_onion() {
    fn host_behavior<T: proxima::RequestBehavior>() {}
    host_behavior::<PassThrough>();
    let _: Option<(proxima::ToolCall, proxima::Next<'static>)> = None;

    let baseline = proxima::flavor::FlavorRegistry::new()
        .try_freeze()
        .expect("empty registry freezes")
        .request_behaviors()
        .len();
    let mut registry = proxima::flavor::FlavorRegistry::new();
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

impl proxima::flavor::Tool for OwnerOnlyTool {
    const NAME: &'static str = "facade_owner_only";
    const DESCRIPTION: &'static str = "declare tool consts through the facade";
    const ARGV_ACTION_SPECS: &'static [proxima::flavor::McpArgvActionSpec] = &[];
    const AUDIENCE: proxima::flavor::McpToolAudience = proxima::flavor::McpToolAudience::Owner;

    type Args = OwnerOnlyArgs;
    type Output = String;

    fn call(
        _ctx: proxima::flavor::ToolCtx,
        args: Self::Args,
    ) -> futures::future::BoxFuture<'static, Result<Self::Output, proxima::flavor::ToolError>> {
        Box::pin(async move { Ok(args.note.unwrap_or_default()) })
    }
}

#[test]
fn both_tiers_name_the_tool_declaration_consts() {
    use proxima::flavor::Tool as _;

    // Host tier: the descriptor fields a host partitions surfaces on.
    fn declaration(
        descriptor: &proxima::McpToolDescriptor,
    ) -> (
        proxima::McpToolAudience,
        &'static [proxima::McpArgvActionSpec],
    ) {
        (descriptor.audience, descriptor.argv_action_specs)
    }

    // Flavor tier: the consts a `Tool` declares.
    assert_eq!(
        OwnerOnlyTool::AUDIENCE,
        proxima::flavor::McpToolAudience::Owner
    );
    assert!(OwnerOnlyTool::ARGV_ACTION_SPECS.is_empty());

    let frozen = proxima::flavor::FlavorRegistry::new()
        .try_freeze()
        .expect("empty registry freezes");
    assert!(
        frozen
            .list_mcp_tools()
            .iter()
            .map(declaration)
            .any(|(audience, _)| audience == proxima::McpToolAudience::Shared)
    );
}

#[test]
fn host_tier_names_the_mcp_edge() {
    let _edge: Arc<proxima::McpEdgeAuth> =
        Arc::new(proxima::McpEdgeAuth::headless().with_tool_scope(proxima::ToolScope::All));
    let origins =
        proxima::OriginAllowlist::parse(["https://proxima.example.com"]).expect("concrete origin");
    assert!(!origins.origins().is_empty());
    let _cors = proxima::cors_layer(origins);
}

#[test]
fn host_tier_names_the_owner_rls_boot_surface() {
    fn platform_scope(ctx: &proxima::AppContext) -> Option<proxima::PgPlatformScope> {
        ctx.platform_scope_for_host()
    }
    async fn runtime_rls(ctx: &proxima::AppContext) -> Result<(), proxima::StorageError> {
        proxima::assert_runtime_rls(&ctx.clone_pool_for_host(), &["proxima_core"]).await
    }

    let _: fn(&proxima::AppContext) -> Option<proxima::PgPlatformScope> = platform_scope;
    std::hint::black_box(runtime_rls);
    let _: fn(_) -> proxima::StorageError = proxima::map_err;
    let _: fn(_) -> proxima::StorageError = proxima::flavor::map_err;
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

    use proxima::auth::{
        KeyResolver, OidcAuthConfig, OidcAuthenticator, OidcBinding, OidcBindingRoute,
        OidcBindingSet, OidcBindingSetError, OidcConfigError, OidcRoleShape, OidcSubjectMap,
        OidcSubjectMapError, StaticJwksResolver, SubjectBinding,
    };

    /// One route: the stock single-audience authenticator.
    fn single_route(
        config: OidcAuthConfig,
        keys: Arc<dyn KeyResolver>,
        subjects: OidcSubjectMap,
        owner_access: Arc<dyn proxima::OwnerAccessPort>,
    ) -> Result<OidcAuthenticator, OidcConfigError> {
        OidcAuthenticator::new(config, keys, subjects, owner_access)
    }

    /// One member of a multi-route `OidcBindingSet`.
    fn binding(
        config: OidcAuthConfig,
        keys: Arc<dyn KeyResolver>,
        subjects: OidcSubjectMap,
        owner_access: Arc<dyn proxima::OwnerAccessPort>,
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
        let user = proxima::UserId::new(uuid::Uuid::nil());
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

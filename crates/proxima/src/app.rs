use std::sync::Arc;

use axum::Router;
use axum::extract::FromRequestParts;
use axum::http::StatusCode;
use http::request::Parts;
use proxima_blob_s3::CitedBlobStore;
use proxima_core::AuthzContext;
use proxima_core::{Engine, FlavorServiceError, FlavorServices, Owner};
use proxima_mcp_server::McpAuthContext;
use proxima_storage_pg::{PgSidecarRegistryFrozen, PgTuning};
use sqlx::PgPool;

use crate::RuntimeBuilder;
use crate::bundle::FlavorBundle;

/// Static identity for one composed Proxima application binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppInfo {
    pub id: &'static str,
    pub title: &'static str,
    pub version: &'static str,
}

/// Framework surface implemented by host application bundles.
///
/// In tuple apps, the first element is the primary flavor: its
/// [`AppInfo`] identifies the composed binary. Configuration and HTTP
/// mounting fold left-to-right.
pub trait FlavorApp: FlavorBundle {
    fn app_info() -> AppInfo;

    /// Folded left-to-right for tuples; later tuple elements can
    /// override fields set by earlier elements, except
    /// [`RuntimeBuilder::host_state_participant`]: a second registration
    /// refuses boot.
    #[must_use]
    fn configure(builder: RuntimeBuilder) -> RuntimeBuilder {
        builder
    }

    fn mount_http(router: Router, ctx: AppContext) -> Router {
        let _ = ctx;
        router
    }

    /// Build this app's typed runtime services once. Tuple apps merge every
    /// element's set left-to-right and reject duplicate concrete types.
    ///
    /// # Errors
    ///
    /// Returns a duplicate-service error when this app inserts one concrete
    /// type more than once.
    fn services(_ctx: &AppContext) -> Result<FlavorServices, FlavorServiceError> {
        Ok(FlavorServices::default())
    }
}

/// Runtime handles passed to host HTTP mounting code.
#[derive(Clone)]
pub struct AppContext {
    pub engine: Arc<Engine>,
    pub(crate) pool: PgPool,
    pub(crate) platform_scope: Option<proxima_storage_pg::PgPlatformScope>,
    pub(crate) pg_tuning: PgTuning,
    pub(crate) pg_sidecars: Arc<PgSidecarRegistryFrozen>,
    pub(crate) host_state_erase_context: proxima_storage_pg::PgHostStateEraseContext,
    pub blobs: Option<CitedBlobStore>,
    pub owner: Option<Owner>,
    pub(crate) services: FlavorServices,
}

impl AppContext {
    /// Typed services published to this runtime.
    ///
    /// Inside [`FlavorApp::services`] this is the host's own set
    /// ([`crate::RuntimeBuilder::services`]), so a flavor can build on a
    /// host-provided client. Everywhere after — [`FlavorApp::mount_http`]
    /// and the served paths — it is the composed set every tool, request
    /// behavior, and worker sees: host, flavors, and substrate services.
    #[must_use]
    pub fn services(&self) -> &FlavorServices {
        &self.services
    }

    /// Host-only extra-table bridge. Not Flavor SDK.
    ///
    /// Use this inside [`FlavorApp::services`] to construct a flavor-owned
    /// store over tables the host migrates (as `proxima-mcp` does with
    /// `CodeFlavorStore::from_backend_pool_for_host`). Wrap the pool in
    /// that store immediately. Do not put `PgPool` on `FlavorServices`,
    /// do not pass it into a [`proxima_core::tool::Tool`], and do not
    /// run `proxima_core.*` SQL through it — flavor `src/` still has
    /// zero core-table SQL.
    ///
    /// Sidecar-only flavors never call this: they write through
    /// [`proxima_core::Engine`] / [`proxima_core::engine::UnitOfWork`].
    #[must_use]
    pub fn clone_pool_for_host(&self) -> PgPool {
        self.pool.clone()
    }

    /// Host-resolved Postgres query tuning for an extra-table store using
    /// [`Self::clone_pool_for_host`]. Passing this alongside the pool keeps
    /// flavor queries on the canonical environment-independent boot policy.
    #[must_use]
    pub fn pg_tuning_for_host(&self) -> PgTuning {
        self.pg_tuning
    }

    #[must_use]
    pub fn platform_scope_for_host(&self) -> Option<proxima_storage_pg::PgPlatformScope> {
        self.platform_scope.clone()
    }

    /// The sidecar registry this boot froze, for a flavor-owned store that
    /// has to reach the substrate through a storage verb.
    ///
    /// A flavor tearing down one of its own scopes deletes its own rows and
    /// then hands the admissions to `verbs::forget::erase_memory_series`,
    /// which walks THIS registry to reach the sidecars each admission
    /// stamped. Handing over the boot's registry rather than letting the
    /// flavor compose a second one is the point: two compositions can
    /// disagree, and the one the write path used is the only one whose
    /// table list matches what is actually in the rows.
    ///
    /// Cheap to clone — the entries live behind an `Arc`.
    #[must_use]
    pub fn pg_sidecars_for_host(&self) -> PgSidecarRegistryFrozen {
        self.pg_sidecars.as_ref().clone()
    }

    /// The full boot-frozen registry and callback required by host flavors
    /// that invoke physical memory erasure. The context is opaque and grants
    /// no erase authority by itself.
    #[must_use]
    pub fn host_state_erase_context_for_host(&self) -> proxima_storage_pg::PgHostStateEraseContext {
        self.host_state_erase_context.clone()
    }
}

impl std::fmt::Debug for AppContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppContext")
            .field("blobs", &self.blobs)
            .field("owner", &self.owner)
            .field("services", &self.services)
            .finish_non_exhaustive()
    }
}

/// Authorization context extracted from the MCP auth layer extension.
#[derive(Debug, Clone)]
pub struct Authz(pub AuthzContext);

impl<S> FromRequestParts<S> for Authz
where
    S: Send + Sync,
{
    type Rejection = StatusCode;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<McpAuthContext>()
            .map(|ctx| Self(ctx.authz.clone()))
            .ok_or(StatusCode::UNAUTHORIZED)
    }
}

macro_rules! impl_flavor_app_tuple {
    ($first:ident $(, $rest:ident)*) => {
        impl<$first: FlavorApp, $($rest: FlavorApp),*> FlavorApp for ($first, $($rest,)*) {
            fn app_info() -> AppInfo {
                $first::app_info()
            }

            fn configure(builder: RuntimeBuilder) -> RuntimeBuilder {
                let builder = $first::configure(builder);
                $(let builder = $rest::configure(builder);)*
                builder
            }

            fn mount_http(router: Router, ctx: AppContext) -> Router {
                let router = $first::mount_http(router, ctx.clone());
                $(let router = $rest::mount_http(router, ctx.clone());)*
                router
            }

            fn services(ctx: &AppContext) -> Result<FlavorServices, FlavorServiceError> {
                let services = $first::services(ctx)?;
                $(
                    let mut services = services;
                    services.try_extend($rest::services(ctx)?)?;
                )*
                Ok(services)
            }
        }
    };
}

impl_flavor_app_tuple!(A);
impl_flavor_app_tuple!(A, B);
impl_flavor_app_tuple!(A, B, C);
impl_flavor_app_tuple!(A, B, C, D);
impl_flavor_app_tuple!(A, B, C, D, E);
impl_flavor_app_tuple!(A, B, C, D, E, F);
impl_flavor_app_tuple!(A, B, C, D, E, F, G);
impl_flavor_app_tuple!(A, B, C, D, E, F, G, H);

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use proxima_core::{FlavorRegistry, FlavorRegistryError};

    use super::{AppContext, AppInfo, FlavorApp};
    use crate::NamedMigrator;
    use crate::bundle::FlavorBundle;

    #[derive(Debug)]
    struct AlphaService;

    #[derive(Debug)]
    struct BetaService;

    #[derive(Debug)]
    struct SharedService;

    struct AlphaApp;
    struct BetaApp;
    struct DuplicateOne;
    struct DuplicateTwo;

    macro_rules! empty_bundle {
        ($app:ty) => {
            impl FlavorBundle for $app {
                fn register(_registry: &mut FlavorRegistry) -> Result<(), FlavorRegistryError> {
                    Ok(())
                }

                fn migrators() -> Vec<NamedMigrator> {
                    Vec::new()
                }
            }
        };
    }

    empty_bundle!(AlphaApp);
    empty_bundle!(BetaApp);
    empty_bundle!(DuplicateOne);
    empty_bundle!(DuplicateTwo);

    impl FlavorApp for AlphaApp {
        fn app_info() -> AppInfo {
            AppInfo {
                id: "alpha",
                title: "Alpha",
                version: "1",
            }
        }

        fn services(
            _ctx: &AppContext,
        ) -> Result<proxima_core::FlavorServices, proxima_core::FlavorServiceError> {
            Ok(proxima_core::FlavorServices::with(AlphaService))
        }
    }

    impl FlavorApp for BetaApp {
        fn app_info() -> AppInfo {
            AppInfo {
                id: "beta",
                title: "Beta",
                version: "1",
            }
        }

        fn services(
            _ctx: &AppContext,
        ) -> Result<proxima_core::FlavorServices, proxima_core::FlavorServiceError> {
            Ok(proxima_core::FlavorServices::with(BetaService))
        }
    }

    impl FlavorApp for DuplicateOne {
        fn app_info() -> AppInfo {
            AppInfo {
                id: "duplicate-one",
                title: "Duplicate One",
                version: "1",
            }
        }

        fn services(
            _ctx: &AppContext,
        ) -> Result<proxima_core::FlavorServices, proxima_core::FlavorServiceError> {
            Ok(proxima_core::FlavorServices::with(SharedService))
        }
    }

    impl FlavorApp for DuplicateTwo {
        fn app_info() -> AppInfo {
            AppInfo {
                id: "duplicate-two",
                title: "Duplicate Two",
                version: "1",
            }
        }

        fn services(
            _ctx: &AppContext,
        ) -> Result<proxima_core::FlavorServices, proxima_core::FlavorServiceError> {
            Ok(proxima_core::FlavorServices::with(SharedService))
        }
    }

    fn context() -> AppContext {
        AppContext {
            platform_scope: None,
            engine: Arc::new(proxima_core::Engine::new(
                FlavorRegistry::new().freeze_or_panic_for_tests(),
            )),
            pool: sqlx::PgPool::connect_lazy_with(sqlx::postgres::PgConnectOptions::new()),
            pg_tuning: proxima_storage_pg::PgTuning::default(),
            pg_sidecars: Arc::default(),
            host_state_erase_context:
                proxima_storage_pg::PgHostStateEraseContext::for_surfaces_for_tests(
                    proxima_core::owner_inverse::OwnerSurfaces::from_surfaces(Vec::new()),
                )
                .expect("empty fixture registry has no host lifecycle tables"),
            blobs: None,
            owner: None,
            services: proxima_core::FlavorServices::default(),
        }
    }

    #[tokio::test]
    async fn singleton_tuple_delegates_identity_and_services() {
        tokio::task::yield_now().await;
        let ctx = context();

        assert_eq!(<(AlphaApp,) as FlavorApp>::app_info().id, "alpha");
        let services = <(AlphaApp,) as FlavorApp>::services(&ctx).unwrap();
        assert!(services.get::<AlphaService>().is_some());
    }

    #[tokio::test]
    async fn tuple_composes_every_service() {
        tokio::task::yield_now().await;
        let services = <(AlphaApp, BetaApp) as FlavorApp>::services(&context()).unwrap();

        assert!(services.get::<AlphaService>().is_some());
        assert!(services.get::<BetaService>().is_some());
    }

    #[tokio::test]
    async fn tuple_rejects_duplicate_service_types() {
        tokio::task::yield_now().await;
        let err = <(DuplicateOne, DuplicateTwo) as FlavorApp>::services(&context()).unwrap_err();

        assert!(matches!(
            err,
            proxima_core::FlavorServiceError::DuplicateService { type_name }
                if type_name == std::any::type_name::<SharedService>()
        ));
    }
}

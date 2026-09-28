use axum::Router;
use axum::extract::FromRequestParts;
use axum::http::StatusCode;
use http::request::Parts;
use proxima_core::AuthzContext;
use proxima_core::{FlavorServiceError, FlavorServices};
use proxima_mcp_server::McpAuthContext;

use crate::bundle::FlavorBundle;
use crate::{ProximaHost, RuntimeBuilder};

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

    /// The app's defaults: the lowest configuration layer, under the
    /// environment and the host's own builder calls. Every feature starts
    /// when its config is present, so a flavor that wants MCP by default
    /// sets [`RuntimeBuilder::mcp_bind`] here.
    ///
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

/// What [`FlavorApp::services`] and [`FlavorApp::mount_http`] receive: the
/// booted runtime's [`ProximaHost`], reached as on `BuiltProxima` and
/// `RunningProxima`.
#[derive(Clone)]
pub struct AppContext {
    pub(crate) host: ProximaHost,
}

impl AppContext {
    /// The booted runtime's host accessors. Inside [`FlavorApp::services`]
    /// its [`ProximaHost::services`] is the host's own set; everywhere after,
    /// the composed one.
    #[must_use]
    pub const fn host(&self) -> &ProximaHost {
        &self.host
    }
}

impl std::fmt::Debug for AppContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppContext")
            .field("host", &self.host)
            .finish()
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
    struct SharedService;

    struct AlphaApp;
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
            host: crate::ProximaHost::for_tests(Arc::new(proxima_core::Engine::new(
                FlavorRegistry::new().freeze_or_panic_for_tests(),
            ))),
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

use std::sync::Arc;

use axum::Router;
use axum::extract::FromRequestParts;
use axum::http::StatusCode;
use http::request::Parts;
use proxima_blob_s3::CitedBlobStore;
use proxima_core::AuthzContext;
use proxima_core::mcp::McpToolExtensions;
use proxima_core::{Engine, Owner};
use proxima_mcp_server::McpAuthContext;
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
    /// override fields set by earlier elements.
    #[must_use]
    fn configure(builder: RuntimeBuilder) -> RuntimeBuilder {
        builder
    }

    fn mount_http(router: Router, ctx: AppContext) -> Router {
        let _ = ctx;
        router
    }

    #[must_use]
    fn mcp_tool_extensions(_ctx: &AppContext) -> McpToolExtensions {
        McpToolExtensions::default()
    }
}

/// Runtime handles passed to host HTTP mounting code.
#[derive(Clone)]
pub struct AppContext {
    pub engine: Arc<Engine>,
    pub(crate) pool: PgPool,
    pub blobs: Option<CitedBlobStore>,
    pub owner: Owner,
}

impl AppContext {
    /// Host-only bridge for composing backend-owned services.
    #[doc(hidden)]
    #[must_use]
    pub fn clone_pool_for_host(&self) -> PgPool {
        self.pool.clone()
    }
}

impl std::fmt::Debug for AppContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppContext")
            .field("blobs", &self.blobs)
            .field("owner", &self.owner)
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
    ($first:ident, $($rest:ident),+) => {
        impl<$first: FlavorApp, $($rest: FlavorApp),+> FlavorApp for ($first, $($rest,)+) {
            fn app_info() -> AppInfo {
                $first::app_info()
            }

            fn configure(builder: RuntimeBuilder) -> RuntimeBuilder {
                let mut builder = $first::configure(builder);
                $(builder = $rest::configure(builder);)+
                builder
            }

            fn mount_http(router: Router, ctx: AppContext) -> Router {
                let mut router = $first::mount_http(router, ctx.clone());
                $(router = $rest::mount_http(router, ctx.clone());)+
                router
            }
        }
    };
}

impl_flavor_app_tuple!(A, B);
impl_flavor_app_tuple!(A, B, C);
impl_flavor_app_tuple!(A, B, C, D);
impl_flavor_app_tuple!(A, B, C, D, E);
impl_flavor_app_tuple!(A, B, C, D, E, F);
impl_flavor_app_tuple!(A, B, C, D, E, F, G);
impl_flavor_app_tuple!(A, B, C, D, E, F, G, H);

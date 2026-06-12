use std::sync::Arc;

use axum::Router;
use proxima_blob_s3::CitedBlobStore;
use proxima_core::{Engine, Owner};
use sqlx::PgPool;

use crate::{FlavorBundle, RuntimeBuilder};

/// Static identity for one composed Proxima application binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppInfo {
    pub id: &'static str,
    pub title: &'static str,
    pub version: &'static str,
}

/// Framework surface implemented by host application bundles.
pub trait FlavorApp: FlavorBundle {
    fn app_info() -> AppInfo;

    #[must_use]
    fn configure(builder: RuntimeBuilder) -> RuntimeBuilder {
        builder
    }

    fn mount_http(router: Router, ctx: AppContext) -> Router {
        let _ = ctx;
        router
    }
}

/// Runtime handles passed to host HTTP mounting code.
#[derive(Clone)]
pub struct AppContext {
    pub engine: Arc<Engine>,
    pub pool: PgPool,
    pub blobs: Option<CitedBlobStore>,
    pub owner: Owner,
}

impl std::fmt::Debug for AppContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppContext")
            .field("pool", &self.pool)
            .field("blobs", &self.blobs)
            .field("owner", &self.owner)
            .finish_non_exhaustive()
    }
}

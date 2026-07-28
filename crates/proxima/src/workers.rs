//! Flavor-contributed background workers: spawned by the serving
//! runtime after boot, joined at shutdown.

use std::sync::Arc;

use proxima_core::Engine;
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;

/// Runtime handles passed to [`FlavorBundle::spawn_workers`].
///
/// The `cancel` token is the serving runtime's shutdown signal: every
/// worker spawned from this context MUST observe it and terminate when
/// it is cancelled.
///
/// [`FlavorBundle::spawn_workers`]: crate::flavor::FlavorBundle::spawn_workers
#[derive(Clone)]
pub struct FlavorWorkerContext {
    pub engine: Arc<Engine>,
    pub(crate) pool: PgPool,
    pub cancel: CancellationToken,
}

impl FlavorWorkerContext {
    /// Host-only bridge for composing backend-owned services.
    #[doc(hidden)]
    #[must_use]
    pub fn clone_pool_for_host(&self) -> PgPool {
        self.pool.clone()
    }
}

impl std::fmt::Debug for FlavorWorkerContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FlavorWorkerContext")
            .field("cancel", &self.cancel)
            .finish_non_exhaustive()
    }
}

/// One spawned flavor worker: a join handle plus the name used in
/// shutdown logging when its join fails.
#[derive(Debug)]
pub struct FlavorWorker {
    pub name: &'static str,
    pub handle: tokio::task::JoinHandle<()>,
}

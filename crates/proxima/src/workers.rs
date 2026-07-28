//! Flavor-contributed background workers: spawned by the serving
//! runtime after boot, joined at shutdown.

use std::sync::Arc;

use proxima_core::Engine;
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;

/// Runtime handles passed to [`FlavorBundle::spawn_workers`].
///
/// The `cancel` token is an observation of the serving runtime's
/// shutdown, not a control over it: it is a child of the runtime's own
/// token, so it fires when the runtime shuts down, but cancelling it
/// only reaches this context's clones. Every worker spawned from this
/// context MUST observe it and terminate when it is cancelled.
///
/// [`FlavorBundle::spawn_workers`]: crate::flavor::FlavorBundle::spawn_workers
#[derive(Clone)]
pub struct FlavorWorkerContext {
    pub engine: Arc<Engine>,
    pub(crate) pool: PgPool,
    pub cancel: CancellationToken,
}

impl FlavorWorkerContext {
    /// Test-only context for exercising a bundle's `spawn_workers`
    /// without booting the serving runtime. The backend pool is a lazy
    /// pool that never connects; workers that touch it are integration
    /// territory.
    #[cfg(any(test, feature = "testkit", debug_assertions))]
    #[must_use]
    pub fn new_for_tests(engine: Arc<Engine>, cancel: CancellationToken) -> Self {
        Self {
            engine,
            pool: PgPool::connect_lazy_with(sqlx::postgres::PgConnectOptions::new()),
            cancel,
        }
    }

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

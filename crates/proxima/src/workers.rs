//! Flavor-contributed background workers: spawned by the serving
//! runtime after boot, joined at shutdown.

use std::sync::Arc;

use proxima_core::{Engine, FlavorServices};
use tokio_util::sync::CancellationToken;

/// Runtime handles passed to [`FlavorBundle::spawn_workers`].
///
/// The `cancel` token is an observation of the serving runtime's
/// shutdown, not a control over it: it is a child of the runtime's own
/// token, so it fires when the runtime shuts down, but cancelling it
/// only reaches this context's clones. Every worker spawned from this
/// context MUST observe it and terminate when it is cancelled.
///
/// `service::<T>()` resolves from the same composed service set used by MCP
/// and REST tools. `CitedBlobService` is absent unless the host configured
/// S3, so a worker that needs it must fail its job typed rather than silently
/// no-op. Every
/// [`CitedBlobPort`](proxima_core::storage_ports::CitedBlobPort) method
/// takes an [`AuthzContext`](proxima_core::AuthzContext) and an
/// `OwnerRef` that the worker supplies per job: a worker has no request
/// to inherit them from, and the port's own re-check is defense in
/// depth, not the caller-facing gate an MCP tool provides.
/// [`read_url`](proxima_core::storage_ports::CitedBlobPort::read_url)
/// returns a presigned URL and never the bucket or object key.
///
/// [`FlavorBundle::spawn_workers`]: crate::flavor::FlavorBundle::spawn_workers
#[derive(Clone)]
pub struct FlavorWorkerContext {
    pub engine: Arc<Engine>,
    pub cancel: CancellationToken,
    pub(crate) services: FlavorServices,
}

impl FlavorWorkerContext {
    /// Test-only context for exercising a bundle's `spawn_workers`
    /// without booting the serving runtime. Services start empty; attach the
    /// exact test set with [`with_services`](Self::with_services).
    #[cfg(any(test, feature = "testkit", debug_assertions))]
    #[must_use]
    pub fn new_for_tests(engine: Arc<Engine>, cancel: CancellationToken) -> Self {
        Self {
            engine,
            cancel,
            services: FlavorServices::default(),
        }
    }

    /// Attach the composed service set to a test context.
    #[cfg(any(test, feature = "testkit", debug_assertions))]
    #[must_use]
    pub fn with_services(mut self, services: FlavorServices) -> Self {
        self.services = services;
        self
    }

    #[must_use]
    pub fn service<T>(&self) -> Option<Arc<T>>
    where
        T: Send + Sync + 'static,
    {
        self.services.get::<T>()
    }
}

impl std::fmt::Debug for FlavorWorkerContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FlavorWorkerContext")
            .field("cancel", &self.cancel)
            .field("services", &self.services)
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

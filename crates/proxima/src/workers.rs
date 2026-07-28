//! Flavor-contributed background workers: spawned by the serving
//! runtime after boot, joined at shutdown.

use std::sync::Arc;

use proxima_core::Engine;
use proxima_core::storage_ports::CitedBlobService;
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
/// `blobs` is the host-wired cited-blob lane — the same service
/// `core_upload` resolves from its MCP tool extensions. It is `None`
/// unless the host configured S3, so a worker that needs it must fail
/// its job typed rather than silently no-op. Every
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
    pub(crate) pool: PgPool,
    pub cancel: CancellationToken,
    pub blobs: Option<CitedBlobService>,
}

impl FlavorWorkerContext {
    /// Test-only context for exercising a bundle's `spawn_workers`
    /// without booting the serving runtime. The backend pool is a lazy
    /// pool that never connects; workers that touch it are integration
    /// territory. `blobs` starts empty — attach one with
    /// [`with_blobs`](Self::with_blobs).
    ///
    /// Constructing the lazy pool requires a Tokio context, so call this
    /// from `#[tokio::test]`, not `#[test]`.
    #[cfg(any(test, feature = "testkit", debug_assertions))]
    #[must_use]
    pub fn new_for_tests(engine: Arc<Engine>, cancel: CancellationToken) -> Self {
        Self {
            engine,
            pool: PgPool::connect_lazy_with(sqlx::postgres::PgConnectOptions::new()),
            cancel,
            blobs: None,
        }
    }

    /// Attach a cited-blob service to a test context — typically the
    /// flavor's own `impl CitedBlobPort` fake, so a worker that reads
    /// artefacts can be exercised without S3.
    ///
    /// A builder rather than a `new_for_tests` parameter: the two-arg
    /// form is the documented entry point for out-of-tree flavor
    /// authors, and widening it would break every one of them to serve
    /// the minority that needs a blob service.
    #[cfg(any(test, feature = "testkit", debug_assertions))]
    #[must_use]
    pub fn with_blobs(mut self, blobs: CitedBlobService) -> Self {
        self.blobs = Some(blobs);
        self
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
            .field("blobs", &self.blobs)
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

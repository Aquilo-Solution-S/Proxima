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
/// and REST tools. `CitedBlobService` and `CitedBlobReadService` are absent
/// unless the host configured S3, so a worker that needs either must fail its
/// job typed rather than silently no-op. A queued worker stores only a
/// [`DelegationId`](proxima_core::DelegationId), resolves the shared
/// [`DelegatedAuthorityService`](proxima_core::DelegatedAuthorityService),
/// and redeems a non-cloneable
/// [`DelegatedPhase`](proxima_core::DelegatedPhase) at job claim and at each
/// subsequent phase boundary. The service is absent when the host has no
/// authenticator.
/// Delegated-capable Engine methods and the bound `CitedBlobService` /
/// `CitedBlobReadService` accept that phase; raw delegated `AuthzContext`
/// values are rejected. `read_url` returns a presigned URL and never the
/// bucket or object key. `collect_verified` additionally requires a non-zero
/// byte ceiling and returns bytes only after length, BLAKE3, and SHA-256
/// verification. Owner reconciliation is not delegated-capable.
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

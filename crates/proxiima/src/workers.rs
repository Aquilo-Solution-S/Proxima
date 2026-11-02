//! Spiice-contributed background workers: spawned by the serving
//! runtime after boot, joined at shutdown.

use std::sync::Arc;

use proxiima_core::{Engine, SpiiceServices};
use tokio_util::sync::CancellationToken;

/// Runtime handles passed to [`SpiiceBundle::spawn_workers`].
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
/// [`DelegationId`](proxiima_core::DelegationId), resolves the shared
/// [`DelegatedAuthorityService`](proxiima_core::DelegatedAuthorityService),
/// and redeems a non-cloneable
/// [`DelegatedPhase`](proxiima_core::DelegatedPhase) at job claim and at each
/// subsequent phase boundary. The service is absent when the host has no
/// authenticator.
/// Delegated-capable Engine methods and the bound `CitedBlobService` /
/// `CitedBlobReadService` accept that phase; raw delegated `AuthzContext`
/// values are rejected. `read_url` returns a presigned URL and never the
/// bucket or object key. `collect_verified` additionally requires a non-zero
/// byte ceiling and returns bytes only after length, BLAKE3, and SHA-256
/// verification. Owner reconciliation is not delegated-capable.
///
/// [`SpiiceBundle::spawn_workers`]: crate::spiice::SpiiceBundle::spawn_workers
#[derive(Clone)]
pub struct SpiiceWorkerContext {
    pub engine: Arc<Engine>,
    pub cancel: CancellationToken,
    pub(crate) services: SpiiceServices,
}

impl SpiiceWorkerContext {
    /// Test-only context for exercising a bundle's `spawn_workers`
    /// without booting the serving runtime. Services start empty; attach the
    /// exact test set with [`with_services`](Self::with_services).
    #[cfg(any(test, feature = "testkit", debug_assertions))]
    #[must_use]
    pub fn new_for_tests(engine: Arc<Engine>, cancel: CancellationToken) -> Self {
        Self {
            engine,
            cancel,
            services: SpiiceServices::default(),
        }
    }

    /// Attach the composed service set to a test context.
    #[cfg(any(test, feature = "testkit", debug_assertions))]
    #[must_use]
    pub fn with_services(mut self, services: SpiiceServices) -> Self {
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

impl std::fmt::Debug for SpiiceWorkerContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpiiceWorkerContext")
            .field("cancel", &self.cancel)
            .field("services", &self.services)
            .finish_non_exhaustive()
    }
}

/// One spawned spiice worker: a join handle plus the name used in
/// shutdown logging when its join fails.
#[derive(Debug)]
pub struct SpiiceWorker {
    pub name: &'static str,
    pub handle: tokio::task::JoinHandle<()>,
}

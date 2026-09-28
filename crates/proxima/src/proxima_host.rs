//! The booted runtime's host accessors, defined once.

use std::sync::Arc;

use proxima_blob_s3::CitedBlobStore;
use proxima_core::storage_ports::publication::{OriginScope, PublicationOriginEligibilityPort};
use proxima_core::{Engine, FlavorRegistryFrozen, FlavorServiceError, FlavorServices, Owner};
use proxima_storage_pg::{
    PgHostStateEraseContext, PgPlatformScope, PgSidecarRegistryFrozen, PgTuning,
};
use sqlx::PgPool;

use crate::CoreMcpTools;

/// Host accessors of one booted runtime, reached the same way from every
/// handle on it: [`crate::BuiltProxima::host`], [`crate::RunningProxima::host`]
/// and [`crate::AppContext::host`].
///
/// Host API, not Flavor SDK. It carries no authority: the boot-held
/// [`crate::SystemAuthority`] and [`crate::HostStateMaintenanceAuthority`] stay
/// on `BuiltProxima` / `RunningProxima`, so a flavor handed an `AppContext`
/// cannot reach either. Cheap to clone — every handle is shared.
#[derive(Clone)]
pub struct ProximaHost {
    pub(crate) engine: Arc<Engine>,
    pub(crate) registry: Arc<FlavorRegistryFrozen>,
    pub(crate) pool: PgPool,
    pub(crate) platform_scope: Option<PgPlatformScope>,
    pub(crate) pg_tuning: PgTuning,
    pub(crate) pg_sidecars: Arc<PgSidecarRegistryFrozen>,
    pub(crate) erase_context: PgHostStateEraseContext,
    pub(crate) origin_scope: OriginScope,
    pub(crate) publication_origin_eligibility: Arc<dyn PublicationOriginEligibilityPort>,
    pub(crate) blobs: Option<CitedBlobStore>,
    pub(crate) owner: Option<Owner>,
    pub(crate) services: FlavorServices,
}

impl ProximaHost {
    #[must_use]
    pub const fn engine(&self) -> &Arc<Engine> {
        &self.engine
    }

    /// The boot-frozen flavor registry.
    #[must_use]
    pub const fn registry(&self) -> &Arc<FlavorRegistryFrozen> {
        &self.registry
    }

    /// The S3 cited-blob store; `None` unless S3 is configured.
    #[must_use]
    pub const fn blobs(&self) -> Option<&CitedBlobStore> {
        self.blobs.as_ref()
    }

    /// The configured engine `Owner`, if any.
    #[must_use]
    pub const fn owner(&self) -> Option<Owner> {
        self.owner
    }

    /// Typed services published to this runtime.
    ///
    /// Inside [`crate::FlavorApp::services`] this is the host's own set
    /// ([`crate::RuntimeBuilder::services`]), so a flavor can build on a
    /// host-provided client. Everywhere after — [`crate::FlavorApp::mount_http`],
    /// the served paths and the booted handles — it is the composed set every
    /// tool, request behavior, and worker sees: host, flavors, and substrate
    /// services.
    #[must_use]
    pub const fn services(&self) -> &FlavorServices {
        &self.services
    }

    /// Host-only extra-table bridge. Not Flavor SDK.
    ///
    /// Use this inside [`crate::FlavorApp::services`] to construct a
    /// flavor-owned store over tables the host migrates (as `proxima-mcp`
    /// does with `CodeFlavorStore::from_backend_pool_for_host`). Wrap the pool
    /// in that store immediately. Do not put `PgPool` on `FlavorServices`, do
    /// not pass it into a [`proxima_core::tool::Tool`], and do not run
    /// `proxima_core.*` SQL through it — flavor `src/` still has zero
    /// core-table SQL.
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
    pub const fn pg_tuning_for_host(&self) -> PgTuning {
        self.pg_tuning
    }

    /// The platform scope boot validated; `None` without a platform URL.
    #[must_use]
    pub fn platform_scope_for_host(&self) -> Option<PgPlatformScope> {
        self.platform_scope.clone()
    }

    /// The sidecar registry this boot froze, for a host-owned store that
    /// has to reach the substrate through a storage verb.
    ///
    /// Handing over the boot's registry rather than composing a second one
    /// is the point: two compositions can disagree, and the one the write
    /// path used is the only one whose table list matches what is actually
    /// in the rows. A flavor erasing its own series does not need it:
    /// `UnitOfWork::erase_own_series` runs on the Engine's own.
    ///
    /// Cheap to clone — the entries live behind an `Arc`.
    #[must_use]
    pub fn pg_sidecars_for_host(&self) -> PgSidecarRegistryFrozen {
        self.pg_sidecars.as_ref().clone()
    }

    /// The full boot-frozen registry and callback required by a host that
    /// invokes physical memory erasure on its own storage. The context is
    /// opaque and grants no erase authority by itself. A flavor uses
    /// `UnitOfWork::erase_own_series`, which carries the Engine's own.
    #[must_use]
    pub fn host_state_erase_context_for_host(&self) -> PgHostStateEraseContext {
        self.erase_context.clone()
    }

    /// This installation's minted identity, for a host wiring its own
    /// publisher or cleaner against another broker.
    #[must_use]
    pub const fn origin_scope_for_host(&self) -> OriginScope {
        self.origin_scope
    }

    /// Narrow host-only provenance check for intake and backlog re-offer.
    /// The port reveals only eligible/ineligible for one typed owner/Fact
    /// pair, never Fact contents or a general core read capability.
    #[must_use]
    pub fn publication_origin_eligibility_for_host(
        &self,
    ) -> Arc<dyn PublicationOriginEligibilityPort> {
        self.publication_origin_eligibility.clone()
    }

    /// The registry's tools over this runtime's composed service set.
    #[must_use]
    pub fn core_mcp_tools(&self) -> CoreMcpTools {
        CoreMcpTools::new(
            self.registry.clone(),
            self.engine.clone(),
            self.services.clone(),
        )
    }

    /// Same as [`Self::core_mcp_tools`], with a per-request
    /// [`FlavorServices`] bag merged onto the boot set (`try_extend`).
    ///
    /// # Errors
    ///
    /// [`FlavorServiceError::DuplicateService`] when `request` repeats a
    /// type already in the boot bag.
    pub fn core_mcp_tools_with_request_services(
        &self,
        request: FlavorServices,
    ) -> Result<CoreMcpTools, FlavorServiceError> {
        let mut services = self.services.clone();
        services.try_extend(request)?;
        Ok(CoreMcpTools::new(
            self.registry.clone(),
            self.engine.clone(),
            services,
        ))
    }

    /// Test-only backend pool access for integration fixtures.
    #[cfg(any(test, feature = "testkit", debug_assertions))]
    #[must_use]
    pub const fn pool_for_tests(&self) -> &PgPool {
        &self.pool
    }

    /// A host over a lazy pool and an unbooted engine, for unit tests that
    /// exercise composition without storage.
    #[cfg(test)]
    pub(crate) fn for_tests(engine: Arc<Engine>) -> Self {
        Self {
            registry: Arc::new(engine.registry().clone()),
            engine,
            pool: PgPool::connect_lazy_with(sqlx::postgres::PgConnectOptions::new()),
            platform_scope: None,
            pg_tuning: PgTuning::default(),
            pg_sidecars: Arc::default(),
            erase_context: PgHostStateEraseContext::for_surfaces_for_tests(
                proxima_core::owner_inverse::OwnerSurfaces::from_surfaces(Vec::new()),
            )
            .expect("empty fixture registry has no host lifecycle tables"),
            origin_scope: OriginScope::new(uuid::Uuid::nil()),
            publication_origin_eligibility: Arc::new(IneligibleForTests),
            blobs: None,
            owner: None,
            services: FlavorServices::default(),
        }
    }
}

impl std::fmt::Debug for ProximaHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProximaHost")
            .field("pool", &self.pool)
            .field("blobs", &self.blobs)
            .field("owner", &self.owner)
            .field("services", &self.services)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
struct IneligibleForTests;

#[cfg(test)]
#[async_trait::async_trait]
impl PublicationOriginEligibilityPort for IneligibleForTests {
    async fn check_committed(
        &self,
        _original_owner: Owner,
        _fact_id: proxima_core::MemoryId,
    ) -> Result<
        proxima_core::storage_ports::publication::PublicationOriginEligibility,
        proxima_core::StorageError,
    > {
        Ok(proxima_core::storage_ports::publication::PublicationOriginEligibility::Ineligible)
    }

    async fn check_in_transaction(
        &self,
        _tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        _original_owner: Owner,
        _fact_id: proxima_core::MemoryId,
    ) -> Result<
        proxima_core::storage_ports::publication::PublicationOriginEligibility,
        proxima_core::StorageError,
    > {
        Ok(proxima_core::storage_ports::publication::PublicationOriginEligibility::Ineligible)
    }
}

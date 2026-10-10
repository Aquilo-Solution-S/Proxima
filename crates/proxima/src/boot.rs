//! Storage and engine boot: connect, migrate, compose, start.
//!
//! Internal. Hosts boot through [`crate::Proxima`] / [`crate::RuntimeBuilder`];
//! this module turns one resolved [`RuntimeConfig`] plus its [`RuntimeParts`]
//! into a started engine and the handles the runtime keeps from it.

use std::sync::Arc;

use proxima_core::authz::SystemAuthority;
use proxima_core::llm::{EmbeddingClient, EmbeddingRouter};
use proxima_core::storage_ports::publication::{OriginScope, PublicationOriginEligibilityPort};
use proxima_core::{
    ColdObjectStore, Engine, EngineHandle, FlavorRegistry, StorageError, composed_schema_names,
};
use proxima_storage_pg::{
    PgSidecarRegistry, PgSidecarRegistryFrozen, PgStorage, register_core_pg_sidecars,
};
use sqlx::PgPool;

use crate::bundle::FlavorBundle;
use crate::runtime_grants::{self, RuntimeGrants};
use crate::{
    CitedBlobStore, MigrationError, NamedMigrator, ProximaError, RuntimeConfig, RuntimeParts,
    preflight_without_migrations, run_core_and_flavor_migrations,
};

/// A started engine plus the handles the runtime keeps from its boot.
pub(crate) struct Booted {
    pub(crate) engine: Arc<Engine>,
    pub(crate) system_authority: SystemAuthority,
    pub(crate) host_state_maintenance_authority:
        Option<proxima_core::engine::HostStateMaintenanceAuthority>,
    pub(crate) delegation_runtime_authority: proxima_core::DelegationRuntimeAuthority,
    pub(crate) handle: EngineHandle,
    pub(crate) pool: PgPool,
    pub(crate) platform_scope: Option<proxima_storage_pg::PgPlatformScope>,
    pub(crate) registry: Arc<proxima_core::FlavorRegistryFrozen>,
    pub(crate) pg_sidecars: Arc<PgSidecarRegistryFrozen>,
    pub(crate) erase_context: proxima_storage_pg::PgHostStateEraseContext,
    pub(crate) publication_origin_eligibility: Arc<dyn PublicationOriginEligibilityPort>,
    /// This installation's minted identity, read once from the same
    /// database that answers `publication_origin_eligibility`.
    ///
    /// Both halves of the retained-copy rule have to come from one place:
    /// the publisher stamps this on every broker message and the cleaner
    /// refuses any message not carrying it, so "no origin row" is only ever
    /// read as a revocation over copies this database actually published.
    pub(crate) origin_scope: OriginScope,
    pub(crate) blobs: Option<CitedBlobStore>,
    /// The host-only drain over captured publication records.
    ///
    /// Deliberately absent from `StoragePorts`, `Engine` and `ToolCtx` — a
    /// flavor that could claim an outbox record could delay or suppress an
    /// export — so only the publisher task the runtime starts ever sees it.
    /// Carried only when an adapter is compiled in: without one there is
    /// nothing that could drain the outbox.
    #[cfg(feature = "outbox-nats")]
    pub(crate) outbox: Arc<dyn proxima_core::storage_ports::publication::PublicationOutboxPort>,
    /// Operator-only reclaim of records that were already DELIVERED, held
    /// under the same rule and for the same reason as `outbox`. A second
    /// handle rather than a method on the first: a drain loop must not be
    /// able to delete anything, and separate traits are how that is
    /// enforced rather than promised.
    #[cfg(feature = "outbox-nats")]
    pub(crate) outbox_retention:
        Arc<dyn proxima_core::storage_ports::publication::PublicationRetentionPort>,
}

impl std::fmt::Debug for Booted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Booted")
            .field("handle", &self.handle)
            .field("blobs", &self.blobs)
            .finish_non_exhaustive()
    }
}

/// Connect, migrate, compose, and start the engine for bundle `A`.
///
/// Everything that can be refused without a connection is refused first:
/// the registry freeze, the runtime-grant plan and the embedding width.
///
/// # Errors
///
/// [`ProximaError::Storage`] for connection, migration or runtime-grant
/// failures, [`ProximaError::Config`] for a refused grant plan or embedding
/// client, [`ProximaError::SchemaResetRequired`] when the target database does
/// not match `0001_v008.sql` (see `docs/how-to/migrations.md`), and
/// [`ProximaError::Engine`] when engine startup fails.
pub(crate) async fn boot<A: FlavorBundle>(
    config: &RuntimeConfig,
    parts: &RuntimeParts,
) -> Result<Booted, ProximaError> {
    let migrators = A::migrators();
    let registry = compose_registry::<A>()?;
    let grants = config
        .runtime_grants
        .then(|| {
            RuntimeGrants::plan(
                &config.database_url,
                config.platform_database_url.as_deref(),
                composed_schema_names(&registry),
                runtime_grants::ledger_names(&migrators),
            )
        })
        .transpose()?;
    let embedding_router =
        embedding_router_for(parts.embed_client.clone(), parts.embedding_router.clone())?;

    let mut pg = connect_and_migrate(config, migrators, grants.as_ref()).await?;
    if let Some(participant) = parts.host_state_participant.clone() {
        pg = pg.with_host_state_participant(participant);
    }
    let pg = admit_owner_rls(pg, &registry, config.platform_database_url.as_deref()).await?;
    let pg_sidecars = compose_pg_sidecars::<A>(&pg, &registry).await?;
    let pg = pg
        .with_sidecars(pg_sidecars.as_ref().clone())
        .try_with_flavors(&registry)
        .map_err(|error| ProximaError::Storage(error.to_string()))?
        .with_embedding_runtime_policy(config.embedding_runtime_policy);
    let erase_context = pg
        .host_state_erase_context()
        .map_err(|error| ProximaError::Storage(error.to_string()))?;
    let (publication_origin_eligibility, origin_scope) = publication_origin_ports(&pg).await?;

    let pool = pg.clone_pool_for_backend();
    let platform_scope = pg.platform_scope_for_host();
    let (pg, blobs) = compose_blob_stores(pg, config.s3.clone())?;

    let engine = compose_engine(registry, &pg, config, embedding_router)?;
    // The one handle on the captured outbox. Host-only: it is not in
    // `StoragePorts`, so no flavor, tool or write session can reach a
    // captured event, and a publisher task gets it from here.
    #[cfg(feature = "outbox-nats")]
    let outbox: Arc<dyn proxima_core::storage_ports::publication::PublicationOutboxPort> =
        Arc::new(pg.clone());
    #[cfg(feature = "outbox-nats")]
    let outbox_retention: Arc<
        dyn proxima_core::storage_ports::publication::PublicationRetentionPort,
    > = Arc::new(pg.clone());

    let (engine, system_authority, delegation_runtime_authority) =
        engine.into_runtime_authorities();
    let host_state_maintenance_authority = engine
        .host_state_maintenance_authority(&system_authority)
        .map_err(|error| ProximaError::Engine(error.to_string()))?;
    if let Some(store) = &blobs {
        store
            .bind_system_authority(&system_authority)
            .map_err(|error| ProximaError::Config(error.to_string()))?;
    }
    let engine = Arc::new(engine);
    let handle = engine
        .clone()
        .start()
        .await
        .map_err(|e| ProximaError::Engine(e.to_string()))?;
    let registry = Arc::new(engine.registry().clone());
    Ok(Booted {
        engine,
        system_authority,
        host_state_maintenance_authority,
        delegation_runtime_authority,
        handle,
        pool,
        platform_scope,
        registry,
        pg_sidecars,
        erase_context,
        publication_origin_eligibility,
        origin_scope,
        blobs,
        #[cfg(feature = "outbox-nats")]
        outbox,
        #[cfg(feature = "outbox-nats")]
        outbox_retention,
    })
}

/// The two halves of the retained-copy rule, taken from ONE database.
///
/// They are returned together because that is the invariant: the port
/// answers "does an origin row still exist", the scope answers "is this
/// message mine to ask that about", and a cleaner holding a port and a
/// scope from different databases would read a foreign stream as entirely
/// revoked. Deriving both here leaves no call site free to mix them.
///
/// An unreadable scope fails the boot. A publisher that cannot stamp
/// produces events no cleaner is ever allowed to act on — retained forever,
/// every cycle unhealthy — and entering that state silently is worse than
/// not starting. Migration 0012 writes the row, so its absence means the
/// schema was tampered with, not that a feature is switched off.
async fn publication_origin_ports(
    pg: &PgStorage,
) -> Result<(Arc<dyn PublicationOriginEligibilityPort>, OriginScope), ProximaError> {
    let scope = pg
        .origin_scope()
        .await
        .map_err(|error| ProximaError::Storage(error.to_string()))?;
    Ok((Arc::new(pg.clone()), scope))
}

/// Connect the Postgres storage backend and bring its schema to this
/// binary's expectation.
///
/// `runtime_grants` runs on the migration (platform) pool after migrating
/// and before the runtime pool connects: that connect already asserts the
/// runtime role under owner RLS.
async fn connect_and_migrate(
    config: &RuntimeConfig,
    migrators: Vec<NamedMigrator>,
    runtime_grants: Option<&RuntimeGrants>,
) -> Result<PgStorage, ProximaError> {
    let migration_url = config
        .platform_database_url
        .as_deref()
        .unwrap_or(&config.database_url);
    let migration_pg = PgStorage::connect_for_migrations_with_config(
        migration_url,
        config.pg_pool_config,
        config.pg_tuning,
    )
    .await
    .map_err(storage_error)?;
    if config.skip_migrations {
        // GitOps split-role deploy: schema is migrated out-of-band under a
        // DDL role; here we only run the preflight and issue no DDL.
        preflight_without_migrations(&migration_pg, migrators)
            .await
            .map_err(migration_error)?;
    } else {
        run_core_and_flavor_migrations(&migration_pg, migrators)
            .await
            .map_err(migration_error)?;
    }
    if let Some(grants) = runtime_grants {
        grants.apply(&migration_pg.clone_pool_for_backend()).await?;
    }
    drop(migration_pg);
    let pg = PgStorage::connect_with_config(
        &config.database_url,
        config.pg_pool_config,
        config.pg_tuning,
    )
    .await
    .map_err(storage_error)?;
    // Semantic search sends pgvector 0.8 session settings (iterative scan);
    // refuse an older or refusing extension at boot, not on every search.
    pg.ensure_pgvector_compatible()
        .await
        .map_err(storage_error)?;
    Ok(pg)
}

/// Run every linked flavor's registration callback and freeze the result.
fn compose_registry<A: FlavorBundle>() -> Result<proxima_core::FlavorRegistryFrozen, ProximaError> {
    let mut registry = FlavorRegistry::new();
    A::register(&mut registry)?;
    Ok(registry.try_freeze()?)
}

async fn admit_owner_rls(
    mut pg: PgStorage,
    registry: &proxima_core::FlavorRegistryFrozen,
    platform_database_url: Option<&str>,
) -> Result<PgStorage, ProximaError> {
    let schema_names = composed_schema_names(registry);
    let schema_refs: Vec<&str> = schema_names.iter().map(String::as_str).collect();
    let runtime_pool = pg.clone_pool_for_backend();
    let enforcing = proxima_storage_pg::owner_rls_enforced(&runtime_pool, &schema_refs)
        .await
        .map_err(|error| ProximaError::Storage(error.to_string()))?;
    if enforcing {
        if platform_database_url.is_none() {
            return Err(ProximaError::Storage(
                "owner RLS requires PROXIMA_PLATFORM_DATABASE_URL".into(),
            ));
        }
        proxima_storage_pg::assert_runtime_rls(&runtime_pool, &schema_refs)
            .await
            .map_err(|error| ProximaError::Storage(error.to_string()))?;
    }
    if let Some(platform_url) = platform_database_url {
        let platform_pool = sqlx::PgPool::connect(platform_url)
            .await
            .map_err(|error| ProximaError::Storage(error.to_string()))?;
        let platform = proxima_storage_pg::PgPlatformScope::new(platform_pool, &schema_refs)
            .await
            .map_err(|error| ProximaError::Storage(error.to_string()))?;
        pg = pg.with_platform_scope(platform);
    }
    Ok(pg)
}

/// Freeze the PG sidecar registry against the composed contracts, then
/// verify the deployed schema agrees with it.
async fn compose_pg_sidecars<A: FlavorBundle>(
    pg: &PgStorage,
    registry: &proxima_core::FlavorRegistryFrozen,
) -> Result<Arc<PgSidecarRegistryFrozen>, ProximaError> {
    let mut pg_sidecars = PgSidecarRegistry::new();
    register_core_pg_sidecars(&mut pg_sidecars);
    A::register_pg_sidecars(&mut pg_sidecars);
    let pg_sidecars = pg_sidecars
        .freeze_against(registry)
        .map_err(storage_error)?;
    let pg_sidecars = Arc::new(pg_sidecars);
    // The backend accessor, not `pool_for_tests`. This is a production boot
    // path on every embed; a `#[doc(hidden)]` handle whose name says "tests"
    // is not the contract it was reaching through.
    let pool = pg.clone_pool_for_backend();
    // Re-run the projection generator against the composed contracts and
    // compare it with the catalog. The migration carries the generator's
    // output verbatim; this is the half that notices a deployment whose
    // schema and whose linked flavors disagree — a flavor added without
    // its migrations, or a migration hand-edited away from the generator.
    proxima_storage_pg::projection::ensure_projection_schema(&pool, registry.contracts())
        .await
        .map_err(storage_error)?;
    // The same half, one layer down. The projection check asks whether
    // the tables a search reads exist; this asks whether the guard that
    // keeps every registered memory sidecar reachable by forget, erase
    // and export is installed on it. Both read the catalog and issue no
    // DDL, so both hold under `PROXIMA_SKIP_MIGRATIONS` in a split-role
    // deploy, where this process's role cannot create a trigger at all.
    proxima_storage_pg::integrity::ensure_declaration_triggers(&pool, pg_sidecars.as_ref())
        .await
        .map_err(storage_error)?;
    Ok(pg_sidecars)
}

/// Missing durable storage must not fall back to the low-level in-memory
/// test store: a successful forget would lose its payload on restart, and
/// a successful delete would acknowledge bytes the host cannot reach.
struct UnconfiguredColdStore;

impl UnconfiguredColdStore {
    fn unavailable() -> StorageError {
        StorageError::Unavailable("durable cold object storage is not configured".into())
    }
}

#[async_trait::async_trait]
impl ColdObjectStore for UnconfiguredColdStore {
    fn backend(&self) -> &'static str {
        "unconfigured://"
    }

    async fn put(&self, _key: &str, _bytes: &[u8]) -> Result<(), StorageError> {
        Err(Self::unavailable())
    }

    async fn get(&self, _key: &str) -> Result<Vec<u8>, StorageError> {
        Err(Self::unavailable())
    }

    async fn delete(&self, _key: &str) -> Result<(), StorageError> {
        Err(Self::unavailable())
    }
}

/// Wire durable cold storage, or an unavailable adapter for database-only
/// hosts. Cited uploads and cold storage must name the same backend.
fn compose_blob_stores(
    pg: PgStorage,
    s3: Option<proxima_blob_s3::S3RuntimeConfig>,
) -> Result<(PgStorage, Option<CitedBlobStore>), ProximaError> {
    let configured_bucket = s3.as_ref().map(|s3| s3.bucket.clone());
    let blobs = s3
        .map(|s3| {
            CitedBlobStore::new(pg.clone_pool_for_backend(), s3).map(|store| {
                match pg.platform_scope_for_host() {
                    Some(scope) => store.with_platform_scope(scope),
                    None => store,
                }
            })
        })
        .transpose()
        .map_err(|error| ProximaError::Config(error.to_string()))?;
    let pg = wire_cold_store(pg, blobs.as_ref(), configured_bucket.as_deref())?;
    Ok((pg, blobs))
}

fn wire_cold_store(
    pg: PgStorage,
    blobs: Option<&CitedBlobStore>,
    configured_bucket: Option<&str>,
) -> Result<PgStorage, ProximaError> {
    let Some(store) = blobs else {
        return Ok(pg.with_cold(Arc::new(UnconfiguredColdStore)));
    };
    let cold = store.cold_store();
    // Assert the composition the purge queue's `backend` column
    // depends on. `cold_purge_pending` records the bucket an
    // upload row named, and the drain refuses any row whose
    // backend is not the wired store's — so a store wired against
    // a different bucket than the one uploads publish to must
    // fail here, at boot, not silently stop reclaiming bytes.
    let configured_bucket = configured_bucket.unwrap_or_default();
    if ColdObjectStore::backend(&cold) != configured_bucket {
        return Err(ProximaError::Config(format!(
            "cold object store reports backend {:?} but cited uploads publish to \
             bucket {configured_bucket:?}; the purge queue and the object store \
             must name one backend",
            ColdObjectStore::backend(&cold),
        )));
    }
    Ok(pg.with_cold(Arc::new(cold)))
}

/// Compose the engine over the frozen registry and wired storage, attaching
/// the deployment tool scope, the publication configuration and the host's
/// embedding router.
///
/// `try_with_publication_config` is on the UNCONDITIONAL path, with the
/// parsed config even when it binds no source. That is the whole boot
/// guarantee: a bundle that freezes a listenable schema and a deployment
/// that never set `PROXIMA_PUBLICATION_SOURCE` is refused here, naming the
/// schemas, instead of admitting writes that would each fail at capture.
fn compose_engine(
    registry: proxima_core::FlavorRegistryFrozen,
    pg: &PgStorage,
    config: &RuntimeConfig,
    embedding_router: Option<Arc<dyn EmbeddingRouter>>,
) -> Result<Engine, ProximaError> {
    let mut engine = Engine::new(registry)
        .with_storage_ports(Arc::new(pg.clone()).storage_ports())
        .with_embedding_runtime_policy(config.embedding_runtime_policy)
        .try_with_publication_config(config.publication.clone())
        .map_err(|error| ProximaError::Config(error.to_string()))?
        .with_deployment_tool_scope(config.tool_scope.clone());
    if let Some(router) = embedding_router {
        engine = engine.with_embedding_router(router);
    }
    Ok(engine)
}

/// The router the engine embeds through: the host's, or one client for
/// every Owner. Resolved before any connection, so an unusable client is a
/// boot refusal rather than a claimed-then-refused job queue.
fn embedding_router_for(
    embed_client: Option<Arc<dyn EmbeddingClient>>,
    embedding_router: Option<Arc<dyn EmbeddingRouter>>,
) -> Result<Option<Arc<dyn EmbeddingRouter>>, ProximaError> {
    match (embed_client, embedding_router) {
        (Some(_), Some(_)) => Err(ProximaError::Config(
            "set embed_client or embedding_router, not both".into(),
        )),
        (Some(client), None) => {
            // Fail fast on a width no lane indexes: jobs would be claimed
            // and then rejected at insert, silently burning the queue.
            let router = proxima_core::llm::SingleClientRouter::bind(client)
                .map_err(|error| ProximaError::Config(error.to_string()))?;
            Ok(Some(Arc::new(router)))
        }
        (None, router) => Ok(router),
    }
}

/// Map a storage error onto [`ProximaError`], preserving the typed
/// [`StorageError::SchemaResetRequired`] signal instead of collapsing it
/// into the generic [`ProximaError::Storage`] string.
pub(crate) fn storage_error(err: StorageError) -> ProximaError {
    match err {
        StorageError::SchemaResetRequired { details } => {
            ProximaError::SchemaResetRequired { details }
        }
        other => ProximaError::Storage(other.to_string()),
    }
}

/// Map a migration-facade error onto [`ProximaError`], unwrapping the core
/// preflight check so a stale pre-lane database still surfaces as
/// [`ProximaError::SchemaResetRequired`] rather than a generic storage string.
fn migration_error(err: MigrationError) -> ProximaError {
    match err {
        MigrationError::CorePreflight(storage_err) => storage_error(storage_err),
        other => ProximaError::Storage(other.to_string()),
    }
}

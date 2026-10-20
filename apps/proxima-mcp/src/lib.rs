pub mod args;

pub use args::{
    ArgsError, DEFAULT_BIND, DEFAULT_DATABASE_URL, MAINTAIN_BLOBS_USAGE, MAINTAIN_USAGE,
    MaintainBlobsConfig, MaintainConfig, McpConfig, ReconcileScope, STORAGE_MAINTENANCE_USAGE,
    StorageMaintenanceConfig, USAGE, parse_args, parse_maintain_args, parse_maintain_blobs_args,
    parse_storage_maintenance_args,
};

use proxima_blob_s3::{CitedBlobStore, S3RuntimeConfig};

use std::collections::{BTreeSet, HashSet};
use std::num::NonZeroU32;
use std::sync::Arc;

use proxima::flavor::FlavorBundle;
use proxima::{
    AppContext, AppInfo, EmbeddingRuntimePolicy, FlavorApp, FlavorServiceError, FlavorServices,
    Proxima, ProximaError, RunningProxima, RuntimeBuilder, embedding_runtime_policy_from_lookup,
    missing_migration_policy_from_lookup, run_core_and_flavor_migrations_with_policy,
};
use proxima_core::llm::{
    BoundEmbeddingClient, QueryInstruction, QueryInstructions, QueryTask, SingleClientRouter,
};
use proxima_core::protocol::profile as protocol_profile;
use proxima_core::{
    FlavorRegistry, FlavorRegistryError, ScopeKey, ServedSurface, all_core_actions,
    all_core_resources,
};
use proxima_llm_openai_compat::OpenAiCompatEmbeddingClient;
use proxima_storage_pg::{
    ColdPurgeRetryOptions, ColdPurgeRetryOutcome, EmbeddingReconcileOptions,
    EmbeddingReconcileScope, PgStorage,
};

const PROXIMA_EMBED_MODEL: &str = "PROXIMA_EMBED_MODEL";
const PROXIMA_EMBED_BASE_URL: &str = "PROXIMA_EMBED_BASE_URL";
const PROXIMA_EMBED_API_KEY: &str = "PROXIMA_EMBED_API_KEY";
const PROXIMA_EMBED_MATRYOSHKA: &str = "PROXIMA_EMBED_MATRYOSHKA";
const PROXIMA_EMBED_DIM: &str = "PROXIMA_EMBED_DIM";
const PROXIMA_EMBED_MAX_INPUT_CHARS: &str = "PROXIMA_EMBED_MAX_INPUT_CHARS";
const DEFAULT_EMBED_MAX_INPUT_CHARS: NonZeroU32 =
    NonZeroU32::new(16_384).expect("embedding input default is positive");
const PROXIMA_EMBED_QUERY_INSTRUCTION: &str = "PROXIMA_EMBED_QUERY_INSTRUCTION";
const PROXIMA_TOOL_PROFILE: &str = "PROXIMA_TOOL_PROFILE";
const PROXIMA_TOOL_ALLOW: &str = "PROXIMA_TOOL_ALLOW";
const PROXIMA_TOOL_DENY: &str = "PROXIMA_TOOL_DENY";

/// Tool/action scope keys advertised by the `memory` profile. Flat entries
/// reference the owning tool's `McpTool::NAME`; grouped tools contribute
/// action leaf keys from their manifest.
///
/// Reads `all_core_actions()` because it is a curated allow-list — a
/// deployment decision about which substrate actions the memory profile
/// advertises — not an enumeration of what exists; that is
/// [`registered_tool_ids`], which reads the descriptors. The two agreeing on
/// the substrate actions they both name is pinned by
/// `core_action_meta_decorates_only_declared_actions` in
/// `crates/core/tests/flavor_registry.rs`.
fn memory_keep_set() -> Vec<&'static str> {
    use proxima_core::mcp::McpTool;
    use proxima_core::mcp::core_tools::{
        CoreFactTool, CoreGoalTool, CoreUploadTool, DeriveTool, EpisodeCommitTool, ForgetTool,
        InterpretTool, MemorySpacesTool, RecallTool, RecordUtteranceTool, RememberTool,
        RestoreTool, SearchMemoriesTool, ThinkTool,
    };

    let mut ids = vec![
        // authoring
        RememberTool::NAME,
        EpisodeCommitTool::NAME,
        ForgetTool::NAME,
        RestoreTool::NAME,
        DeriveTool::NAME,
        InterpretTool::NAME,
        RecordUtteranceTool::NAME,
        // retrieval
        SearchMemoriesTool::NAME,
        RecallTool::NAME,
        ThinkTool::NAME,
        MemorySpacesTool::NAME,
    ];
    // The memory profile carries the full goal lifecycle, non-destructive
    // fact/citation actions, and the cited-blob upload lane (an upload is
    // memory authoring: it exists to be cited by a Fact).
    // Retention/cleanup are host/config-only and stay out.
    ids.extend(
        all_core_actions()
            .filter(|action| {
                action.tool == CoreGoalTool::NAME
                    || action.tool == CoreFactTool::NAME
                    || action.tool == CoreUploadTool::NAME
            })
            .map(|action| action.scope_key),
    );
    ids.extend(all_core_resources().map(|resource| resource.scope_key));
    ids
}

/// proxima-mcp serves the core only: it links no flavor. A host that wants a
/// flavor composes its own app with it.
type LinkedFlavors = ();

#[derive(Debug)]
pub struct ProximaMcpApp;

impl FlavorBundle for ProximaMcpApp {
    fn register(registry: &mut FlavorRegistry) -> Result<(), FlavorRegistryError> {
        <LinkedFlavors as FlavorBundle>::register(registry)
    }

    fn register_pg_sidecars(registry: &mut proxima::flavor::PgSidecarRegistry) {
        <LinkedFlavors as FlavorBundle>::register_pg_sidecars(registry);
    }

    fn migrators() -> Vec<proxima::NamedMigrator> {
        <LinkedFlavors as FlavorBundle>::migrators()
    }
}

impl FlavorApp for ProximaMcpApp {
    fn app_info() -> AppInfo {
        AppInfo {
            id: "proxima-mcp",
            title: "Proxima MCP",
            version: env!("CARGO_PKG_VERSION"),
        }
    }

    fn configure(builder: RuntimeBuilder) -> RuntimeBuilder {
        builder.database_url(DEFAULT_DATABASE_URL).mcp_bind(
            DEFAULT_BIND
                .parse()
                .expect("DEFAULT_BIND must be a valid SocketAddr"),
        )
    }

    fn services(_ctx: &AppContext) -> Result<FlavorServices, FlavorServiceError> {
        Ok(FlavorServices::default())
    }
}

/// Headless composition used by the global blob maintenance command.
///
/// It links the same compiled flavors and migrations as the serving app but
/// deliberately contributes no MCP configuration. Booting through the facade
/// binds the runtime-provided store and `SystemAuthority` to the same boot.
struct BlobMaintenanceApp;

impl FlavorBundle for BlobMaintenanceApp {
    fn register(registry: &mut FlavorRegistry) -> Result<(), FlavorRegistryError> {
        <ProximaMcpApp as FlavorBundle>::register(registry)
    }

    fn register_pg_sidecars(registry: &mut proxima::flavor::PgSidecarRegistry) {
        <ProximaMcpApp as FlavorBundle>::register_pg_sidecars(registry);
    }

    fn migrators() -> Vec<proxima::NamedMigrator> {
        <ProximaMcpApp as FlavorBundle>::migrators()
    }
}

impl FlavorApp for BlobMaintenanceApp {
    fn app_info() -> AppInfo {
        AppInfo {
            id: "proxima-mcp-maintain-blobs",
            title: "Proxima MCP Blob Maintenance",
            version: env!("CARGO_PKG_VERSION"),
        }
    }

    /// Supply the required deployment policy for this maintenance runtime,
    /// which starts without an MCP listener.
    fn configure(builder: RuntimeBuilder) -> RuntimeBuilder {
        builder.served_surface(ServedSurface::Palette(Vec::new()))
    }
}

/// # Errors
///
/// Returns argument, facade boot, or MCP transport failures.
pub async fn run<I: IntoIterator<Item = String>>(args: I) -> Result<(), CliError> {
    let mut args: Vec<String> = args.into_iter().collect();
    if args.first().is_some_and(|arg| arg == "maintain-embeddings") {
        args.remove(0);
        let config = parse_maintain_args(args).map_err(|err| match err {
            ArgsError::Help => CliError::Help(MAINTAIN_USAGE),
            other @ ArgsError::Invalid(_) => CliError::Args(other),
        })?;
        return run_maintain(config).await;
    }
    if args.first().is_some_and(|arg| arg == "maintain-storage") {
        args.remove(0);
        let config = parse_storage_maintenance_args(args).map_err(|err| match err {
            ArgsError::Help => CliError::Help(STORAGE_MAINTENANCE_USAGE),
            other @ ArgsError::Invalid(_) => CliError::Args(other),
        })?;
        return run_maintain_storage(config).await;
    }
    if args.first().is_some_and(|arg| arg == "maintain-blobs") {
        args.remove(0);
        let config = parse_maintain_blobs_args(args).map_err(|err| match err {
            ArgsError::Help => CliError::Help(MAINTAIN_BLOBS_USAGE),
            other @ ArgsError::Invalid(_) => CliError::Args(other),
        })?;
        return run_maintain_blobs(config).await;
    }
    if args.first().is_some_and(|arg| arg == "serve") {
        args.remove(0);
    }
    let config = parse_args(args).map_err(|err| match err {
        ArgsError::Help => CliError::Help(USAGE),
        other @ ArgsError::Invalid(_) => CliError::Args(other),
    })?;
    // rmcp 3.x logs idle-session keep-alive expiry and the resulting
    // session-cleanup race at ERROR; both are clean lifecycle events
    // (`quit_reason=Closed`), so those targets are pinned to `warn`.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new(
                    "info,rmcp::transport::worker=warn,\
                     rmcp::transport::streamable_http_server::tower=warn",
                )
            }),
        )
        .try_init()
        .ok();

    let running = run_with_handle(config).await?;
    let addr = running
        .mcp_addr()
        .ok_or_else(|| CliError::Runtime(ProximaError::Mcp("MCP listener disabled".into())))?;
    tracing::info!(addr = %addr, "proxima-mcp listening; POST http://{addr}/mcp");
    // The runtime started every configured feature — the embedding worker
    // with an embedding client — and joins them all on the way out. This
    // binary hands the runtime no outbox intake, so captured Facts wait in
    // the outbox for a host that embeds Proxima and supplies one (docs/18).
    running.until_shutdown_signal().await?;
    Ok(())
}

/// Reconcile the blob store against the rows that name it, and print what
/// disagrees.
///
/// NO ADVISORY LOCK, unlike `maintain-embeddings`. That pass mutates —
/// it deletes orphan rows and enqueues jobs — so two of them racing would
/// duplicate work. This one only reads, so concurrent passes are merely
/// redundant, and a lock would add a failure mode (a crashed holder blocking
/// the health check) to a verb whose whole job is telling you something is
/// wrong.
///
/// EXITS 0 EVEN WHEN ARTEFACTS ARE MISSING, matching `maintain-embeddings`
/// and `maintain-storage`. A non-zero exit here would mean "the pass
/// failed", and the pass did not fail — it succeeded, and the news is bad.
/// Alert on the `missing=` number.
async fn run_maintain_blobs(config: MaintainBlobsConfig) -> Result<(), CliError> {
    let s3 = S3RuntimeConfig::from_env().map_err(|err| {
        ProximaError::Config(format!(
            "maintain-blobs needs the host's PROXIMA_S3_* block: {err}"
        ))
    })?;
    let built = blob_maintenance_app(config.database_url, s3, &proxima_core::process_env)?
        .build()
        .await?;
    let outcome = match built.host().blobs() {
        Some(store) => store
            .reconcile_all(built.system_authority())
            .await
            .map_err(|err| ProximaError::Storage(err.to_string())),
        None => Err(ProximaError::Config(
            "maintain-blobs booted without its required S3 store".into(),
        )),
    };
    built.shutdown().await;
    let outcome = outcome?;

    println!(
        "reconcile-blobs: rows={} objects={} missing={} orphans={} owed={} newly-owed={} stale-transfers={} foreign={}",
        outcome.rows_scanned,
        outcome.objects_scanned,
        outcome.missing_objects,
        outcome.orphan_objects,
        outcome.owed_objects,
        outcome.newly_owed_objects,
        outcome.stale_transfer_objects,
        outcome.foreign_locators,
    );
    // The samples are what an operator acts on, and they are bounded by
    // `MAX_RECONCILE_SAMPLE` rather than by anything decided here.
    for missing in &outcome.missing_sample {
        println!(
            "  missing: {} ({} bytes, {}) cited_object_id={}",
            missing.object_key, missing.byte_len, missing.filename, missing.cited_object_id,
        );
    }
    for orphan in &outcome.orphan_sample {
        println!("  orphan: {orphan}");
    }
    for stale in &outcome.stale_transfer_sample {
        println!("  stale-transfer: {stale}");
    }
    for foreign in &outcome.foreign_sample {
        println!("  foreign: {foreign}");
    }
    if outcome.is_intact() {
        println!("every artefact this store is responsible for has its object");
    }
    Ok(())
}

/// One embedding self-healing pass: orphan sweep, reconcile enqueue,
/// health report. Serialized across processes by a
/// maintenance lease — an overlapping pass skips with exit 0 so cron
/// overlap is harmless by construction, not by scheduling discipline.
async fn run_maintain(config: MaintainConfig) -> Result<(), CliError> {
    let space = maintenance_embedding_space(config.model, proxima_core::process_env)?;
    let embedding_policy = embedding_runtime_policy_from_lookup(&proxima_core::process_env)?;
    let storage = maintenance_storage(&config.database_url, embedding_policy).await?;

    let Some(lock) = storage
        .try_embedding_maintenance_lock()
        .await
        .map_err(|err| ProximaError::Storage(err.to_string()))?
    else {
        println!("skipped: another embedding maintenance pass holds the lease");
        return Ok(());
    };

    let pass: Result<(), CliError> = async {
        let sweep = storage
            .sweep_orphan_embedding_rows()
            .await
            .map_err(|err| ProximaError::Storage(err.to_string()))?;
        println!(
            "orphan-sweep: embeddings={} heads={} jobs={}",
            sweep.embeddings_deleted, sweep.heads_deleted, sweep.jobs_deleted
        );

        // This pass runs against a bare `PgStorage` with no flavor app booted,
        // so the only declarations it can see are core's own, which is where
        // `core/upload-v1` lives. A host whose FLAVORS declare non-embeddable
        // schemas must reconcile through its own binary, where the full
        // registry exists; this pass would re-enqueue those. Printed below, so
        // the scope of the pass is visible in its own output.
        let core_registry = proxima_core::FlavorRegistry::new()
            .try_freeze()
            .map_err(|err| ProximaError::Storage(err.to_string()))?;
        let non_embeddable = core_registry.non_embeddable_schema_ids();
        let outcome = storage
            .reconcile_embeddings(EmbeddingReconcileOptions {
                space: &space,
                owners: None,
                scope: reconcile_scope(config.scope),
                // Omitted `--limit` is the operator full pass. Process boot
                // never takes this path; it uses EMBEDDING_RECONCILE_DEFAULT_LIMIT.
                limit: Some(config.limit.unwrap_or(i64::MAX)),
                non_embeddable_schemas: non_embeddable,
            })
            .await
            .map_err(|err| ProximaError::Storage(err.to_string()))?;
        println!(
            "reconcile: scanned={} enqueued={} skipped={} not-embeddable=[{}]",
            outcome.scanned,
            outcome.enqueued,
            outcome.skipped,
            non_embeddable.join(", ")
        );

        let health = storage
            .embedding_ann_observability()
            .await
            .map_err(|err| ProximaError::Storage(err.to_string()))?;
        println!(
            "backlog: pending={} processing={} failed={} stale-processing={}",
            health.backlog.pending,
            health.backlog.processing,
            health.backlog.failed,
            health.stale_processing_jobs
        );
        println!(
            "orphans-remaining: embeddings={} heads={} jobs={}",
            health.orphan_rows.embeddings, health.orphan_rows.heads, health.orphan_rows.jobs
        );
        match health.recall_canary {
            Some(canary) => println!(
                "recall-canary: model={} recall@{}={:.3} (ann={} exact={} overlap={})",
                canary.model_id,
                canary.k,
                canary.recall_at_k,
                canary.ann_count,
                canary.exact_count,
                canary.overlap_count
            ),
            None => println!("recall-canary: none (no embeddings yet)"),
        }
        Ok(())
    }
    .await;
    if let Err(err) = lock.release().await {
        tracing::debug!(error = %err, "embedding maintenance lease release failed; it expires instead");
    }
    pass
}

/// The `maintain-blobs` runtime: no listener, the platform URL, and the
/// migration URL when one is set, so its boot migrates where every other
/// entry point does.
fn blob_maintenance_app(
    database_url: String,
    s3: S3RuntimeConfig,
    lookup: &impl Fn(&str) -> Option<String>,
) -> Result<Proxima<BlobMaintenanceApp>, CliError> {
    let urls = maintenance_urls(lookup)?;
    let app = Proxima::<BlobMaintenanceApp>::app()
        .database_url(database_url)
        .platform_database_url(urls.platform)
        .s3(s3);
    Ok(match urls.migration {
        Some(migration) => app.migration_database_url(migration),
        None => app,
    })
}

async fn maintenance_storage(
    database_url: &str,
    embedding_policy: proxima_core::EmbeddingRuntimePolicy,
) -> Result<PgStorage, CliError> {
    let missing_policy =
        missing_migration_policy_from_lookup(&proxima_core::process_env)?.unwrap_or_default();
    let urls = maintenance_urls(&proxima_core::process_env)?;
    let store_number = proxima::parse_store_number(
        &proxima_core::env_value(&proxima_core::process_env, "PROXIMA_STORE_NUMBER").ok_or_else(
            || ProximaError::Config("PROXIMA_STORE_NUMBER is required for maintenance".into()),
        )?,
    )?;
    let config = proxima_storage_pg::PgPoolConfig::from_env()
        .map_err(|err| ProximaError::Storage(err.to_string()))?;
    let tuning = proxima_storage_pg::PgTuning::from_env()
        .map_err(|err| ProximaError::Storage(err.to_string()))?;
    let mut registry = FlavorRegistry::new();
    ProximaMcpApp::register(&mut registry).map_err(ProximaError::from)?;
    let registry = registry.try_freeze().map_err(ProximaError::from)?;
    let schemas = proxima::composed_schema_names(&registry);
    let schema_refs: Vec<&str> = schemas.iter().map(String::as_str).collect();
    let mut sidecars = proxima::flavor::PgSidecarRegistry::new();
    proxima::flavor::register_core_pg_sidecars(&mut sidecars);
    ProximaMcpApp::register_pg_sidecars(&mut sidecars);
    let sidecars = sidecars
        .freeze_against(&registry)
        .map_err(|err| ProximaError::Storage(err.to_string()))?;
    // Migrations hold a session lock and session settings: they run on
    // PROXIMA_MIGRATION_DATABASE_URL when the platform URL goes through a
    // transaction-mode pooler. This one-shot command keeps that connection
    // as its platform scope too.
    let migration_pg = proxima_storage_pg::PgMigrationStorage::connect_with_config(
        urls.migrations(),
        config,
        tuning,
    )
    .await
    .map_err(|err| ProximaError::Storage(err.to_string()))?;
    run_core_and_flavor_migrations_with_policy(
        &migration_pg,
        ProximaMcpApp::migrators(),
        &sidecars,
        missing_policy,
    )
    .await
    .map_err(|err| ProximaError::Storage(err.to_string()))?;
    let platform_scope = proxima_storage_pg::PgPlatformScope::new(
        migration_pg.clone_pool_for_backend(),
        &schema_refs,
    )
    .await
    .map_err(|err| ProximaError::Storage(err.to_string()))?;
    migration_pg
        .initialize_store_number(store_number)
        .await
        .map_err(|err| ProximaError::Storage(err.to_string()))?;
    drop(migration_pg);
    let storage =
        PgStorage::connect_with_config(database_url, config, tuning, platform_scope, store_number)
            .await
            .map_err(|err| ProximaError::Storage(err.to_string()))?
            .with_sidecars(sidecars)
            .with_embedding_runtime_policy(embedding_policy);
    Ok(storage)
}

/// The maintenance commands' platform URL and, when set, their migration
/// URL, checked as boot checks it (same role and database, no URL echoed).
struct MaintenanceUrls {
    platform: String,
    migration: Option<String>,
}

impl MaintenanceUrls {
    /// Where the command migrates.
    fn migrations(&self) -> &str {
        self.migration.as_deref().unwrap_or(&self.platform)
    }
}

fn maintenance_urls(lookup: &impl Fn(&str) -> Option<String>) -> Result<MaintenanceUrls, CliError> {
    let platform =
        proxima_core::env_value(lookup, "PROXIMA_PLATFORM_DATABASE_URL").ok_or_else(|| {
            ProximaError::Config("PROXIMA_PLATFORM_DATABASE_URL is required for maintenance".into())
        })?;
    let migration = proxima::migration_database_url_from_lookup(lookup, &platform)?;
    Ok(MaintenanceUrls {
        platform,
        migration,
    })
}

/// One storage-maintenance pass: the expired-upload sweep, when
/// `--sweep-expired-uploads` asks for it, then the cold-object purge drain,
/// when `--retry-cold-object-purges` does. The same lease serializes it with
/// the in-process drain of every Proxima server on this database. Serialized across
/// processes by its own maintenance lease — an overlapping pass skips
/// with exit 0, like `maintain-embeddings`.
async fn run_maintain_storage(config: StorageMaintenanceConfig) -> Result<(), CliError> {
    let s3 = ((config.retry_cold_object_purges && !config.dry_run) || config.sweep_expired_uploads)
        .then(|| {
            S3RuntimeConfig::from_env().map_err(|err| {
                ProximaError::Config(format!(
                    "storage maintenance object-store actions need the host's \
                     PROXIMA_S3_* block: {err}"
                ))
            })
        })
        .transpose()?;

    let storage = maintenance_storage(
        &config.database_url,
        embedding_runtime_policy_from_lookup(&proxima_core::process_env)?,
    )
    .await?;

    let (storage, sweeper) = if let Some(s3) = s3 {
        let mut registry = FlavorRegistry::new();
        <ProximaMcpApp as FlavorBundle>::register(&mut registry).map_err(ProximaError::from)?;
        let registry = registry.try_freeze().map_err(ProximaError::from)?;
        let mut sidecars = proxima::flavor::PgSidecarRegistry::new();
        proxima::flavor::register_core_pg_sidecars(&mut sidecars);
        <ProximaMcpApp as FlavorBundle>::register_pg_sidecars(&mut sidecars);
        let sidecars = sidecars
            .freeze_against(&registry)
            .map_err(|err| ProximaError::Storage(err.to_string()))?;
        let blobs = CitedBlobStore::new(
            storage.clone_pool_for_backend(),
            s3,
            storage.platform_scope_for_host(),
        )
        .map_err(|err| ProximaError::Config(err.to_string()))?;
        // This command is its own boot: its engine's authority is the one
        // the store answers host-wide operations to.
        let (_, authority) = proxima::Engine::new(registry).into_system_authority();
        blobs
            .bind_system_authority(&authority)
            .map_err(|err| ProximaError::Config(err.to_string()))?;
        let storage = storage
            .with_sidecars(sidecars)
            .with_cold(Arc::new(blobs.cold_store()));
        (storage, Some((blobs, authority)))
    } else {
        (storage, None)
    };

    let Some(lock) = storage
        .try_storage_maintenance_lock()
        .await
        .map_err(|err| ProximaError::Storage(err.to_string()))?
    else {
        println!("skipped: another storage maintenance pass holds the lease");
        return Ok(());
    };

    // Release the lease on every exit, so the next cron fire need not wait
    // for it to expire.
    let pass = storage_maintenance_steps(&config, &storage, sweeper.as_ref(), &lock).await;
    if let Err(err) = lock.release().await {
        tracing::debug!(error = %err, "storage maintenance lease release failed; it expires instead");
    }
    pass
}

/// The steps of one storage-maintenance pass, run under its lease.
async fn storage_maintenance_steps(
    config: &StorageMaintenanceConfig,
    storage: &PgStorage,
    sweeper: Option<&(CitedBlobStore, proxima_core::authz::SystemAuthority)>,
    lock: &proxima_storage_pg::StorageMaintenanceLock,
) -> Result<(), CliError> {
    let dry_run_suffix = if config.dry_run { " (dry-run)" } else { "" };

    if config.sweep_expired_uploads {
        let (blobs, authority) = sweeper.ok_or_else(|| {
            ProximaError::Config("the expired-upload sweep needs the PROXIMA_S3_* block".into())
        })?;
        let limit = u32::try_from(config.batch_size).unwrap_or(u32::MAX);
        let mut expired = 0_u64;
        let mut owed = 0_u64;
        let remaining = loop {
            lock.renew()
                .await
                .map_err(|err| ProximaError::Storage(err.to_string()))?;
            let batch = blobs
                .sweep_expired_uploads(authority, limit)
                .await
                .map_err(|err| ProximaError::Storage(err.to_string()))?;
            expired += batch.expired;
            owed += batch.owed;
            if batch.expired == 0 || batch.remaining == 0 {
                break batch.remaining;
            }
        };
        println!("upload-expiry-sweep: expired={expired} owed={owed} remaining={remaining}");
    }

    if config.retry_cold_object_purges {
        lock.renew()
            .await
            .map_err(|err| ProximaError::Storage(err.to_string()))?;
        let outcome = storage
            .retry_cold_object_purges(
                ColdPurgeRetryOptions::default()
                    .batch_size(config.batch_size)
                    .dry_run(config.dry_run),
            )
            .await
            .map_err(|err| ProximaError::Storage(err.to_string()))?;
        for line in cold_purge_retry_report(&outcome, dry_run_suffix) {
            println!("{line}");
        }
        if outcome.foreign > 0 {
            // The drain left them in place; the operator still has to wire
            // the store they are owed to, so this pass is not a success.
            return Err(ProximaError::Storage(format!(
                "purge queue holds {} debt(s) owed to object stores this process is not \
                 wired to; run the drain with the PROXIMA_S3_* settings (or \
                 PROXIMA_S3_STORE_ID) of the store each is owed to",
                outcome.foreign
            ))
            .into());
        }
    }
    Ok(())
}

/// The report lines for one drain: the counts, then each stuck debt and each
/// other store owed debt.
fn cold_purge_retry_report(outcome: &ColdPurgeRetryOutcome, dry_run_suffix: &str) -> Vec<String> {
    let mut lines = vec![format!(
        "cold-object-purge: selected={} purged={} absent={} failed={} due={} deferred={} \
         stuck={} foreign={}{dry_run_suffix}",
        outcome.selected,
        outcome.purged,
        outcome.absent,
        outcome.failed,
        outcome.due,
        outcome.deferred,
        outcome.stuck_count,
        outcome.foreign,
    )];
    for debt in &outcome.stuck {
        lines.push(stuck_debt_line(
            &debt.object_key,
            debt.owner_id.as_ref(),
            debt.attempts,
            &debt.next_attempt_at,
            debt.last_error.as_deref(),
        ));
    }
    for (store, count) in &outcome.foreign_stores {
        lines.push(format!(
            "cold-object-purge: foreign store={store} debts={count}"
        ));
    }
    lines
}

/// One stuck debt, named with its owner (`-` once the owner is erased) and
/// its last error.
fn stuck_debt_line(
    object_key: &str,
    owner: Option<&proxima_core::OwnerId>,
    attempts: u32,
    next_attempt_at: &dyn std::fmt::Display,
    last_error: Option<&str>,
) -> String {
    let owner = owner.map_or_else(|| "-".to_owned(), ToString::to_string);
    format!(
        "cold-object-purge: stuck key={object_key} owner={owner} attempts={attempts} \
         next_attempt_at={next_attempt_at} last_error={}",
        last_error.unwrap_or("-"),
    )
}

fn reconcile_scope(scope: ReconcileScope) -> EmbeddingReconcileScope {
    match scope {
        ReconcileScope::MissingOnly => EmbeddingReconcileScope::MissingOnly,
        ReconcileScope::IncludeStale => EmbeddingReconcileScope::IncludeStale,
        ReconcileScope::Since(since) => EmbeddingReconcileScope::Since(since),
    }
}

/// # Errors
///
/// Returns facade config, storage, migration, engine, bind, or transport failures.
pub async fn run_with_handle(config: McpConfig) -> Result<RunningProxima, CliError> {
    let app = build_app(config, proxima_core::process_env)?;
    app.run().await.map_err(Into::into)
}

fn build_app(
    config: McpConfig,
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<Proxima<ProximaMcpApp>, CliError> {
    let registered = registered_tool_ids()?;
    let served_surface = served_surface_from_env(&lookup, &registered)?;
    let embedding_policy = embedding_runtime_policy_from_lookup(&lookup)?;
    let mut app = Proxima::<ProximaMcpApp>::app()
        .from_lookup(&lookup)?
        .database_url(config.database_url)
        .served_surface(served_surface)
        .embedding_runtime_policy(embedding_policy);
    if let Some(bind) = config.bind {
        app = app.mcp_bind(bind);
    }
    if let Some(client) = embedding_client_from_env(&lookup, embedding_policy)? {
        app = app.embedding_router(Arc::new(embedding_router_from_env(&lookup, client)?));
    }
    Ok(app)
}

/// Every Owner embedded through `client`, its search queries worded by the
/// configured query instruction.
fn embedding_router_from_env(
    lookup: &impl Fn(&str) -> Option<String>,
    client: OpenAiCompatEmbeddingClient,
) -> Result<SingleClientRouter, CliError> {
    let bound = BoundEmbeddingClient::bind(Arc::new(client))
        .map_err(|err| CliError::Runtime(ProximaError::Config(err.to_string())))?
        .with_query_instructions(query_instructions_from_env(lookup)?);
    Ok(SingleClientRouter::new(bound))
}

/// The instruction `PROXIMA_EMBED_QUERY_INSTRUCTION` wraps every search
/// query in before it is embedded. Unset embeds the query as sent.
fn query_instructions_from_env(
    lookup: &impl Fn(&str) -> Option<String>,
) -> Result<QueryInstructions, CliError> {
    let mut instructions = QueryInstructions::default();
    if let Some(instruction) = query_instruction_from_env(lookup, PROXIMA_EMBED_QUERY_INSTRUCTION)?
    {
        instructions = instructions.with_instruction(QueryTask::DEFAULT, instruction);
    }
    Ok(instructions)
}

/// One query instruction template. `\n` in the value is a newline: formats
/// such as Qwen3's need one, and an environment value cannot easily hold it.
fn query_instruction_from_env(
    lookup: &impl Fn(&str) -> Option<String>,
    key: &'static str,
) -> Result<Option<QueryInstruction>, CliError> {
    lookup_non_empty(lookup, key)
        .map(|raw| {
            QueryInstruction::new(raw.replace("\\n", "\n"))
                .map_err(|err| CliError::Runtime(ProximaError::Config(format!("{key}: {err}"))))
        })
        .transpose()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolProfile {
    Full,
    Memory,
}

/// Every key deployment configuration may name: one id per flat tool, one
/// `tool:action` leaf per dispatcher action, plus the resource keys.
///
/// The descriptor's `action_arg_specs` are the enumeration, so a flavor
/// dispatcher contributes one leaf per action rather than a single bare id.
/// That is what lets `PROXIMA_TOOL_ALLOW` name one of its leaves and
/// `PROXIMA_TOOL_PROFILE=full` grant it leaf by leaf, exactly as it does for
/// a substrate dispatcher.
fn registered_tool_ids() -> Result<RegisteredKeys, CliError> {
    let mut registry = FlavorRegistry::new();
    <ProximaMcpApp as FlavorBundle>::register(&mut registry)
        .map_err(|err| CliError::Runtime(ProximaError::Registry(err)))?;
    let frozen = registry
        .try_freeze()
        .map_err(|err| CliError::Runtime(ProximaError::Registry(err)))?;
    Ok(RegisteredKeys {
        all: proxima_core::canonical_scope_keys(&frozen),
        served_reads: proxima_core::canonical_served_read_keys(&frozen),
    })
}

/// What this build registers, as the env profile is resolved against it.
#[derive(Debug, Default)]
struct RegisteredKeys {
    /// Every canonical key: what `PROXIMA_TOOL_ALLOW` and `PROXIMA_TOOL_DENY`
    /// may name.
    all: Vec<ScopeKey>,
    /// Every declared read and core resource: a
    /// `PROXIMA_TOOL_DENY` entry naming one is refused.
    served_reads: Vec<ScopeKey>,
}

fn served_surface_from_env(
    lookup: &impl Fn(&str) -> Option<String>,
    registered: &RegisteredKeys,
) -> Result<ServedSurface, CliError> {
    resolve_served_surface(
        lookup_non_empty(lookup, PROXIMA_TOOL_PROFILE).as_deref(),
        lookup_non_empty(lookup, PROXIMA_TOOL_ALLOW).as_deref(),
        lookup_non_empty(lookup, PROXIMA_TOOL_DENY).as_deref(),
        registered,
    )
}

fn resolve_served_surface(
    profile_name: Option<&str>,
    allow_raw: Option<&str>,
    deny_raw: Option<&str>,
    registered: &RegisteredKeys,
) -> Result<ServedSurface, CliError> {
    // Fail closed. With no PROXIMA_TOOL_PROFILE set, default to `memory` —
    // which excludes owner transfer and membership changes. Declared reads
    // remain served. Operators opt into the dangerous surface with
    // an explicit PROXIMA_TOOL_PROFILE=full.
    let profile = match profile_name {
        Some(name) => parse_tool_profile(name)?,
        None => ToolProfile::Memory,
    };
    let registered_ids = registered.all.as_slice();
    let allow = parse_tool_id_csv(allow_raw, PROXIMA_TOOL_ALLOW)?;
    let deny = parse_tool_id_csv(deny_raw, PROXIMA_TOOL_DENY)?;
    reject_unknown_tool_ids(&allow, registered_ids, PROXIMA_TOOL_ALLOW)?;
    reject_unknown_tool_ids(&deny, registered_ids, PROXIMA_TOOL_DENY)?;
    reject_served_read_denials(&deny, &registered.served_reads)?;

    if profile == ToolProfile::Full && allow.is_empty() && deny.is_empty() {
        tracing::warn!(
            "PROXIMA_TOOL_PROFILE=full advertises the whole tool surface, including \
             core_transfer (owner transfer of a memory series) and membership changes; \
             set PROXIMA_TOOL_PROFILE=memory for agent-facing hosts"
        );
        return Ok(ServedSurface::All);
    }

    let mut palette: BTreeSet<ScopeKey> = match profile {
        ToolProfile::Full => registered_ids.iter().cloned().collect(),
        ToolProfile::Memory => keep_set_keys(memory_keep_set()),
    };
    palette.extend(allow);
    for key in deny {
        palette.remove(&key);
    }
    Ok(ServedSurface::Palette(palette.into_iter().collect()))
}

fn parse_tool_profile(raw: &str) -> Result<ToolProfile, CliError> {
    match raw.trim() {
        protocol_profile::FULL => Ok(ToolProfile::Full),
        protocol_profile::MEMORY => Ok(ToolProfile::Memory),
        other => Err(CliError::Runtime(ProximaError::Config(format!(
            "unknown {PROXIMA_TOOL_PROFILE} {other:?}; expected \"{}\" or \"{}\"",
            protocol_profile::FULL,
            protocol_profile::MEMORY,
        )))),
    }
}

/// A keep set is code: constants of the tools, actions and resources it
/// names. They are keys by construction, and the profile tests resolve every
/// one of them.
fn keep_set_keys(ids: impl IntoIterator<Item = &'static str>) -> BTreeSet<ScopeKey> {
    ids.into_iter()
        .map(|id| ScopeKey::parse(id).expect("a keep-set id is a scope key"))
        .collect()
}

/// The comma-separated ids of `env_var`, each parsed into a [`ScopeKey`]
/// where it enters the process.
///
/// # Errors
///
/// A config error naming the entry that is not a scope key.
fn parse_tool_id_csv(raw: Option<&str>, env_var: &str) -> Result<Vec<ScopeKey>, CliError> {
    raw.map_or_else(
        || Ok(Vec::new()),
        |raw| {
            raw.split(',')
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .map(|id| {
                    ScopeKey::parse(id).map_err(|reason| {
                        CliError::Runtime(ProximaError::Config(format!(
                            "{env_var} contains invalid tool id {id:?}: {reason}"
                        )))
                    })
                })
                .collect()
        },
    )
}

/// Fail closed on a `PROXIMA_TOOL_ALLOW`/`PROXIMA_TOOL_DENY` entry that does
/// not name a registered tool/action/resource id in this build (e.g. a
/// typo). A silently-ignored unknown `DENY` entry is a fail-open deployment
/// bug: the operator believes a tool is disabled when it never was.
fn reject_unknown_tool_ids(
    ids: &[ScopeKey],
    registered_ids: &[ScopeKey],
    env_var: &str,
) -> Result<(), CliError> {
    let registered: HashSet<&ScopeKey> = registered_ids.iter().collect();
    for id in ids {
        if !registered.contains(id) {
            let id = id.to_string();
            return Err(CliError::Runtime(ProximaError::Config(format!(
                "{env_var} contains unknown tool id {id:?}; not registered in this build"
            ))));
        }
    }
    Ok(())
}

/// Refuse a deployment deny entry naming a mandatory declared read or resource.
/// Accepting it would imply the deployment had disabled an operation it serves.
fn reject_served_read_denials(
    deny: &[ScopeKey],
    served_reads: &[ScopeKey],
) -> Result<(), CliError> {
    match deny.iter().find(|id| served_reads.contains(id)) {
        Some(id) => Err(CliError::Runtime(ProximaError::Config(format!(
            "{PROXIMA_TOOL_DENY} contains {:?}, a mandatory read: a served surface narrows configured commands, never \
             declared reads, so denying it would disable nothing",
            id.to_string()
        )))),
        None => Ok(()),
    }
}

/// Build the embedding client from env, or `None` for degraded mode.
///
/// Any OpenAI-compatible `/embeddings` endpoint. Base URL and model are
/// explicit; hosted endpoints may also set `PROXIMA_EMBED_API_KEY`.
///
/// The model's vectors must be `PROXIMA_EMBED_DIM` wide (default 1024), and
/// that width must be one the store indexes (see
/// [`proxima_core::llm::EmbeddingDim`]). Set `PROXIMA_EMBED_MATRYOSHKA=true`
/// for a nested-prefix model (qwen3-embedding, text-embedding-3-*) so the
/// request asks for that width rather than the model's native one.
///
/// `PROXIMA_EMBED_MAX_INPUT_CHARS` bounds what is *sent*. It defaults to
/// 16,384 characters; set it explicitly when a provider needs a different
/// client-side limit. See docs/10 §Bounding embedding input.
fn embedding_client_from_env(
    lookup: impl Fn(&str) -> Option<String>,
    policy: EmbeddingRuntimePolicy,
) -> Result<Option<OpenAiCompatEmbeddingClient>, CliError> {
    // Parsed first so a wildcard or range in the setting fails boot whether
    // or not embeddings are configured.
    let plaintext_hosts = proxima_core::PlaintextHosts::from_lookup(&lookup)
        .map_err(|err| CliError::Runtime(ProximaError::Config(err.to_string())))?;
    let api_key = lookup_non_empty(&lookup, PROXIMA_EMBED_API_KEY);
    let base_url = lookup_non_empty(&lookup, PROXIMA_EMBED_BASE_URL);
    let model = embedding_model_from_env(&lookup);
    let matryoshka_is_set = lookup_non_empty(&lookup, PROXIMA_EMBED_MATRYOSHKA).is_some();
    let dim_is_set = lookup_non_empty(&lookup, PROXIMA_EMBED_DIM).is_some();
    let max_input_is_set = lookup_non_empty(&lookup, PROXIMA_EMBED_MAX_INPUT_CHARS).is_some();
    let query_instruction_is_set =
        lookup_non_empty(&lookup, PROXIMA_EMBED_QUERY_INSTRUCTION).is_some();

    if base_url.is_none()
        && model.is_none()
        && api_key.is_none()
        && !matryoshka_is_set
        && !dim_is_set
        && !max_input_is_set
        && !query_instruction_is_set
    {
        return Ok(None);
    }

    let base_url = base_url.ok_or_else(|| {
        CliError::Runtime(ProximaError::Config(
            "PROXIMA_EMBED_BASE_URL is required when any PROXIMA_EMBED_* setting is set".into(),
        ))
    })?;
    let model = model.ok_or_else(|| {
        CliError::Runtime(ProximaError::Config(
            "PROXIMA_EMBED_MODEL is required when any PROXIMA_EMBED_* setting is set".into(),
        ))
    })?;

    let matryoshka = parse_bool_env(&lookup, PROXIMA_EMBED_MATRYOSHKA)?;
    let dim = u32::try_from(embedding_dim_from_env(&lookup)?.width()).map_err(|_| {
        CliError::Runtime(ProximaError::Config(
            "embedding width does not fit u32".into(),
        ))
    })?;

    let max_input_chars = parse_positive_u32_env(&lookup, PROXIMA_EMBED_MAX_INPUT_CHARS)?
        .unwrap_or(DEFAULT_EMBED_MAX_INPUT_CHARS);
    let caps =
        proxima_core::models::EmbedCaps::new(dim, matryoshka).with_max_input_chars(max_input_chars);

    OpenAiCompatEmbeddingClient::new(
        model,
        caps,
        proxima_llm_openai_compat::OpenAiCompatConfig::new(base_url, api_key)
            .with_timeout(policy.request_timeout())
            .with_plaintext_hosts(plaintext_hosts),
    )
    .map(Some)
    .map_err(|err| CliError::Runtime(ProximaError::Config(err.to_string())))
}

/// Parse an optional positive integer setting.
///
/// Zero is refused rather than treated as "unset": a cap of zero would
/// reject every input, and the omitted variable uses the finite default.
fn parse_positive_u32_env(
    lookup: &impl Fn(&str) -> Option<String>,
    key: &'static str,
) -> Result<Option<NonZeroU32>, CliError> {
    let Some(raw) = lookup_non_empty(lookup, key) else {
        return Ok(None);
    };
    raw.parse::<NonZeroU32>().map(Some).map_err(|_| {
        CliError::Runtime(ProximaError::Config(format!(
            "{key} must be a positive integer, got {raw:?}"
        )))
    })
}

fn parse_bool_env(
    lookup: &impl Fn(&str) -> Option<String>,
    key: &'static str,
) -> Result<bool, CliError> {
    match lookup_non_empty(lookup, key) {
        None => Ok(false),
        Some(raw) => match raw.to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Ok(true),
            "0" | "false" | "no" | "off" => Ok(false),
            other => Err(CliError::Runtime(ProximaError::Config(format!(
                "{key} must be a boolean, got {other:?}"
            )))),
        },
    }
}

fn embedding_model_from_env(lookup: impl Fn(&str) -> Option<String>) -> Option<String> {
    lookup_non_empty(&lookup, PROXIMA_EMBED_MODEL)
}

/// `PROXIMA_EMBED_DIM`, or 1024 when unset.
fn embedding_dim_from_env(
    lookup: &impl Fn(&str) -> Option<String>,
) -> Result<proxima_core::llm::EmbeddingDim, CliError> {
    let Some(raw) = lookup_non_empty(lookup, PROXIMA_EMBED_DIM) else {
        return Ok(proxima_core::llm::EmbeddingDim::D1024);
    };
    let width = raw.parse::<usize>().map_err(|_| {
        CliError::Runtime(ProximaError::Config(format!(
            "{PROXIMA_EMBED_DIM} must be an integer, got {raw:?}"
        )))
    })?;
    proxima_core::llm::EmbeddingDim::try_from(width).map_err(|err| {
        CliError::Runtime(ProximaError::Config(format!("{PROXIMA_EMBED_DIM}: {err}")))
    })
}

/// The space a maintenance pass reconciles: `--model` or
/// `PROXIMA_EMBED_MODEL`, at `PROXIMA_EMBED_DIM`.
fn maintenance_embedding_space(
    explicit: Option<String>,
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<proxima_core::EmbeddingSpace, CliError> {
    let dim = embedding_dim_from_env(&lookup)?;
    let model = explicit
        .or_else(|| embedding_model_from_env(lookup))
        .ok_or_else(|| {
            CliError::Runtime(ProximaError::Config(
                "maintain-embeddings requires --model or PROXIMA_EMBED_MODEL".into(),
            ))
        })?;
    Ok(proxima_core::EmbeddingSpace::new(model, dim))
}

fn lookup_non_empty(lookup: &impl Fn(&str) -> Option<String>, key: &str) -> Option<String> {
    proxima_core::env_value(lookup, key)
}

#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error("help requested")]
    Help(&'static str),
    #[error(transparent)]
    Args(#[from] ArgsError),
    #[error(transparent)]
    Runtime(#[from] ProximaError),
    #[error("transport: {0}")]
    Transport(String),
}

impl CliError {
    #[must_use]
    pub fn is_help(&self) -> bool {
        match self {
            Self::Help(_) => true,
            Self::Args(args) => args.is_help(),
            Self::Runtime(_) | Self::Transport(_) => false,
        }
    }

    #[must_use]
    pub fn help_text(&self) -> Option<&'static str> {
        match self {
            Self::Help(usage) => Some(usage),
            Self::Args(args) if args.is_help() => Some(USAGE),
            Self::Args(_) | Self::Runtime(_) | Self::Transport(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use proxima_core::llm::EmbeddingClient;
    use proxima_core::protocol::tool as protocol_tool;
    use proxima_core::protocol::{action as protocol_action, resource as protocol_resource};

    use super::*;
    use proxima_core::ToolName;

    fn key(text: &str) -> ScopeKey {
        ScopeKey::parse(text).expect("a scope key")
    }

    fn tool(name: &str) -> ToolName {
        ToolName::parse(name).expect("a tool name")
    }

    #[test]
    fn the_purge_report_names_foreign_stores() {
        let mut outcome = ColdPurgeRetryOutcome::default();
        outcome.selected = 3;
        outcome.purged = 2;
        outcome.absent = 1;
        outcome.deferred = 4;
        outcome.foreign = 5;
        outcome.foreign_stores = vec![(
            proxima_core::ObjectStoreId::operator("other").expect("valid id"),
            5,
        )];
        let lines = cold_purge_retry_report(&outcome, "");
        assert_eq!(
            lines,
            vec![
                "cold-object-purge: selected=3 purged=2 absent=1 failed=0 due=0 deferred=4 \
                 stuck=0 foreign=5"
                    .to_owned(),
                "cold-object-purge: foreign store=s3:id/other debts=5".to_owned(),
            ]
        );
    }

    #[test]
    fn a_stuck_debt_is_named_with_its_owner_and_last_error() {
        let owner = proxima_core::OwnerId::from(uuid::Uuid::nil());
        assert_eq!(
            stuck_debt_line(
                "objects/k",
                Some(&owner),
                9,
                &"2026-10-19 20:00:00 +00:00:00",
                Some("access denied"),
            ),
            "cold-object-purge: stuck key=objects/k \
             owner=00000000-0000-0000-0000-000000000000 attempts=9 \
             next_attempt_at=2026-10-19 20:00:00 +00:00:00 last_error=access denied"
        );
        assert_eq!(
            stuck_debt_line("objects/k", None, 8, &"t", None),
            "cold-object-purge: stuck key=objects/k owner=- attempts=8 next_attempt_at=t \
             last_error=-"
        );
    }

    fn keys<const N: usize>(ids: [&str; N]) -> Vec<ScopeKey> {
        ids.map(key).to_vec()
    }

    /// The registry `registered_tool_ids` reads: this build's.
    fn frozen_registry() -> proxima_core::FlavorRegistryFrozen {
        let mut registry = FlavorRegistry::new();
        <ProximaMcpApp as FlavorBundle>::register(&mut registry).expect("register");
        registry.try_freeze().expect("freeze")
    }

    /// Every registered key served to an unrestricted caller, judged by the
    /// same combined policy and exact declaration as invocation.
    fn admitted(
        frozen: &proxima_core::FlavorRegistryFrozen,
        scope: &ServedSurface,
    ) -> BTreeSet<String> {
        let capabilities = proxima_core::CapabilitySet {
            tool_scope: proxima_core::ToolScope::All,
            served_surface: scope.clone(),
        };
        proxima_core::canonical_scope_keys(frozen)
            .into_iter()
            .filter(|key| match key {
                ScopeKey::Tool(tool) => capabilities.admits(
                    key,
                    frozen
                        .mcp_tool(tool.as_str())
                        .expect("registered")
                        .declaration(None),
                ),
                ScopeKey::Action { tool, action, .. } => capabilities.admits_action(
                    tool,
                    action,
                    frozen
                        .mcp_tool(tool.as_str())
                        .expect("registered")
                        .declaration(Some(action.as_str())),
                ),
                ScopeKey::Resource(resource)
                    if *resource == proxima_core::mcp::how_to_resource_key() =>
                {
                    capabilities.admits(key, proxima_core::mcp::how_to_resource_declaration())
                }
                ScopeKey::Resource(resource) => capabilities.admits(
                    key,
                    all_core_resources()
                        .find(|contract| contract.key() == *resource)
                        .expect("a core resource")
                        .declaration(),
                ),
            })
            .map(|key| key.to_string())
            .collect()
    }

    fn configured(surface: &ServedSurface, key: &ScopeKey) -> bool {
        match surface {
            ServedSurface::All => true,
            ServedSurface::Palette(keys) => keys.contains(key),
        }
    }

    fn configured_group(surface: &ServedSurface, tool: &proxima_core::ToolName) -> bool {
        match surface {
            ServedSurface::All => true,
            ServedSurface::Palette(keys) => keys.iter().any(|key| match key {
                ScopeKey::Tool(held) | ScopeKey::Action { tool: held, .. } => held == tool,
                ScopeKey::Resource(_) => false,
            }),
        }
    }

    /// Hand-built registered keys, none of them a read no palette narrows.
    fn registered(all: Vec<ScopeKey>) -> RegisteredKeys {
        RegisteredKeys {
            all,
            served_reads: Vec::new(),
        }
    }

    fn build_app(
        config: McpConfig,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Proxima<ProximaMcpApp>, CliError> {
        let platform = config.database_url.clone();
        super::build_app(config, |key| {
            lookup(key)
                .or_else(|| (key == "PROXIMA_PLATFORM_DATABASE_URL").then(|| platform.clone()))
                .or_else(|| (key == "PROXIMA_STORE_NUMBER").then(|| "0".into()))
        })
    }

    fn config() -> McpConfig {
        McpConfig {
            database_url: DEFAULT_DATABASE_URL.to_string(),
            bind: Some(DEFAULT_BIND.parse().expect("valid bind")),
        }
    }

    fn pairs<'a>(pairs: &'a [(&'a str, String)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| {
            pairs
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| value.clone())
        }
    }

    /// The maintenance commands migrate on the platform URL unless
    /// `PROXIMA_MIGRATION_DATABASE_URL` is set, and refuse one naming
    /// another role or database without echoing it.
    #[test]
    fn maintenance_urls_take_a_checked_migration_url() {
        let platform = "postgres://platform@pooler:6432/proxima".to_owned();
        let unset = [("PROXIMA_PLATFORM_DATABASE_URL", platform.clone())];
        let urls = maintenance_urls(&pairs(&unset)).expect("platform only");
        assert_eq!(urls.migrations(), platform);

        let direct = [
            ("PROXIMA_PLATFORM_DATABASE_URL", platform.clone()),
            (
                "PROXIMA_MIGRATION_DATABASE_URL",
                "postgres://platform@db:5432/proxima".to_owned(),
            ),
        ];
        let urls = maintenance_urls(&pairs(&direct)).expect("same role and database");
        assert_eq!(urls.migrations(), "postgres://platform@db:5432/proxima");
        assert_eq!(urls.platform, platform);

        for wrong in [
            "postgres://admin@db:5432/proxima",
            "postgres://platform@db:5432/other",
        ] {
            let env = [
                ("PROXIMA_PLATFORM_DATABASE_URL", platform.clone()),
                ("PROXIMA_MIGRATION_DATABASE_URL", wrong.to_owned()),
            ];
            let Err(error) = maintenance_urls(&pairs(&env)) else {
                panic!("{wrong} is refused");
            };
            let message = error.to_string();
            assert!(
                message.contains("PROXIMA_MIGRATION_DATABASE_URL") && !message.contains("db:5432"),
                "{message}"
            );
        }
    }

    /// `maintain-blobs` boots through the facade: with the migration URL on
    /// a port nothing listens on, its boot fails and nothing is migrated,
    /// though the platform URL works.
    #[tokio::test]
    async fn maintain_blobs_migrates_on_the_migration_url() {
        let database = proxima_pg_testkit::unique_db_name("proxima_mcp_blobs_migration_url");
        proxima_pg_testkit::create_db(&database)
            .await
            .expect("PG required for tests");
        let (runtime_url, platform_url) = proxima_pg_testkit::split_role_urls(&database)
            .await
            .expect("split roles");
        let mut unreachable = reqwest::Url::parse(&platform_url).expect("fixture URL");
        unreachable
            .set_port(Some(1))
            .expect("a postgres URL takes a port");
        let env = [
            ("PROXIMA_PLATFORM_DATABASE_URL", platform_url),
            ("PROXIMA_MIGRATION_DATABASE_URL", unreachable.to_string()),
        ];
        let s3 = S3RuntimeConfig::from_lookup(&pairs(&[
            ("PROXIMA_S3_BUCKET", "unused".to_owned()),
            ("PROXIMA_S3_REGION", "us-east-1".to_owned()),
            ("PROXIMA_S3_ACCESS_KEY_ID", "unused".to_owned()),
            ("PROXIMA_S3_SECRET_ACCESS_KEY", "unused".to_owned()),
            ("PROXIMA_S3_ENDPOINT_URL", "http://127.0.0.1:1".to_owned()),
        ]))
        .expect("S3 settings parse")
        .expect("a bucket is set");
        let booted = blob_maintenance_app(runtime_url, s3, &pairs(&env))
            .expect("the URLs pass the check")
            .store_number(0)
            .build()
            .await;
        let admin = sqlx::PgPool::connect(&proxima_pg_testkit::db_url(&database))
            .await
            .expect("admin pool");
        let migrated: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM pg_namespace WHERE nspname = 'proxima_core')",
        )
        .fetch_one(&admin)
        .await
        .expect("catalog read");
        admin.close().await;
        if let Ok(built) = booted {
            built.shutdown().await;
            panic!("boot must connect its migrations to the migration URL");
        }
        proxima_pg_testkit::drop_db(&database)
            .await
            .expect("drop db");
        assert!(!migrated, "nothing migrated over the platform URL");
    }

    /// `maintain-blobs` resolves with no environment: its app names the
    /// required tool scope, empty, and starts no listener.
    #[test]
    fn blob_maintenance_resolves_without_env_and_serves_nothing() {
        let (config, _) = BlobMaintenanceApp::configure(RuntimeBuilder::default())
            .database_url(DEFAULT_DATABASE_URL)
            .store_number(0)
            .platform_database_url(DEFAULT_DATABASE_URL)
            .resolve()
            .expect("maintain-blobs resolves");
        assert_eq!(config.served_surface, ServedSurface::Palette(Vec::new()));
        assert!(config.mcp.is_none());
    }

    /// Every id `registered_tool_ids` produces resolves back against the
    /// registry that produced it, and every registered tool contributes at
    /// least one — so a dispatcher is named by its leaves and a flat tool by
    /// its bare id, whoever registered it.
    ///
    /// The claim underneath is that the descriptor is the enumeration: a
    /// *flavor* dispatcher must contribute its leaves, so
    /// `PROXIMA_TOOL_ALLOW=flavor:action` resolves and
    /// `PROXIMA_TOOL_PROFILE=full` grants it leaf by leaf.
    #[test]
    fn registered_ids_carry_a_flavor_dispatchers_leaves() {
        let mut registry = FlavorRegistry::new();
        <ProximaMcpApp as FlavorBundle>::register(&mut registry).expect("register");
        let frozen = registry.try_freeze().expect("freeze");
        let ids = registered_tool_ids()
            .expect("ids derive from the same registry")
            .all;

        for id in &ids {
            match id {
                ScopeKey::Resource(resource)
                    if *resource == proxima_core::mcp::how_to_resource_key() =>
                {
                    assert_eq!(id.to_string(), protocol_resource::HOW_TO);
                    assert!(proxima_core::mcp::how_to_resource_declaration().is_read());
                }
                ScopeKey::Resource(_) => {
                    assert!(
                        all_core_resources().any(|resource| resource.scope_key == id.to_string()),
                        "{id} is a resource key no core resource declares",
                    );
                }
                ScopeKey::Action { tool, action, .. } => {
                    let descriptor = frozen
                        .mcp_tool(tool.as_str())
                        .unwrap_or_else(|| panic!("leaf {id} names an unregistered tool"));
                    assert!(
                        descriptor
                            .action_arg_specs
                            .iter()
                            .map(|spec| spec.action)
                            .chain(descriptor.argv_action_specs.iter().map(|spec| spec.action))
                            .any(|declared| declared == action.as_str()),
                        "leaf {id} names an action {tool} does not declare",
                    );
                }
                ScopeKey::Tool(tool) => {
                    let descriptor = frozen.mcp_tool(tool.as_str()).unwrap_or_else(|| {
                        panic!("{id} is neither a registered tool nor a resource")
                    });
                    assert!(
                        descriptor.action_arg_specs.is_empty()
                            && descriptor.argv_action_specs.is_empty(),
                        "{id} dispatches actions and must be named by its leaves, not bare",
                    );
                }
            }
        }

        for descriptor in frozen.list_mcp_tools() {
            let name = tool(descriptor.name);
            assert!(
                ids.iter().any(|id| match id {
                    ScopeKey::Tool(held) | ScopeKey::Action { tool: held, .. } => *held == name,
                    ScopeKey::Resource(_) => false,
                }),
                "{} is registered and contributes no scope id",
                descriptor.name,
            );
        }
    }

    #[tokio::test]
    async fn app_construction_rejects_invalid_embedding_runtime_policy() {
        for (key, value) in [
            (proxima_core::PROXIMA_EMBED_REQUEST_TIMEOUT_SECONDS, "0"),
            (proxima_core::PROXIMA_EMBED_BATCH_SIZE, "bad"),
            (proxima_core::PROXIMA_EMBED_WORKER_INTERVAL_SECONDS, "3601"),
            (
                proxima_core::PROXIMA_EMBED_STALE_CLAIM_TIMEOUT_SECONDS,
                "120",
            ),
        ] {
            let err = build_app(config(), |candidate| {
                (candidate == key).then(|| value.to_string())
            })
            .expect_err("invalid embedding runtime policy must fail before boot");
            assert!(err.to_string().contains(key), "{key}: {err}");
        }
    }

    #[tokio::test]
    async fn app_construction_resolves_pool_policy_from_its_injected_lookup() {
        let err = build_app(config(), |key| {
            (key == "PROXIMA_PG_MAX_CONNECTIONS").then(|| "many".to_string())
        })
        .expect_err("canonical app must resolve pool policy from this lookup");

        assert!(
            err.to_string().contains("PROXIMA_PG_MAX_CONNECTIONS=many"),
            "message: {err}"
        );
    }

    #[tokio::test]
    async fn oidc_env_wires_authenticator_and_metadata() {
        build_app(config(), |key| match key {
            "PROXIMA_OIDC_ISSUER" => Some("https://idp.test".into()),
            "PROXIMA_OIDC_AUDIENCE" => Some("proxima-mcp".into()),
            "PROXIMA_PUBLIC_URL" => Some("https://proxima.test".into()),
            "PROXIMA_OIDC_SUBJECT_MAP_JSON" => Some(format!(
                r#"[{{"iss":"https://idp.test","sub":"subject-1","user_id":"{}"}}]"#,
                uuid::Uuid::nil()
            )),
            "PROXIMA_OIDC_HTTP_TIMEOUT_SECONDS" => Some("17".into()),
            _ => None,
        })
        .expect("app builds with oidc env");
    }

    #[tokio::test]
    async fn oidc_env_rejects_invalid_http_timeout() {
        let err = build_app(config(), |key| match key {
            "PROXIMA_OIDC_ISSUER" => Some("https://idp.test".into()),
            "PROXIMA_OIDC_AUDIENCE" => Some("proxima-mcp".into()),
            "PROXIMA_PUBLIC_URL" => Some("https://proxima.test".into()),
            "PROXIMA_OIDC_SUBJECT_MAP_JSON" => Some(format!(
                r#"[{{"iss":"https://idp.test","sub":"subject-1","user_id":"{}"}}]"#,
                uuid::Uuid::nil()
            )),
            "PROXIMA_OIDC_HTTP_TIMEOUT_SECONDS" => Some("0".into()),
            _ => None,
        })
        .expect("builder accepts unresolved OIDC settings")
        .build()
        .await
        .expect_err("zero OIDC HTTP timeout must fail boot");
        assert!(
            err.to_string()
                .contains("PROXIMA_OIDC_HTTP_TIMEOUT_SECONDS"),
            "message: {err}"
        );
    }

    #[tokio::test]
    async fn oidc_env_wires_authenticator_with_issuer_aware_subject_map_json() {
        let subject_map_json = format!(
            r#"[{{"iss":"https://idp.test","sub":"subject-1","user_id":"{}"}}]"#,
            uuid::Uuid::nil()
        );
        build_app(config(), move |key| match key {
            "PROXIMA_OIDC_ISSUER" => Some("https://idp.test".into()),
            "PROXIMA_OIDC_AUDIENCE" => Some("proxima-mcp".into()),
            "PROXIMA_PUBLIC_URL" => Some("https://proxima.test".into()),
            "PROXIMA_OIDC_SUBJECT_MAP_JSON" => Some(subject_map_json.clone()),
            _ => None,
        })
        .expect("app builds with issuer-aware subject map json");
    }

    #[tokio::test]
    async fn oidc_env_requires_a_subject_map_when_issuer_is_configured() {
        let err = build_app(config(), |key| match key {
            "PROXIMA_OIDC_ISSUER" => Some("https://idp.test".into()),
            "PROXIMA_OIDC_AUDIENCE" => Some("proxima-mcp".into()),
            "PROXIMA_PUBLIC_URL" => Some("https://proxima.test".into()),
            _ => None,
        })
        .expect("builder accepts unresolved OIDC settings")
        .build()
        .await
        .expect_err("missing subject map must be rejected");
        assert!(
            err.to_string().contains("PROXIMA_OIDC_SUBJECT_MAP_JSON"),
            "message: {err}"
        );
    }

    #[tokio::test]
    async fn oidc_env_rejects_http_issuer_and_jwks_uri() {
        for insecure_key in ["PROXIMA_OIDC_ISSUER", "PROXIMA_OIDC_JWKS_URI"] {
            let result = build_app(config(), |key| match key {
                "PROXIMA_OIDC_ISSUER" => Some(if insecure_key == "PROXIMA_OIDC_ISSUER" {
                    "http://idp.test".into()
                } else {
                    "https://idp.test".into()
                }),
                "PROXIMA_OIDC_JWKS_URI" if insecure_key == "PROXIMA_OIDC_JWKS_URI" => {
                    Some("http://idp.test/keys".into())
                }
                "PROXIMA_OIDC_AUDIENCE" => Some("proxima-mcp".into()),
                "PROXIMA_PUBLIC_URL" => Some("https://proxima.test".into()),
                _ => None,
            })
            .expect("builder accepts unresolved OIDC settings")
            .build()
            .await;
            let Err(err) = result else {
                panic!("http oidc URL must be rejected");
            };
            assert!(err.to_string().contains("must use https"), "message: {err}");
        }
    }

    #[test]
    fn tool_profile_resolver_builds_deployment_scope() {
        let registered_ids = registered(keys([
            protocol_resource::MEMORY,
            protocol_resource::SCHEMAS,
            protocol_tool::CORE_SEARCH_MEMORIES,
            protocol_tool::CORE_MEMORY_SPACES,
            protocol_tool::CORE_MEMBERSHIP,
            protocol_tool::CORE_TRANSFER,
            protocol_action::CORE_MEMBERSHIP_ADD_MEMBER,
            protocol_action::CORE_MEMBERSHIP_REMOVE_MEMBER,
            protocol_action::CORE_MEMBERSHIP_LIST_MEMBERS,
            protocol_action::CORE_TRANSFER_TO_OWNER,
            protocol_action::CORE_FACT_CITATION_OF_FACT,
            protocol_action::CORE_FACT_FACTS_CITING_OBJECT,
            protocol_tool::CORE_FORGET,
            protocol_tool::CORE_RESTORE,
            protocol_action::CORE_GOAL_SET,
            "test-flavor_register_notebook",
            "test-flavor_emit_request",
        ]));

        // No PROXIMA_TOOL_PROFILE => fail-closed `memory` default, NOT full.
        let default_scope =
            resolve_served_surface(None, None, None, &registered_ids).expect("default profile");
        for (name, expected) in [
            (protocol_tool::CORE_SEARCH_MEMORIES, true),
            (protocol_tool::CORE_TRANSFER, false),
            (protocol_tool::CORE_MEMBERSHIP, false),
            (protocol_action::CORE_TRANSFER_TO_OWNER, false),
        ] {
            assert_eq!(
                configured(&default_scope, &key(name)),
                expected,
                "default: {name}"
            );
        }

        // Explicit `full` still yields the whole surface.
        let full =
            resolve_served_surface(Some(protocol_profile::FULL), None, None, &registered_ids)
                .expect("full profile");
        assert_eq!(full, ServedSurface::All);

        let memory =
            resolve_served_surface(Some(protocol_profile::MEMORY), None, None, &registered_ids)
                .expect("memory profile");
        for (name, expected) in [
            (protocol_tool::CORE_SEARCH_MEMORIES, true),
            (protocol_tool::CORE_MEMORY_SPACES, true),
            (protocol_resource::MEMORY, true),
            (protocol_resource::SCHEMAS, true),
            (protocol_action::CORE_FACT_CITATION_OF_FACT, true),
            (protocol_action::CORE_FACT_FACTS_CITING_OBJECT, true),
            (protocol_tool::CORE_FORGET, true),
            (protocol_tool::CORE_RESTORE, true),
            (protocol_tool::CORE_MEMBERSHIP, false),
            (protocol_tool::CORE_TRANSFER, false),
            (protocol_action::CORE_MEMBERSHIP_ADD_MEMBER, false),
            (protocol_action::CORE_MEMBERSHIP_REMOVE_MEMBER, false),
            (protocol_action::CORE_MEMBERSHIP_LIST_MEMBERS, false),
            (protocol_action::CORE_TRANSFER_TO_OWNER, false),
        ] {
            assert_eq!(configured(&memory, &key(name)), expected, "memory: {name}");
        }
        assert!(!configured_group(
            &memory,
            &tool(protocol_tool::CORE_MEMBERSHIP)
        ));
        assert!(!configured_group(
            &memory,
            &tool(protocol_tool::CORE_TRANSFER)
        ));

        // A flavor's tools are not in the memory keep set; an allow entry
        // adds one back.
        assert!(!configured(&memory, &key("test-flavor_register_notebook")));
        assert!(configured(&memory, &key(protocol_action::CORE_GOAL_SET)));
        assert!(!configured(&memory, &key("test-flavor_emit_request")));

        let overridden = resolve_served_surface(
            Some(protocol_profile::MEMORY),
            Some("test-flavor_emit_request"),
            Some(protocol_resource::MEMORY),
            &registered_ids,
        )
        .expect("overridden memory profile");
        assert!(!configured(&overridden, &key(protocol_resource::MEMORY)));
        assert!(configured(&overridden, &key("test-flavor_emit_request")));
    }

    /// `PROXIMA_TOOL_PROFILE=code` is a startup refusal that names the
    /// profiles there are, not a silent fallback to another palette.
    #[test]
    fn tool_profile_code_is_refused_naming_the_valid_profiles() {
        let err = resolve_served_surface(Some("code"), None, None, &RegisteredKeys::default())
            .expect_err("the code profile is gone");
        let message = err.to_string();
        assert!(
            message.contains(PROXIMA_TOOL_PROFILE)
                && message.contains("\"memory\"")
                && message.contains("\"full\"")
                && !message.contains("\"code\" or"),
            "{message}"
        );
    }

    /// The shipped host links no flavor: its frozen registry stores into
    /// the core schema only, under every feature set.
    #[test]
    fn the_default_host_links_no_flavor() {
        assert_eq!(
            proxima::host::composed_schema_names(&frozen_registry()),
            ["proxima_core"]
        );
    }

    #[test]
    fn unknown_tool_profile_fails_closed() {
        let err = resolve_served_surface(Some("unknown"), None, None, &RegisteredKeys::default())
            .expect_err("unknown profile");
        assert!(err.to_string().contains("unknown PROXIMA_TOOL_PROFILE"));
    }

    /// Deployment policy always serves declared reads, including Owner
    /// audience and resources, so a deny entry must fail at startup.
    #[test]
    fn a_denied_read_fails_closed() {
        let registered = registered_tool_ids().expect("registered ids");
        for read in [
            protocol_tool::CORE_SEARCH_MEMORIES,
            "core_upload:read_url",
            protocol_resource::MEMORY,
            protocol_resource::HOW_TO,
            protocol_action::CORE_MEMBERSHIP_LIST_MEMBERS,
        ] {
            let err = resolve_served_surface(None, None, Some(read), &registered)
                .expect_err("a denied read is refused");
            let message = err.to_string();
            assert!(
                message.contains(PROXIMA_TOOL_DENY) && message.contains(read),
                "{message}"
            );
        }
        assert!(
            registered
                .served_reads
                .contains(&key(protocol_action::CORE_MEMBERSHIP_LIST_MEMBERS))
        );
    }

    #[test]
    fn unknown_deny_entry_typo_fails_closed() {
        let registered_ids = registered(keys([protocol_tool::CORE_SEARCH_MEMORIES]));
        let err = resolve_served_surface(None, None, Some("core_memroy"), &registered_ids)
            .expect_err("typo'd deny entry must be rejected, not silently ignored");
        assert!(
            err.to_string().contains(PROXIMA_TOOL_DENY),
            "message: {err}"
        );
        assert!(err.to_string().contains("core_memroy"), "message: {err}");
    }

    #[test]
    fn unknown_allow_entry_typo_fails_closed() {
        let registered_ids = registered(keys([protocol_tool::CORE_SEARCH_MEMORIES]));
        let err = resolve_served_surface(
            Some(protocol_profile::MEMORY),
            Some("core_memroy"),
            None,
            &registered_ids,
        )
        .expect_err("typo'd allow entry must be rejected, not silently ignored");
        assert!(
            err.to_string().contains(PROXIMA_TOOL_ALLOW),
            "message: {err}"
        );
        assert!(err.to_string().contains("core_memroy"), "message: {err}");
    }

    /// An entry that is no scope key never reaches the registry comparison:
    /// it fails startup, naming the variable and the entry, as an unknown id
    /// does. Neighbouring valid entries do not rescue it.
    #[test]
    fn an_entry_that_is_not_a_scope_key_fails_startup_naming_it() {
        let registered_ids = registered(keys([
            protocol_tool::CORE_SEARCH_MEMORIES,
            protocol_action::CORE_GOAL_SET,
        ]));
        for bad in [
            "a:b:c",
            "core_goal:",
            ":set",
            ":",
            "resource:",
            "core goal:set",
            "core/goal",
            "core_goal:set:extra",
            "core_goal:\u{a0}set",
        ] {
            for (allow, deny, variable) in [
                (
                    Some(format!("core_search_memories, {bad}")),
                    None,
                    PROXIMA_TOOL_ALLOW,
                ),
                (
                    None,
                    Some(format!("{bad},core_search_memories")),
                    PROXIMA_TOOL_DENY,
                ),
            ] {
                let err = resolve_served_surface(
                    Some(protocol_profile::MEMORY),
                    allow.as_deref(),
                    deny.as_deref(),
                    &registered_ids,
                )
                .expect_err("an entry that is no key must fail startup");
                let message = err.to_string();
                assert!(message.contains(variable), "{bad:?}: {message}");
                assert!(message.contains("invalid tool id"), "{bad:?}: {message}");
                assert!(message.contains(&format!("{bad:?}")), "{bad:?}: {message}");
            }
        }

        // The same entry, set in the environment, stops the app from building.
        let lookup = |name: &str| {
            (name == PROXIMA_TOOL_ALLOW).then(|| "core_memory_spaces,a:b:c".to_string())
        };
        let Err(err) = build_app(config(), lookup) else {
            panic!("a malformed PROXIMA_TOOL_ALLOW entry must fail startup");
        };
        assert!(err.to_string().contains("a:b:c"), "{err}");
    }

    #[test]
    fn known_deny_entry_still_narrows_scope() {
        let registered_ids = registered(keys([
            protocol_tool::CORE_SEARCH_MEMORIES,
            protocol_tool::CORE_FORGET,
        ]));
        let scope = resolve_served_surface(
            None,
            None,
            Some(protocol_tool::CORE_FORGET),
            &registered_ids,
        )
        .expect("known deny entry is accepted");
        assert!(configured(
            &scope,
            &key(protocol_tool::CORE_SEARCH_MEMORIES)
        ));
        assert!(!configured(&scope, &key(protocol_tool::CORE_FORGET)));
    }

    /// Grouped write denials narrow invocation and listing; the membership
    /// read remains served independently of the configured command keys.
    #[test]
    fn grouped_action_deny_narrows_scope_and_advertisement() {
        let frozen = frozen_registry();
        let scope = resolve_served_surface(
            Some(protocol_profile::FULL),
            None,
            Some("core_membership:add_member,core_membership:remove_member,core_transfer:transfer_to_owner"),
            &registered_tool_ids().expect("registered ids"),
        )
        .expect("known grouped deny entries are accepted");

        let admitted = admitted(&frozen, &scope);
        let capabilities = proxima_core::CapabilitySet {
            tool_scope: proxima_core::ToolScope::All,
            served_surface: scope,
        };
        for tool in [protocol_tool::CORE_MEMBERSHIP, protocol_tool::CORE_TRANSFER] {
            let descriptor = frozen.mcp_tool(tool).expect("registered");
            let has_read = tool == protocol_tool::CORE_MEMBERSHIP;
            assert_eq!(
                descriptor.advertised_by(&capabilities),
                has_read,
                "{tool} listing"
            );
            for (action, _) in descriptor.actions() {
                let read =
                    format!("{tool}:{action}") == protocol_action::CORE_MEMBERSHIP_LIST_MEMBERS;
                assert_eq!(
                    descriptor.action_advertised_by(&capabilities, action),
                    read,
                    "{tool}:{action} listing"
                );
                assert_eq!(
                    admitted.contains(&format!("{tool}:{action}")),
                    read,
                    "{tool}:{action} admission"
                );
            }
        }
    }

    #[test]
    fn partial_embedding_config_fails_closed() {
        for (configured_key, value, missing_key) in [
            (
                PROXIMA_EMBED_BASE_URL,
                "https://embeddings.example/v1",
                PROXIMA_EMBED_MODEL,
            ),
            (PROXIMA_EMBED_MODEL, "hosted-embed", PROXIMA_EMBED_BASE_URL),
            (PROXIMA_EMBED_API_KEY, "secret", PROXIMA_EMBED_BASE_URL),
            (PROXIMA_EMBED_MATRYOSHKA, "false", PROXIMA_EMBED_BASE_URL),
            (PROXIMA_EMBED_DIM, "768", PROXIMA_EMBED_BASE_URL),
            (
                PROXIMA_EMBED_MAX_INPUT_CHARS,
                "4095",
                PROXIMA_EMBED_BASE_URL,
            ),
            (
                PROXIMA_EMBED_QUERY_INSTRUCTION,
                "Query:{query}",
                PROXIMA_EMBED_BASE_URL,
            ),
        ] {
            let err = embedding_client_from_env(
                |key| (key == configured_key).then(|| value.to_string()),
                EmbeddingRuntimePolicy::default(),
            )
            .expect_err("partial embedding configuration must not degrade silently");
            assert!(
                err.to_string().contains(missing_key),
                "{configured_key}: {err}"
            );
        }
    }

    /// The default instruction words every search query; `\n` is a newline,
    /// and a template without the placeholder fails boot naming its
    /// variable (#354).
    #[test]
    fn query_instructions_read_the_default() {
        let unset = query_instructions_from_env(&|_: &str| None).expect("unset configures");
        assert!(unset.is_empty());

        let default_only = query_instructions_from_env(&|key: &str| {
            (key == PROXIMA_EMBED_QUERY_INSTRUCTION)
                .then(|| "Instruct: find the note\\nQuery:{query}".to_string())
        })
        .expect("default configures");
        assert_eq!(
            default_only.render("q", QueryTask::DEFAULT),
            "Instruct: find the note\nQuery:q"
        );

        let missing = query_instructions_from_env(&|key: &str| {
            (key == PROXIMA_EMBED_QUERY_INSTRUCTION).then(|| "Instruct: no query".to_string())
        })
        .expect_err("a template without {query} must not configure");
        assert!(
            missing
                .to_string()
                .contains(PROXIMA_EMBED_QUERY_INSTRUCTION),
            "{missing}"
        );
    }

    #[test]
    fn embedding_input_cap_defaults_and_allows_override() {
        let configured = |max: Option<&'static str>| {
            move |key: &str| match key {
                PROXIMA_EMBED_BASE_URL => Some("http://127.0.0.1:11434/v1".to_string()),
                PROXIMA_EMBED_MODEL => Some("local-embed".to_string()),
                PROXIMA_EMBED_MAX_INPUT_CHARS => max.map(str::to_string),
                _ => None,
            }
        };

        let default_client =
            embedding_client_from_env(configured(None), EmbeddingRuntimePolicy::default())
                .expect("default cap configures")
                .expect("embedding client is configured");
        assert_eq!(
            default_client.caps().max_input_chars,
            NonZeroU32::new(16_384)
        );

        let override_client =
            embedding_client_from_env(configured(Some("8192")), EmbeddingRuntimePolicy::default())
                .expect("explicit cap configures")
                .expect("embedding client is configured");
        assert_eq!(
            override_client.caps().max_input_chars,
            NonZeroU32::new(8192)
        );
    }

    /// A cap the chunked rescue cannot satisfy must stop the process, not be
    /// clamped to something workable: the operator picked that number from
    /// their model's context, and silently substituting another means the
    /// running config is not the one they wrote.
    #[test]
    fn an_input_cap_below_the_rescue_floor_refuses_to_boot() {
        let too_low = proxima_core::llm::MIN_EMBED_INPUT_CAP_CHARS - 1;
        let err = embedding_client_from_env(
            |key| match key {
                PROXIMA_EMBED_BASE_URL => Some("http://127.0.0.1:11434/v1".to_string()),
                PROXIMA_EMBED_MODEL => Some("local-embed".to_string()),
                PROXIMA_EMBED_MAX_INPUT_CHARS => Some(too_low.to_string()),
                _ => None,
            },
            EmbeddingRuntimePolicy::default(),
        )
        .expect_err("a cap under the floor must not build");
        assert!(err.to_string().contains("terminal"), "{err}");
    }

    /// Zero is the interesting rejection: as a number it parses, and as a cap
    /// it refuses every input. The finite default remains active when this
    /// variable is omitted.
    #[test]
    fn an_input_cap_must_be_a_positive_integer() {
        for raw in ["0", "-1", "lots", "16k", ""] {
            let lookup =
                |key: &str| (key == PROXIMA_EMBED_MAX_INPUT_CHARS).then(|| raw.to_string());
            let parsed = parse_positive_u32_env(&lookup, PROXIMA_EMBED_MAX_INPUT_CHARS);
            if raw.is_empty() {
                // An empty value is an unset value everywhere else here.
                assert!(matches!(parsed, Ok(None)), "empty must read as unset");
            } else {
                assert!(parsed.is_err(), "{raw:?} must not parse as a cap");
            }
        }
    }

    /// A plaintext endpoint off the local host would put the API key on the
    /// wire, so the client must refuse to be built at all.
    #[test]
    fn remote_plaintext_endpoint_is_refused() {
        let err = embedding_client_from_env(
            |key| match key {
                PROXIMA_EMBED_BASE_URL => Some("http://embeddings.example/v1".to_string()),
                PROXIMA_EMBED_MODEL => Some("hosted-embed".to_string()),
                _ => None,
            },
            EmbeddingRuntimePolicy::default(),
        )
        .expect_err("remote plaintext must not build");
        assert!(err.to_string().contains("https"), "{err}");
    }

    /// `PROXIMA_PLAINTEXT_HOSTS` widens plaintext to exactly the named hosts:
    /// the sidecar is accepted, any other name stays refused, and a wildcard
    /// or range stops boot whether or not embeddings are configured.
    #[test]
    fn plaintext_hosts_admit_only_the_named_embedding_host() {
        let env = |base: Option<&'static str>, hosts: Option<&'static str>| {
            move |key: &str| match key {
                PROXIMA_EMBED_BASE_URL => base.map(str::to_string),
                PROXIMA_EMBED_MODEL => base.map(|_| "local-embed".to_string()),
                "PROXIMA_PLAINTEXT_HOSTS" => hosts.map(str::to_string),
                _ => None,
            }
        };
        let build = |base, hosts| {
            embedding_client_from_env(env(base, hosts), EmbeddingRuntimePolicy::default())
        };

        let client = build(Some("http://gateway:9090/v1"), Some("gateway"))
            .expect("named host builds")
            .expect("embedding client is configured");
        drop(client);
        for (base, hosts) in [
            ("http://gateway:9090/v1", None),
            ("http://other:9090/v1", Some("gateway")),
        ] {
            let err = build(Some(base), hosts).expect_err("unnamed host refused");
            assert!(err.to_string().contains("https"), "{base} {hosts:?}: {err}");
        }
        for hosts in ["*", "*.internal", "10.0.0.0/8", "10.0.0.1"] {
            let err = build(None, Some(hosts)).expect_err("bad setting fails boot");
            assert!(err.to_string().contains("PROXIMA_PLAINTEXT_HOSTS"), "{err}");
        }
        assert!(
            build(None, Some("gateway"))
                .expect("no embeddings")
                .is_none()
        );
        assert!(build(None, None).expect("unset").is_none());
    }

    #[test]
    fn matryoshka_flag_parses_and_rejects_garbage() {
        for raw in ["true", "1", "yes", "on"] {
            assert!(
                parse_bool_env(
                    &|key: &str| (key == PROXIMA_EMBED_MATRYOSHKA).then(|| raw.to_string()),
                    PROXIMA_EMBED_MATRYOSHKA
                )
                .expect("parses")
            );
        }
        assert!(
            !parse_bool_env(
                &|key: &str| (key == PROXIMA_EMBED_MATRYOSHKA).then(|| "false".to_string()),
                PROXIMA_EMBED_MATRYOSHKA
            )
            .expect("parses")
        );
        assert!(
            parse_bool_env(
                &|key: &str| (key == PROXIMA_EMBED_MATRYOSHKA).then(|| "maybe".to_string()),
                PROXIMA_EMBED_MATRYOSHKA
            )
            .is_err()
        );
    }

    #[test]
    fn maintenance_space_is_explicit_or_from_generic_env() {
        assert_eq!(
            maintenance_embedding_space(Some("flag-model".into()), |key| {
                (key == PROXIMA_EMBED_MODEL).then(|| "env-model".into())
            })
            .map(|space| space.model_id().to_owned())
            .expect("explicit model wins"),
            "flag-model"
        );
        let space = maintenance_embedding_space(None, |key| match key {
            PROXIMA_EMBED_MODEL => Some("env-model".into()),
            PROXIMA_EMBED_DIM => Some("768".into()),
            _ => None,
        })
        .expect("generic env model is accepted");
        assert_eq!(
            space,
            proxima_core::EmbeddingSpace::new("env-model", proxima_core::llm::EmbeddingDim::D768)
        );
        let err = maintenance_embedding_space(None, |_| None)
            .expect_err("there is no provider-specific default");
        assert!(err.to_string().contains(PROXIMA_EMBED_MODEL), "{err}");
    }

    /// The configured width is requested from the provider and must be a
    /// width the store indexes; anything else is refused at boot, not at
    /// the first insert.
    #[test]
    fn embed_dim_selects_a_lane_and_refuses_the_rest() {
        let with_dim = |dim: &'static str| {
            move |key: &str| match key {
                PROXIMA_EMBED_MODEL => Some("hosted-embed".to_string()),
                PROXIMA_EMBED_BASE_URL => Some("https://embeddings.example/v1".to_string()),
                PROXIMA_EMBED_DIM => Some(dim.to_string()),
                _ => None,
            }
        };
        for (raw, width) in [("768", 768), ("3072", 3072)] {
            let client =
                embedding_client_from_env(with_dim(raw), EmbeddingRuntimePolicy::default())
                    .expect("a lane width is accepted")
                    .expect("configured");
            assert_eq!(client.dim(), width);
        }
        for raw in ["512", "wide"] {
            let err = embedding_client_from_env(with_dim(raw), EmbeddingRuntimePolicy::default())
                .expect_err("a width with no lane is refused");
            assert!(err.to_string().contains(PROXIMA_EMBED_DIM), "{raw}: {err}");
        }
    }
}

pub mod args;

pub use args::{
    ArgsError, DEFAULT_BIND, DEFAULT_DATABASE_URL, MAINTAIN_USAGE, MaintainConfig, McpConfig,
    RETENTION_USAGE, ReconcileScope, RetentionConfig, USAGE, parse_args, parse_maintain_args,
    parse_retention_args,
};

use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;

use proxima::flavor::FlavorBundle;
use proxima::{
    AppContext, AppInfo, FlavorApp, Proxima, ProximaError, RunningProxima, RuntimeBuilder,
};
use proxima_core::mcp::McpToolExtensions;
use proxima_core::protocol::profile as protocol_profile;
use proxima_core::{
    FlavorRegistry, FlavorRegistryError, OwnerAccessPort, ToolScope, all_core_actions,
    all_core_resources, llm::EmbeddingClient,
};
use proxima_llm_openai_compat::{
    MISTRAL_EMBED_BASE_URL, MISTRAL_EMBED_MODEL, OpenAiCompatEmbeddingClient,
};
use proxima_storage_pg::{
    ChangeEventPruneOptions, ChangeEventPruneOutcome, EmbeddingReconcileOptions,
    EmbeddingReconcileScope, PgStorage, RetentionEnforceOptions, RetentionEnforceOutcome,
};

const MISTRAL_API_KEY: &str = "MISTRAL_API_KEY";
const PROXIMA_EMBED_MODEL: &str = "PROXIMA_EMBED_MODEL";
const MISTRAL_API_BASE: &str = "MISTRAL_API_BASE";
const PROXIMA_EMBED_BASE_URL: &str = "PROXIMA_EMBED_BASE_URL";
const PROXIMA_EMBED_API_KEY: &str = "PROXIMA_EMBED_API_KEY";
const PROXIMA_EMBED_MATRYOSHKA: &str = "PROXIMA_EMBED_MATRYOSHKA";
const PROXIMA_TOOL_PROFILE: &str = "PROXIMA_TOOL_PROFILE";
const PROXIMA_TOOL_ALLOW: &str = "PROXIMA_TOOL_ALLOW";
const PROXIMA_TOOL_DENY: &str = "PROXIMA_TOOL_DENY";

/// Tool/action scope keys advertised by the `memory` profile. Flat entries
/// reference the owning tool's `McpTool::NAME`; grouped tools contribute
/// action leaf keys from their manifest.
fn memory_keep_set() -> Vec<&'static str> {
    use proxima_core::mcp::McpTool;
    use proxima_core::mcp::core_tools::{
        CoreFactTool, CoreGoalTool, DeriveTool, LinkTool, MemorySpacesTool, RecordUtteranceTool,
        RememberTool, SearchMemoriesTool,
    };

    #[allow(unused_mut)]
    let mut ids = vec![
        // authoring
        RememberTool::NAME,
        DeriveTool::NAME,
        LinkTool::NAME,
        RecordUtteranceTool::NAME,
        // retrieval
        SearchMemoriesTool::NAME,
        MemorySpacesTool::NAME,
    ];
    // The memory profile carries the full goal lifecycle plus non-destructive
    // fact/citation actions. Retention/cleanup are host/config-only.
    // Retention/cleanup stay out.
    ids.extend(
        all_core_actions()
            .filter(|action| action.tool == CoreGoalTool::NAME || action.tool == CoreFactTool::NAME)
            .map(|action| action.scope_key),
    );
    ids.extend(all_core_resources().map(|resource| resource.scope_key));

    #[cfg(feature = "code")]
    {
        use proxima_code::mcp::open_file_revision::CodeOpenFileRevisionTool;
        use proxima_code::mcp::repos::{
            CodeIngestHeadSnapshotTool, CodeListReposTool, CodeRegisterRepoTool,
        };
        use proxima_code::mcp::search_chunks::CodeSearchChunksTool;
        use proxima_code::mcp::search_commits::CodeSearchCommitsTool;

        ids.extend([
            // code-as-memory
            CodeRegisterRepoTool::NAME,
            CodeListReposTool::NAME,
            CodeIngestHeadSnapshotTool::NAME,
            CodeSearchChunksTool::NAME,
            CodeOpenFileRevisionTool::NAME,
            CodeSearchCommitsTool::NAME,
        ]);
    }

    ids
}

#[cfg(feature = "code")]
type LinkedFlavors = (proxima_code::CodeFlavor,);
#[cfg(not(feature = "code"))]
type LinkedFlavors = ();

type OidcBundle = (
    Arc<dyn proxima_core::Authenticator>,
    proxima::ResourceServerMetadata,
);

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

    fn mcp_tool_extensions(ctx: &AppContext) -> McpToolExtensions {
        #[cfg(feature = "code")]
        {
            let mut extensions = McpToolExtensions::default();
            extensions.insert(proxima_code::CodeFlavorStore::from_backend_pool_for_host(
                ctx.clone_pool_for_host(),
            ));
            extensions
        }
        #[cfg(not(feature = "code"))]
        {
            let _ = ctx;
            McpToolExtensions::default()
        }
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
    if args.first().is_some_and(|arg| arg == "maintain-retention") {
        args.remove(0);
        let config = parse_retention_args(args).map_err(|err| match err {
            ArgsError::Help => CliError::Help(RETENTION_USAGE),
            other @ ArgsError::Invalid(_) => CliError::Args(other),
        })?;
        return run_maintain_retention(config).await;
    }
    if args
        .first()
        .is_some_and(|arg| arg == "reconcile-embeddings")
    {
        return Err(CliError::Args(ArgsError::Invalid(
            "reconcile-embeddings was renamed to maintain-embeddings in v0.0.7; \
             the pass now also sweeps orphans and prints a health report"
                .into(),
        )));
    }
    if args.first().is_some_and(|arg| arg == "serve") {
        args.remove(0);
    }
    let config = parse_args(args).map_err(|err| match err {
        ArgsError::Help => CliError::Help(USAGE),
        other @ ArgsError::Invalid(_) => CliError::Args(other),
    })?;
    // rmcp 2.x logs idle-session keep-alive expiry and the resulting
    // session-cleanup race at ERROR; both are clean lifecycle events
    // (`quit_reason=Closed`). Pin those targets to `warn` until rmcp
    // upstream lowers them.
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
        .mcp_addr
        .ok_or_else(|| CliError::Runtime(ProximaError::Mcp("MCP listener disabled".into())))?;
    tracing::info!(addr = %addr, "proxima-mcp listening; POST http://{addr}/mcp");
    let embedding_worker = running.spawn_embedding_worker(running.cancel.clone());
    let server_result = if let Some(server) = running.server {
        server
            .await
            .map_err(|err| CliError::Transport(err.to_string()))
    } else {
        Ok(())
    };
    running.cancel.cancel();
    if let Err(err) = embedding_worker.await {
        tracing::warn!(error = %err, "embedding worker join failed");
    }
    server_result?;
    Ok(())
}

/// One embedding self-healing pass: orphan sweep, reconcile enqueue,
/// optional inline drain, health report. Serialized across processes by a
/// Postgres advisory lock — an overlapping pass skips with exit 0 so cron
/// overlap is harmless by construction, not by scheduling discipline.
async fn run_maintain(config: MaintainConfig) -> Result<(), CliError> {
    let model = config
        .model
        .unwrap_or_else(|| active_embedding_model(|key| std::env::var(key).ok()));
    let storage = PgStorage::connect(&config.database_url)
        .await
        .map_err(|err| ProximaError::Storage(err.to_string()))?;
    storage
        .run_migrations()
        .await
        .map_err(|err| ProximaError::Storage(err.to_string()))?;

    let Some(_lock) = storage
        .try_embedding_maintenance_lock()
        .await
        .map_err(|err| ProximaError::Storage(err.to_string()))?
    else {
        println!("skipped: another embedding maintenance pass holds the lock");
        return Ok(());
    };

    let sweep = storage
        .sweep_orphan_embedding_rows()
        .await
        .map_err(|err| ProximaError::Storage(err.to_string()))?;
    println!(
        "orphan-sweep: embeddings={} heads={} jobs={}",
        sweep.embeddings_deleted, sweep.heads_deleted, sweep.jobs_deleted
    );

    let outcome = storage
        .reconcile_embeddings(EmbeddingReconcileOptions {
            model_id: &model,
            scope: reconcile_scope(config.scope),
            limit: config.limit,
        })
        .await
        .map_err(|err| ProximaError::Storage(err.to_string()))?;
    println!(
        "reconcile: scanned={} enqueued={} skipped={}",
        outcome.scanned, outcome.enqueued, outcome.skipped
    );

    if config.drain {
        let client =
            embedding_client_from_env(|key| std::env::var(key).ok())?.ok_or_else(|| {
                CliError::Runtime(ProximaError::Config(
                    "maintain-embeddings --drain requires an embedding endpoint: set \
                 PROXIMA_EMBED_BASE_URL (local, e.g. http://127.0.0.1:11434/v1) \
                 or PROXIMA_EMBED_API_KEY / MISTRAL_API_KEY (hosted)"
                        .into(),
                ))
            })?;
        if client.model_id() != model {
            return Err(CliError::Runtime(ProximaError::Config(format!(
                "maintain-embeddings --drain model mismatch: queued model {model:?}, Mistral client model {:?}",
                client.model_id()
            ))));
        }
        let limit = config.limit.unwrap_or(i64::MAX);
        let drain = storage
            .drain_embedding_jobs_inline(&client, limit)
            .await
            .map_err(|err| ProximaError::Storage(err.to_string()))?;
        println!("drain: embedded={} failed={}", drain.embedded, drain.failed);
    }

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

/// One retention pass: Fact-retention enforcement (tombstone sweep) and/or
/// `change_event` pruning, per the explicit action flags. Serialized across
/// processes by its own Postgres advisory lock — an overlapping pass skips
/// with exit 0, like `maintain-embeddings`. Owners under an active
/// legal/security hold are skipped inside each owner's transaction and
/// surface in the report.
async fn run_maintain_retention(config: RetentionConfig) -> Result<(), CliError> {
    let storage = PgStorage::connect(&config.database_url)
        .await
        .map_err(|err| ProximaError::Storage(err.to_string()))?;
    storage
        .run_migrations()
        .await
        .map_err(|err| ProximaError::Storage(err.to_string()))?;

    let Some(_lock) = storage
        .try_retention_maintenance_lock()
        .await
        .map_err(|err| ProximaError::Storage(err.to_string()))?
    else {
        println!("skipped: another retention maintenance pass holds the lock");
        return Ok(());
    };

    let dry_run_suffix = if config.dry_run { " (dry-run)" } else { "" };

    if config.enforce_fact_retention {
        let outcome = storage
            .enforce_fact_retention(RetentionEnforceOptions {
                batch_size: config.batch_size,
                dry_run: config.dry_run,
            })
            .await
            .map_err(|err| ProximaError::Storage(err.to_string()))?;
        print_retention_report(&outcome, dry_run_suffix);
    }

    if let Some(older_than_seconds) = config.prune_change_events_older_than_seconds {
        let outcome = storage
            .prune_change_events(ChangeEventPruneOptions {
                older_than_seconds,
                batch_size: config.batch_size,
                dry_run: config.dry_run,
            })
            .await
            .map_err(|err| ProximaError::Storage(err.to_string()))?;
        print_prune_report(&outcome, older_than_seconds, dry_run_suffix);
    }

    Ok(())
}

fn print_retention_report(outcome: &RetentionEnforceOutcome, dry_run_suffix: &str) {
    println!(
        "fact-retention: owners={} tombstoned={} skipped-hold={}{dry_run_suffix}",
        outcome.owners.len(),
        outcome.facts_tombstoned,
        outcome.owners_skipped_hold
    );
    for owner in &outcome.owners {
        let hold = if owner.skipped_legal_hold {
            " [legal hold — skipped]"
        } else {
            ""
        };
        println!(
            "  {} retention={}s tombstoned={}{hold}",
            owner.owner.external_key(),
            owner.retention_seconds,
            owner.facts_tombstoned
        );
    }
}

fn print_prune_report(
    outcome: &ChangeEventPruneOutcome,
    older_than_seconds: i64,
    dry_run_suffix: &str,
) {
    println!(
        "change-event-prune: horizon={older_than_seconds}s owners={} pruned={} skipped-hold={}{dry_run_suffix}",
        outcome.owners.len(),
        outcome.events_pruned,
        outcome.owners_skipped_hold
    );
    for owner in &outcome.owners {
        let hold = if owner.skipped_legal_hold {
            " [legal hold — skipped]"
        } else {
            ""
        };
        println!(
            "  {} pruned={}{hold}",
            owner.owner.external_key(),
            owner.events_pruned
        );
    }
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
    let app = build_app(config, |key| std::env::var(key).ok())?;
    app.run().await.map_err(Into::into)
}

fn build_app(
    config: McpConfig,
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<Proxima<ProximaMcpApp>, CliError> {
    let registered_ids = registered_tool_ids()?;
    let tool_scope = tool_scope_from_env(&lookup, &registered_ids)?;
    let owner_access: Arc<dyn OwnerAccessPort> = Arc::new(
        proxima_storage_pg::PgOwnerAccessResolver::connect_lazy(&config.database_url)
            .map_err(|err| CliError::Runtime(ProximaError::Storage(err.to_string())))?,
    );
    let oidc = oidc_from_env(&lookup, owner_access.clone())?;
    let mut app = Proxima::<ProximaMcpApp>::app()
        .from_env()
        .database_url(config.database_url)
        .owner_access(owner_access)
        .tool_scope(tool_scope);
    if let Some(bind) = config.bind {
        app = app.mcp_bind(bind);
    }
    if let Some((authenticator, metadata)) = oidc {
        app = app.authenticator(authenticator).resource_metadata(metadata);
    }
    if let Some(client) = embedding_client_from_env(&lookup)? {
        app = app.embed_client(Arc::new(client));
    }
    Ok(app)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolProfile {
    Full,
    Memory,
}

fn registered_tool_ids() -> Result<Vec<&'static str>, CliError> {
    let mut registry = FlavorRegistry::new();
    <ProximaMcpApp as FlavorBundle>::register(&mut registry)
        .map_err(|err| CliError::Runtime(ProximaError::Registry(err)))?;
    let frozen = registry
        .try_freeze()
        .map_err(|err| CliError::Runtime(ProximaError::Registry(err)))?;
    let mut ids = Vec::new();
    for tool in frozen.list_mcp_tools() {
        let mut added_actions = false;
        for action in all_core_actions().filter(|action| action.tool == tool.name) {
            ids.push(action.scope_key);
            added_actions = true;
        }
        if !added_actions {
            ids.push(tool.name);
        }
    }
    ids.extend(all_core_resources().map(|resource| resource.scope_key));
    Ok(ids)
}

fn tool_scope_from_env(
    lookup: &impl Fn(&str) -> Option<String>,
    registered_ids: &[&str],
) -> Result<ToolScope, CliError> {
    resolve_tool_scope(
        lookup_non_empty(lookup, PROXIMA_TOOL_PROFILE).as_deref(),
        lookup_non_empty(lookup, PROXIMA_TOOL_ALLOW).as_deref(),
        lookup_non_empty(lookup, PROXIMA_TOOL_DENY).as_deref(),
        registered_ids,
    )
}

fn resolve_tool_scope(
    profile_name: Option<&str>,
    allow_raw: Option<&str>,
    deny_raw: Option<&str>,
    registered_ids: &[&str],
) -> Result<ToolScope, CliError> {
    // Fail closed. With no PROXIMA_TOOL_PROFILE set,
    // default to `memory` — which excludes core_publish (irreversible owner
    // transfer to World) and core_membership — instead of the old `full`
    // default that advertised those to every token. Operators opt into the
    // dangerous surface with an explicit PROXIMA_TOOL_PROFILE=full.
    let profile = match profile_name {
        Some(name) => parse_tool_profile(name)?,
        None => ToolProfile::Memory,
    };
    let allow = parse_tool_id_csv(allow_raw);
    let deny = parse_tool_id_csv(deny_raw);
    reject_unknown_tool_ids(&allow, registered_ids, PROXIMA_TOOL_ALLOW)?;
    reject_unknown_tool_ids(&deny, registered_ids, PROXIMA_TOOL_DENY)?;

    if profile == ToolProfile::Full && allow.is_empty() && deny.is_empty() {
        tracing::warn!(
            "PROXIMA_TOOL_PROFILE=full advertises the whole tool surface, including \
             core_publish (irreversible owner transfer to World) and core_membership; \
             set PROXIMA_TOOL_PROFILE=memory for agent-facing hosts"
        );
        return Ok(ToolScope::All);
    }

    let mut palette: BTreeSet<String> = match profile {
        ToolProfile::Full => registered_ids.iter().map(|id| (*id).to_string()).collect(),
        ToolProfile::Memory => memory_keep_set().into_iter().map(String::from).collect(),
    };
    palette.extend(allow);
    for id in deny {
        palette.remove(&id);
    }
    Ok(ToolScope::Palette(palette.into_iter().collect()))
}

fn parse_tool_profile(raw: &str) -> Result<ToolProfile, CliError> {
    match raw.trim() {
        protocol_profile::FULL => Ok(ToolProfile::Full),
        protocol_profile::MEMORY => Ok(ToolProfile::Memory),
        other => Err(CliError::Runtime(ProximaError::Config(format!(
            "unknown {PROXIMA_TOOL_PROFILE} {other:?}; expected \"{}\" or \"{}\"",
            protocol_profile::FULL,
            protocol_profile::MEMORY
        )))),
    }
}

fn parse_tool_id_csv(raw: Option<&str>) -> Vec<String> {
    raw.map_or_else(Vec::new, |raw| {
        raw.split(',')
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(ToOwned::to_owned)
            .collect()
    })
}

/// Fail closed on a `PROXIMA_TOOL_ALLOW`/`PROXIMA_TOOL_DENY` entry that does
/// not name a registered tool/action/resource id in this build (e.g. a
/// typo). A silently-ignored unknown `DENY` entry is a fail-open deployment
/// bug: the operator believes a tool is disabled when it never was.
fn reject_unknown_tool_ids(
    ids: &[String],
    registered_ids: &[&str],
    env_var: &str,
) -> Result<(), CliError> {
    let registered: HashSet<&str> = registered_ids.iter().copied().collect();
    for id in ids {
        if !registered.contains(id.as_str()) {
            return Err(CliError::Runtime(ProximaError::Config(format!(
                "{env_var} contains unknown tool id {id:?}; not registered in this build"
            ))));
        }
    }
    Ok(())
}

fn oidc_from_env(
    lookup: &impl Fn(&str) -> Option<String>,
    owner_access: Arc<dyn OwnerAccessPort>,
) -> Result<Option<OidcBundle>, CliError> {
    let Some(issuer) = lookup_non_empty(lookup, "PROXIMA_OIDC_ISSUER") else {
        return Ok(None);
    };
    let audience = lookup_non_empty(lookup, "PROXIMA_OIDC_AUDIENCE").ok_or_else(|| {
        CliError::Runtime(ProximaError::Config(
            "PROXIMA_OIDC_ISSUER set without PROXIMA_OIDC_AUDIENCE".into(),
        ))
    })?;
    let public_url = lookup_non_empty(lookup, "PROXIMA_PUBLIC_URL").ok_or_else(|| {
        CliError::Runtime(ProximaError::Config(
            "PROXIMA_OIDC_ISSUER set without PROXIMA_PUBLIC_URL".into(),
        ))
    })?;
    let jwks_uri = lookup_non_empty(lookup, "PROXIMA_OIDC_JWKS_URI");
    let allowed_subjects = lookup_non_empty(lookup, "PROXIMA_OIDC_ALLOWED_SUBJECTS").map(|raw| {
        raw.split(',')
            .map(str::trim)
            .filter(|subject| !subject.is_empty())
            .map(ToOwned::to_owned)
            .collect::<std::collections::HashSet<String>>()
    });
    let oidc_config = proxima_auth_oidc::OidcAuthConfig {
        issuer: issuer.clone(),
        jwks_uri,
        audience,
        allowed_subjects,
        leeway_secs: 60,
    };
    // Validate the JWKS/issuer boundary before touching the subject map or
    // storage, matching the pre-split ordering so an insecure-URL rejection
    // still short-circuits (see `oidc_env_rejects_http_issuer_and_jwks_uri`).
    let resolver =
        proxima_auth_oidc::HttpJwksResolver::new(issuer.clone(), oidc_config.jwks_uri.clone())
            .map_err(|err| CliError::Runtime(ProximaError::Config(err.to_string())))?;

    let subject_map = subject_map_from_env(lookup, &issuer)?;
    // The pool connects lazily: constructing it here never touches the
    // network, so this stays safe to call from synchronous unit tests that
    // never open a real Postgres connection.
    let authn = proxima_auth_oidc::OidcAuthenticator::new(
        oidc_config,
        Arc::new(resolver),
        subject_map,
        owner_access,
    )
    .map_err(|err| CliError::Runtime(ProximaError::Config(err.to_string())))?;
    let metadata = proxima::ResourceServerMetadata {
        public_url,
        authorization_servers: vec![issuer],
    };
    Ok(Some((Arc::new(authn), metadata)))
}

const PROXIMA_OIDC_SUBJECT_MAP_JSON: &str = "PROXIMA_OIDC_SUBJECT_MAP_JSON";
const PROXIMA_OIDC_SUBJECT_MAP: &str = "PROXIMA_OIDC_SUBJECT_MAP";

/// Parse the issuer-aware subject map from env. Exactly one of
/// `PROXIMA_OIDC_SUBJECT_MAP_JSON` (issuer-aware) or `PROXIMA_OIDC_SUBJECT_MAP`
/// (legacy single-issuer shorthand, bound to `issuer`) must be set whenever
/// `PROXIMA_OIDC_ISSUER` is configured.
fn subject_map_from_env(
    lookup: &impl Fn(&str) -> Option<String>,
    issuer: &str,
) -> Result<proxima_auth_oidc::OidcSubjectMap, CliError> {
    let json_raw = lookup_non_empty(lookup, PROXIMA_OIDC_SUBJECT_MAP_JSON);
    let legacy_raw = lookup_non_empty(lookup, PROXIMA_OIDC_SUBJECT_MAP);
    match (json_raw, legacy_raw) {
        (Some(_), Some(_)) => Err(CliError::Runtime(ProximaError::Config(format!(
            "{PROXIMA_OIDC_SUBJECT_MAP_JSON} and {PROXIMA_OIDC_SUBJECT_MAP} are mutually exclusive"
        )))),
        (Some(json), None) => proxima_auth_oidc::OidcSubjectMap::from_json(&json).map_err(|err| {
            CliError::Runtime(ProximaError::Config(format!(
                "{PROXIMA_OIDC_SUBJECT_MAP_JSON}: {err}"
            )))
        }),
        (None, Some(legacy)) => {
            proxima_auth_oidc::OidcSubjectMap::from_legacy_shorthand(&legacy, &[issuer.to_string()])
                .map_err(|err| {
                    CliError::Runtime(ProximaError::Config(format!(
                        "{PROXIMA_OIDC_SUBJECT_MAP}: {err}"
                    )))
                })
        }
        (None, None) => Err(CliError::Runtime(ProximaError::Config(format!(
            "PROXIMA_OIDC_ISSUER set without {PROXIMA_OIDC_SUBJECT_MAP_JSON} or {PROXIMA_OIDC_SUBJECT_MAP}"
        )))),
    }
}

/// Build the embedding client from env, or `None` for degraded mode.
///
/// The endpoint is any OpenAI-compatible `/embeddings` service. A hosted one
/// needs a key; a locally-hosted one (Ollama, llama.cpp, LM Studio, vLLM)
/// needs only a base URL, which is why the key is not what switches
/// embeddings on — `PROXIMA_EMBED_BASE_URL` alone is enough. Requiring a key
/// would mean inventing a fake one to run fully local, and the resulting
/// `MISTRAL_API_KEY=unused` in a local config is a lie about what the
/// deployment talks to.
///
/// `MISTRAL_API_KEY` / `MISTRAL_API_BASE` stay accepted as aliases so
/// existing deployments keep working unchanged.
///
/// Whatever the model, it must land in the substrate's single embedding
/// space: `proxima_core::llm::EMBEDDING_DIM` (1024), which is the width of
/// the `vector(1024)` column. Set `PROXIMA_EMBED_MATRYOSHKA=true` for a
/// nested-prefix model (qwen3-embedding, text-embedding-3-*) so the request
/// asks for 1024 rather than the model's native width.
fn embedding_client_from_env(
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<Option<OpenAiCompatEmbeddingClient>, CliError> {
    let api_key = lookup_non_empty(&lookup, PROXIMA_EMBED_API_KEY)
        .or_else(|| lookup_non_empty(&lookup, MISTRAL_API_KEY));
    let base_url = lookup_non_empty(&lookup, PROXIMA_EMBED_BASE_URL)
        .or_else(|| lookup_non_empty(&lookup, MISTRAL_API_BASE));

    // Degraded mode only when neither a key nor an endpoint was configured:
    // an operator who named an endpoint asked for embeddings.
    let base_url = match (base_url, api_key.as_ref()) {
        (Some(url), _) => url,
        (None, Some(_)) => MISTRAL_EMBED_BASE_URL.to_string(),
        (None, None) => return Ok(None),
    };

    let model = active_embedding_model(&lookup);
    let matryoshka = parse_bool_env(&lookup, PROXIMA_EMBED_MATRYOSHKA)?;
    let dim = u32::try_from(proxima_core::llm::EMBEDDING_DIM).map_err(|_| {
        CliError::Runtime(ProximaError::Config(
            "EMBEDDING_DIM does not fit u32".into(),
        ))
    })?;

    OpenAiCompatEmbeddingClient::new(
        model,
        proxima_core::models::EmbedCaps { dim, matryoshka },
        proxima_llm_openai_compat::OpenAiCompatConfig::new(base_url, api_key),
    )
    .map(Some)
    .map_err(|err| CliError::Runtime(ProximaError::Config(err.to_string())))
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

fn active_embedding_model(lookup: impl Fn(&str) -> Option<String>) -> String {
    lookup_non_empty(&lookup, PROXIMA_EMBED_MODEL)
        .unwrap_or_else(|| MISTRAL_EMBED_MODEL.to_string())
}

fn lookup_non_empty(lookup: &impl Fn(&str) -> Option<String>, key: &str) -> Option<String> {
    lookup(key).and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
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

    fn config() -> McpConfig {
        McpConfig {
            database_url: DEFAULT_DATABASE_URL.to_string(),
            bind: Some(DEFAULT_BIND.parse().expect("valid bind")),
        }
    }

    /// The "Tools:" section in the static help text is hand-written (the
    /// help path is `&'static str` end to end and must never fail), so
    /// this test pins it to the registry: adding or removing a core tool
    /// without updating `USAGE` fails here instead of silently drifting.
    /// Flavor tools stay out of the comparison — the static text cannot
    /// know which optional flavors (e.g. `--features code`) a build
    /// carries, so it lists the always-present substrate surface only.
    #[test]
    fn usage_tools_section_matches_registry() {
        use proxima_core::mcp::McpToolOrigin;

        let mut registry = FlavorRegistry::new();
        <ProximaMcpApp as FlavorBundle>::register(&mut registry).expect("register");
        let frozen = registry.try_freeze().expect("freeze");
        let mut expected: Vec<&str> = frozen
            .list_mcp_tools()
            .iter()
            .filter(|tool| matches!(tool.origin, McpToolOrigin::Substrate))
            .map(|tool| tool.name)
            .collect();
        expected.sort_unstable();

        let section = USAGE
            .split("Tools:\n")
            .nth(1)
            .expect("USAGE carries a Tools section");
        let mut listed: Vec<&str> = section
            .lines()
            .take_while(|line| line.starts_with("  "))
            .map(str::trim)
            .collect();
        listed.sort_unstable();
        assert_eq!(
            listed, expected,
            "USAGE Tools section drifted from the registry"
        );
    }

    // `#[tokio::test]`, not `#[test]`: `PgOwnerAccessResolver::connect_lazy`
    // constructs a `sqlx::PgPool`, which needs an active Tokio context even
    // though it defers the actual network connect (sqlx panics with "this
    // functionality requires a Tokio context" otherwise).
    #[tokio::test]
    async fn app_construction_without_mistral_key_keeps_degraded_mode() {
        build_app(config(), |_| None).expect("app construction does not require embeddings");
    }

    #[tokio::test]
    async fn old_reconcile_subcommand_names_the_rename() {
        let err = run(["reconcile-embeddings".to_string()])
            .await
            .expect_err("renamed subcommand must fail with guidance");
        assert!(
            err.to_string().contains("maintain-embeddings"),
            "message: {err}"
        );
    }

    #[tokio::test]
    async fn oidc_env_wires_authenticator_and_metadata() {
        build_app(config(), |key| match key {
            "PROXIMA_OIDC_ISSUER" => Some("https://idp.test".into()),
            "PROXIMA_OIDC_AUDIENCE" => Some("proxima-mcp".into()),
            "PROXIMA_PUBLIC_URL" => Some("https://proxima.test".into()),
            "PROXIMA_OIDC_SUBJECT_MAP" => Some(format!("subject-1:{}", uuid::Uuid::nil())),
            _ => None,
        })
        .expect("app builds with oidc env");
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
        .expect_err("missing subject map must be rejected");
        assert!(
            err.to_string().contains("PROXIMA_OIDC_SUBJECT_MAP"),
            "message: {err}"
        );
    }

    #[tokio::test]
    async fn oidc_env_rejects_both_subject_map_formats_at_once() {
        let err = build_app(config(), |key| match key {
            "PROXIMA_OIDC_ISSUER" => Some("https://idp.test".into()),
            "PROXIMA_OIDC_AUDIENCE" => Some("proxima-mcp".into()),
            "PROXIMA_PUBLIC_URL" => Some("https://proxima.test".into()),
            "PROXIMA_OIDC_SUBJECT_MAP" => Some(format!("subject-1:{}", uuid::Uuid::nil())),
            "PROXIMA_OIDC_SUBJECT_MAP_JSON" => Some("[]".into()),
            _ => None,
        })
        .expect_err("both subject map formats at once must be rejected");
        assert!(
            err.to_string().contains("mutually exclusive"),
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
            });
            let Err(err) = result else {
                panic!("http oidc URL must be rejected");
            };
            assert!(err.to_string().contains("must use https"), "message: {err}");
        }
    }

    #[test]
    fn tool_profile_resolver_builds_deployment_scope() {
        let registered_ids = [
            protocol_resource::MEMORY,
            protocol_resource::SCHEMAS,
            protocol_tool::CORE_SEARCH_MEMORIES,
            protocol_tool::CORE_MEMORY_SPACES,
            protocol_tool::CORE_MEMBERSHIP,
            protocol_tool::CORE_PUBLISH,
            protocol_action::CORE_MEMBERSHIP_ADD_MEMBER,
            protocol_action::CORE_MEMBERSHIP_REMOVE_MEMBER,
            protocol_action::CORE_MEMBERSHIP_LIST_MEMBERS,
            protocol_action::CORE_PUBLISH_TO_WORLD,
            protocol_action::CORE_FACT_CITATION_OF_FACT,
            protocol_action::CORE_FACT_CITATION_OF_ENTITY_HEAD,
            protocol_action::CORE_FACT_FACTS_CITING_OBJECT,
            protocol_action::CORE_GOAL_SET,
            "proxima-code_register_repo",
            "proxima-code_emit_execution_request",
        ];

        // No PROXIMA_TOOL_PROFILE => fail-closed `memory` default, NOT full.
        let default_scope =
            resolve_tool_scope(None, None, None, &registered_ids).expect("default profile");
        assert!(default_scope.allows(protocol_tool::CORE_SEARCH_MEMORIES));
        assert!(!default_scope.allows(protocol_tool::CORE_PUBLISH));
        assert!(!default_scope.allows(protocol_tool::CORE_MEMBERSHIP));
        assert!(!default_scope.allows(protocol_action::CORE_PUBLISH_TO_WORLD));

        // Explicit `full` still yields the whole surface.
        let full = resolve_tool_scope(Some(protocol_profile::FULL), None, None, &registered_ids)
            .expect("full profile");
        assert_eq!(full, ToolScope::All);

        let memory =
            resolve_tool_scope(Some(protocol_profile::MEMORY), None, None, &registered_ids)
                .expect("memory profile");
        assert!(memory.allows(protocol_tool::CORE_SEARCH_MEMORIES));
        assert!(memory.allows(protocol_tool::CORE_MEMORY_SPACES));
        assert!(memory.allows(protocol_resource::MEMORY));
        assert!(memory.allows(protocol_resource::SCHEMAS));
        assert!(memory.allows(protocol_action::CORE_FACT_CITATION_OF_FACT));
        assert!(memory.allows(protocol_action::CORE_FACT_CITATION_OF_ENTITY_HEAD));
        assert!(memory.allows(protocol_action::CORE_FACT_FACTS_CITING_OBJECT));
        assert!(!memory.allows(protocol_tool::CORE_MEMBERSHIP));
        assert!(!memory.allows(protocol_tool::CORE_PUBLISH));
        assert!(!memory.allows(protocol_action::CORE_MEMBERSHIP_ADD_MEMBER));
        assert!(!memory.allows(protocol_action::CORE_MEMBERSHIP_REMOVE_MEMBER));
        assert!(!memory.allows(protocol_action::CORE_MEMBERSHIP_LIST_MEMBERS));
        assert!(!memory.allows(protocol_action::CORE_PUBLISH_TO_WORLD));
        assert!(!memory.allows_group_advertisement(protocol_tool::CORE_MEMBERSHIP));
        assert!(!memory.allows_group_advertisement(protocol_tool::CORE_PUBLISH));

        // Code-flavor tools join the memory keep set only when the `code`
        // flavor is compiled in (the keep set references their `NAME`
        // consts under the same cfg).
        #[cfg(feature = "code")]
        assert!(memory.allows("proxima-code_register_repo"));
        assert!(memory.allows(protocol_action::CORE_GOAL_SET));
        assert!(!memory.allows("proxima-code_emit_execution_request"));

        let overridden = resolve_tool_scope(
            Some(protocol_profile::MEMORY),
            Some("proxima-code_emit_execution_request"),
            Some(protocol_resource::MEMORY),
            &registered_ids,
        )
        .expect("overridden memory profile");
        assert!(!overridden.allows(protocol_resource::MEMORY));
        assert!(overridden.allows("proxima-code_emit_execution_request"));
    }

    #[test]
    fn unknown_tool_profile_fails_closed() {
        let err =
            resolve_tool_scope(Some("unknown"), None, None, &[]).expect_err("unknown profile");
        assert!(err.to_string().contains("unknown PROXIMA_TOOL_PROFILE"));
    }

    #[test]
    fn unknown_deny_entry_typo_fails_closed() {
        let registered_ids = [protocol_tool::CORE_SEARCH_MEMORIES];
        let err = resolve_tool_scope(None, None, Some("core_memroy"), &registered_ids)
            .expect_err("typo'd deny entry must be rejected, not silently ignored");
        assert!(
            err.to_string().contains(PROXIMA_TOOL_DENY),
            "message: {err}"
        );
        assert!(err.to_string().contains("core_memroy"), "message: {err}");
    }

    #[test]
    fn unknown_allow_entry_typo_fails_closed() {
        let registered_ids = [protocol_tool::CORE_SEARCH_MEMORIES];
        let err = resolve_tool_scope(
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

    #[test]
    fn known_deny_entry_still_narrows_scope() {
        let registered_ids = [
            protocol_tool::CORE_SEARCH_MEMORIES,
            protocol_tool::CORE_MEMORY_SPACES,
        ];
        let scope = resolve_tool_scope(
            None,
            None,
            Some(protocol_tool::CORE_MEMORY_SPACES),
            &registered_ids,
        )
        .expect("known deny entry is accepted");
        assert!(scope.allows(protocol_tool::CORE_SEARCH_MEMORIES));
        assert!(!scope.allows(protocol_tool::CORE_MEMORY_SPACES));
    }

    #[test]
    fn grouped_action_deny_narrows_scope_and_advertisement() {
        let registered_ids = [
            protocol_action::CORE_MEMBERSHIP_ADD_MEMBER,
            protocol_action::CORE_MEMBERSHIP_REMOVE_MEMBER,
            protocol_action::CORE_MEMBERSHIP_LIST_MEMBERS,
            protocol_action::CORE_PUBLISH_TO_WORLD,
        ];
        let scope = resolve_tool_scope(
            None,
            None,
            Some("core_membership:add_member,core_membership:remove_member,core_membership:list_members,core_publish:publish_to_world"),
            &registered_ids,
        )
        .expect("known grouped deny entries are accepted");

        assert!(!scope.allows(protocol_action::CORE_MEMBERSHIP_ADD_MEMBER));
        assert!(!scope.allows(protocol_action::CORE_MEMBERSHIP_REMOVE_MEMBER));
        assert!(!scope.allows(protocol_action::CORE_MEMBERSHIP_LIST_MEMBERS));
        assert!(!scope.allows(protocol_action::CORE_PUBLISH_TO_WORLD));
        assert!(!scope.allows_group_advertisement(protocol_tool::CORE_MEMBERSHIP));
        assert!(!scope.allows_group_advertisement(protocol_tool::CORE_PUBLISH));
    }

    #[test]
    fn mistral_client_is_secret_gated_and_configurable() {
        let client = embedding_client_from_env(|key| match key {
            MISTRAL_API_KEY => Some("secret".to_string()),
            PROXIMA_EMBED_MODEL => Some("custom-mistral-embed".to_string()),
            MISTRAL_API_BASE => Some("https://mistral.example/v1".to_string()),
            _ => None,
        })
        .expect("client construction succeeds")
        .expect("secret enables client");

        assert_eq!(client.model_id(), "custom-mistral-embed");
        assert_eq!(client.dim(), proxima_core::llm::EMBEDDING_DIM);
    }

    /// The local-first path: a loopback endpoint and no credential at all.
    /// Requiring a key here would force `MISTRAL_API_KEY=unused` into every
    /// fully-local config.
    #[test]
    fn local_endpoint_needs_no_api_key() {
        let client = embedding_client_from_env(|key| match key {
            PROXIMA_EMBED_BASE_URL => Some("http://127.0.0.1:11434/v1".to_string()),
            PROXIMA_EMBED_MODEL => Some("qwen3-embedding:0.6b".to_string()),
            _ => None,
        })
        .expect("client construction succeeds")
        .expect("a base URL alone enables embeddings");

        assert_eq!(client.model_id(), "qwen3-embedding:0.6b");
        assert_eq!(client.dim(), proxima_core::llm::EMBEDDING_DIM);
    }

    #[test]
    fn no_endpoint_and_no_key_stays_degraded() {
        assert!(
            embedding_client_from_env(|_| None)
                .expect("no config is not an error")
                .is_none()
        );
    }

    /// `PROXIMA_EMBED_*` is the provider-neutral spelling; the `MISTRAL_*`
    /// names remain as aliases so existing deployments keep working.
    #[test]
    fn proxima_embed_vars_win_over_the_mistral_aliases() {
        let client = embedding_client_from_env(|key| match key {
            PROXIMA_EMBED_BASE_URL => Some("http://127.0.0.1:11434/v1".to_string()),
            PROXIMA_EMBED_API_KEY => Some("preferred".to_string()),
            MISTRAL_API_BASE => Some("https://mistral.example/v1".to_string()),
            MISTRAL_API_KEY => Some("legacy".to_string()),
            PROXIMA_EMBED_MODEL => Some("local-model".to_string()),
            _ => None,
        })
        .expect("client construction succeeds")
        .expect("configured");
        assert_eq!(client.model_id(), "local-model");
    }

    /// A plaintext endpoint off the local host would put the API key on the
    /// wire, so the client must refuse to be built at all.
    #[test]
    fn remote_plaintext_endpoint_is_refused() {
        let err = embedding_client_from_env(|key| match key {
            PROXIMA_EMBED_BASE_URL => Some("http://embeddings.example/v1".to_string()),
            _ => None,
        })
        .expect_err("remote plaintext must not build");
        assert!(err.to_string().contains("https"), "{err}");
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
    fn active_embedding_model_uses_same_env_default_as_mistral_client() {
        assert_eq!(active_embedding_model(|_| None), MISTRAL_EMBED_MODEL);
        assert_eq!(
            active_embedding_model(|key| (key == PROXIMA_EMBED_MODEL).then(|| "custom".into())),
            "custom"
        );
    }
}

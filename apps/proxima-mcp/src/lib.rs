pub mod args;

pub use args::{
    ArgsError, DEFAULT_BIND, DEFAULT_DATABASE_URL, McpConfig, RECONCILE_USAGE, ReconcileConfig,
    ReconcileScope, USAGE, parse_args, parse_reconcile_args,
};

use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;

use proxima::{
    AppInfo, FlavorApp, FlavorBundle, Proxima, ProximaError, RunningProxima, RuntimeBuilder,
};
use proxima_core::{
    FlavorRegistry, ToolScope, all_core_actions, all_core_resources, llm::EmbeddingClient,
};
use proxima_llm_openai_compat::{
    MISTRAL_EMBED_BASE_URL, MISTRAL_EMBED_MODEL, OpenAiCompatEmbeddingClient,
};
use proxima_storage_pg::{EmbeddingReconcileOptions, EmbeddingReconcileScope, PgStorage};

const MISTRAL_API_KEY: &str = "MISTRAL_API_KEY";
const PROXIMA_EMBED_MODEL: &str = "PROXIMA_EMBED_MODEL";
const MISTRAL_API_BASE: &str = "MISTRAL_API_BASE";
const PROXIMA_TOOL_PROFILE: &str = "PROXIMA_TOOL_PROFILE";
const PROXIMA_TOOL_ALLOW: &str = "PROXIMA_TOOL_ALLOW";
const PROXIMA_TOOL_DENY: &str = "PROXIMA_TOOL_DENY";
const CORE_FACT_TOMBSTONE_SCOPE_KEY: &str = "core_fact:tombstone";

/// Tool/action scope keys advertised by the `memory` profile. Flat entries
/// reference the owning tool's `McpTool::NAME`; grouped tools contribute
/// action leaf keys from their manifest.
fn memory_keep_set() -> Vec<&'static str> {
    use proxima_core::mcp::McpTool;
    use proxima_core::mcp::core_tools::{
        CoreFactTool, CoreGoalTool, DeriveTool, LinkTool, RecordUtteranceTool, RememberTool,
        SearchMemoriesTool,
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
    ];
    // The memory profile carries the full goal lifecycle plus non-destructive
    // fact/citation actions. Retention/cleanup are host/config-only.
    // Wake/personality admin stays out.
    ids.extend(
        all_core_actions()
            .filter(|action| {
                (action.tool == CoreGoalTool::NAME || action.tool == CoreFactTool::NAME)
                    && action.scope_key != CORE_FACT_TOMBSTONE_SCOPE_KEY
            })
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
    fn register(registry: &mut FlavorRegistry) {
        <LinkedFlavors as FlavorBundle>::register(registry);
    }

    fn register_pg_sidecars(registry: &mut proxima::PgSidecarRegistry) {
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
}

/// # Errors
///
/// Returns argument, facade boot, or MCP transport failures.
pub async fn run<I: IntoIterator<Item = String>>(args: I) -> Result<(), CliError> {
    let mut args: Vec<String> = args.into_iter().collect();
    if args
        .first()
        .is_some_and(|arg| arg == "reconcile-embeddings")
    {
        args.remove(0);
        let config = parse_reconcile_args(args).map_err(|err| match err {
            ArgsError::Help => CliError::Help(RECONCILE_USAGE),
            other => CliError::Args(other),
        })?;
        return run_reconcile(config).await;
    }
    if args.first().is_some_and(|arg| arg == "serve") {
        args.remove(0);
    }
    let config = parse_args(args).map_err(|err| match err {
        ArgsError::Help => CliError::Help(USAGE),
        other => CliError::Args(other),
    })?;
    // rmcp 1.6 logs idle-session keep-alive expiry and the resulting
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

async fn run_reconcile(config: ReconcileConfig) -> Result<(), CliError> {
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
    let outcome = storage
        .reconcile_embeddings(EmbeddingReconcileOptions {
            model_id: &model,
            scope: reconcile_scope(config.scope),
            limit: config.limit,
        })
        .await
        .map_err(|err| ProximaError::Storage(err.to_string()))?;

    println!(
        "scanned={} enqueued={} skipped={}",
        outcome.scanned, outcome.enqueued, outcome.skipped
    );

    if config.drain {
        let client = mistral_embedding_client(|key| std::env::var(key).ok())?.ok_or_else(|| {
            CliError::Runtime(ProximaError::Config(
                "reconcile-embeddings --drain requires MISTRAL_API_KEY".into(),
            ))
        })?;
        if client.model_id() != model {
            return Err(CliError::Runtime(ProximaError::Config(format!(
                "reconcile-embeddings --drain model mismatch: queued model {model:?}, Mistral client model {:?}",
                client.model_id()
            ))));
        }
        let limit = config.limit.unwrap_or(i64::MAX);
        let drain = storage
            .drain_embedding_jobs_inline(&client, limit)
            .await
            .map_err(|err| ProximaError::Storage(err.to_string()))?;
        println!("embedded={} failed={}", drain.embedded, drain.failed);
    }
    Ok(())
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
    let registered_ids = registered_tool_ids();
    let tool_scope = tool_scope_from_env(&lookup, &registered_ids)?;
    let oidc = oidc_from_env(&config, &lookup)?;
    let mut app = Proxima::<ProximaMcpApp>::app()
        .from_env()
        .database_url(config.database_url)
        .owner(config.owner.clone())
        .tool_scope(tool_scope);
    if let Some(bind) = config.bind {
        app = app.mcp_bind(bind);
    }
    if let Some(token) = config.master_token {
        app = app.master_token(token.to_string());
    }
    if let Some((authenticator, metadata)) = oidc {
        app = app.authenticator(authenticator).resource_metadata(metadata);
    }
    if let Some(client) = mistral_embedding_client(&lookup)? {
        app = app.embed_client(Arc::new(client));
    }
    Ok(app)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolProfile {
    Full,
    Memory,
}

fn registered_tool_ids() -> Vec<&'static str> {
    let mut registry = FlavorRegistry::new();
    <ProximaMcpApp as FlavorBundle>::register(&mut registry);
    let frozen = registry.freeze();
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
    ids
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
    let profile = parse_tool_profile(profile_name.unwrap_or("full"))?;
    let allow = parse_tool_id_csv(allow_raw);
    let deny = parse_tool_id_csv(deny_raw);
    warn_unknown_tool_ids(&allow, registered_ids, PROXIMA_TOOL_ALLOW);
    warn_unknown_tool_ids(&deny, registered_ids, PROXIMA_TOOL_DENY);

    if profile == ToolProfile::Full && allow.is_empty() && deny.is_empty() {
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
        "full" => Ok(ToolProfile::Full),
        "memory" => Ok(ToolProfile::Memory),
        other => Err(CliError::Runtime(ProximaError::Config(format!(
            "unknown {PROXIMA_TOOL_PROFILE} {other:?}; expected \"full\" or \"memory\""
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

fn warn_unknown_tool_ids(ids: &[String], registered_ids: &[&str], env_var: &str) {
    let registered: HashSet<&str> = registered_ids.iter().copied().collect();
    for id in ids {
        if !registered.contains(id.as_str()) {
            tracing::warn!(env_var, tool_id = %id, "tool id is not registered in this build");
        }
    }
}

fn oidc_from_env(
    config: &McpConfig,
    lookup: &impl Fn(&str) -> Option<String>,
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
        owner: config.owner.clone(),
        allowed_subjects,
        leeway_secs: 60,
    };
    let resolver =
        proxima_auth_oidc::HttpJwksResolver::new(issuer.clone(), oidc_config.jwks_uri.clone())
            .map_err(|err| CliError::Runtime(ProximaError::Config(err.to_string())))?;
    let authn = proxima_auth_oidc::OidcAuthenticator::new(oidc_config, Arc::new(resolver))
        .map_err(|err| CliError::Runtime(ProximaError::Config(err.to_string())))?;
    let metadata = proxima::ResourceServerMetadata {
        public_url,
        authorization_servers: vec![issuer],
    };
    Ok(Some((Arc::new(authn), metadata)))
}

fn mistral_embedding_client(
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<Option<OpenAiCompatEmbeddingClient>, CliError> {
    let Some(api_key) = lookup_non_empty(&lookup, MISTRAL_API_KEY) else {
        return Ok(None);
    };
    let model = lookup_non_empty(&lookup, PROXIMA_EMBED_MODEL)
        .unwrap_or_else(|| MISTRAL_EMBED_MODEL.to_string());
    let base_url = lookup_non_empty(&lookup, MISTRAL_API_BASE)
        .unwrap_or_else(|| MISTRAL_EMBED_BASE_URL.to_string());
    OpenAiCompatEmbeddingClient::mistral(api_key, model, base_url)
        .map(Some)
        .map_err(|err| CliError::Runtime(ProximaError::Config(err.to_string())))
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

    use super::*;

    fn config() -> McpConfig {
        McpConfig {
            database_url: DEFAULT_DATABASE_URL.to_string(),
            owner: proxima_core::Principal::User(proxima_core::UserId::new(uuid::Uuid::nil())),
            bind: Some(DEFAULT_BIND.parse().expect("valid bind")),
            master_token: Some(uuid::Uuid::nil()),
        }
    }

    #[test]
    fn app_construction_without_mistral_key_keeps_degraded_mode() {
        build_app(config(), |_| None).expect("app construction does not require embeddings");
    }

    #[test]
    fn oidc_env_wires_authenticator_and_metadata() {
        build_app(config(), |key| match key {
            "PROXIMA_OIDC_ISSUER" => Some("https://idp.test".into()),
            "PROXIMA_OIDC_AUDIENCE" => Some("proxima-mcp".into()),
            "PROXIMA_PUBLIC_URL" => Some("https://proxima.test".into()),
            _ => None,
        })
        .expect("app builds with oidc env");
    }

    #[test]
    fn oidc_env_rejects_http_issuer_and_jwks_uri() {
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
    fn memory_keep_set_excludes_tombstone_but_keeps_fact_reads() {
        let keep = memory_keep_set();
        assert!(!keep.contains(&CORE_FACT_TOMBSTONE_SCOPE_KEY));
        assert!(keep.contains(&"core_fact:citation_of_fact"));
        assert!(keep.contains(&"core_fact:citation_of_entity_head"));
        assert!(keep.contains(&"core_fact:facts_citing_object"));
    }

    #[test]
    fn tool_profile_resolver_builds_deployment_scope() {
        let registered_ids = [
            "resource:memory",
            "resource:schemas",
            "core_search_memories",
            "core_fact:citation_of_fact",
            "core_fact:citation_of_entity_head",
            "core_fact:facts_citing_object",
            "core_fact:tombstone",
            "core_personality:instantiate",
            "core_wake:add",
            "proxima-code_register_repo",
            "proxima-code_emit_execution_request",
        ];

        let full = resolve_tool_scope(None, None, None, &registered_ids).expect("full profile");
        assert_eq!(full, ToolScope::All);

        let memory = resolve_tool_scope(Some("memory"), None, None, &registered_ids)
            .expect("memory profile");
        assert!(memory.allows("core_search_memories"));
        assert!(memory.allows("resource:memory"));
        assert!(memory.allows("resource:schemas"));
        assert!(memory.allows("core_fact:citation_of_fact"));
        assert!(memory.allows("core_fact:citation_of_entity_head"));
        assert!(memory.allows("core_fact:facts_citing_object"));
        assert!(!memory.allows(CORE_FACT_TOMBSTONE_SCOPE_KEY));
        // Code-flavor tools join the memory keep set only when the `code`
        // flavor is compiled in (the keep set references their `NAME`
        // consts under the same cfg).
        #[cfg(feature = "code")]
        assert!(memory.allows("proxima-code_register_repo"));
        assert!(!memory.allows("core_personality:instantiate"));
        assert!(!memory.allows("core_wake:add"));
        assert!(!memory.allows("proxima-code_emit_execution_request"));

        let overridden = resolve_tool_scope(
            Some("memory"),
            Some("core_wake:add"),
            Some("resource:memory"),
            &registered_ids,
        )
        .expect("overridden memory profile");
        assert!(!overridden.allows("resource:memory"));
        assert!(overridden.allows("core_wake:add"));
    }

    #[test]
    fn unknown_tool_profile_fails_closed() {
        let err =
            resolve_tool_scope(Some("unknown"), None, None, &[]).expect_err("unknown profile");
        assert!(err.to_string().contains("unknown PROXIMA_TOOL_PROFILE"));
    }

    #[test]
    fn mistral_client_is_secret_gated_and_configurable() {
        let client = mistral_embedding_client(|key| match key {
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

    #[test]
    fn active_embedding_model_uses_same_env_default_as_mistral_client() {
        assert_eq!(active_embedding_model(|_| None), MISTRAL_EMBED_MODEL);
        assert_eq!(
            active_embedding_model(|key| (key == PROXIMA_EMBED_MODEL).then(|| "custom".into())),
            "custom"
        );
    }
}

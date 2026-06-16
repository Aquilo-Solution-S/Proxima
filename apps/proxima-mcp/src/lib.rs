pub mod args;

pub use args::{ArgsError, DEFAULT_BIND, DEFAULT_DATABASE_URL, McpConfig, USAGE, parse_args};

use std::sync::Arc;

use proxima::{
    AppInfo, FlavorApp, FlavorBundle, Proxima, ProximaError, RunningProxima, RuntimeBuilder,
};
use proxima_core::FlavorRegistry;
use proxima_llm_openai_compat::{
    MISTRAL_EMBED_BASE_URL, MISTRAL_EMBED_MODEL, OpenAiCompatEmbeddingClient,
};

const MISTRAL_API_KEY: &str = "MISTRAL_API_KEY";
const PROXIMA_EMBED_MODEL: &str = "PROXIMA_EMBED_MODEL";
const MISTRAL_API_BASE: &str = "MISTRAL_API_BASE";

#[derive(Debug)]
pub struct ProximaMcpApp;

impl FlavorBundle for ProximaMcpApp {
    fn register(_registry: &mut FlavorRegistry) {}

    fn migrators() -> Vec<proxima::NamedMigrator> {
        Vec::new()
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
    let config = parse_args(args)?;
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
    if let Some(server) = running.server {
        server
            .await
            .map_err(|err| CliError::Transport(err.to_string()))?;
    }
    Ok(())
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
    let mut app = Proxima::<ProximaMcpApp>::app()
        .from_env()
        .database_url(config.database_url)
        .owner(config.owner)
        .mcp_bind(config.bind);
    if let Some(token) = config.master_token {
        app = app.master_token(token.to_string());
    }
    if let Some(client) = mistral_embedding_client(lookup)? {
        app = app.embed_client(Arc::new(client));
    }
    Ok(app)
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

fn lookup_non_empty(lookup: &impl Fn(&str) -> Option<String>, key: &str) -> Option<String> {
    lookup(key).and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

#[derive(Debug, thiserror::Error)]
pub enum CliError {
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
        matches!(self, Self::Args(args) if args.is_help())
    }
}

#[cfg(test)]
mod tests {
    use proxima_core::llm::EmbeddingClient;

    use super::*;

    fn config() -> McpConfig {
        McpConfig {
            database_url: DEFAULT_DATABASE_URL.to_string(),
            owner: proxima_core::Owner {
                principal: proxima_core::Principal::User(proxima_core::UserId::new(
                    uuid::Uuid::nil(),
                )),
                org_id: proxima_core::OrgId::new(uuid::Uuid::nil()),
            },
            bind: DEFAULT_BIND.parse().expect("valid bind"),
            master_token: Some(uuid::Uuid::nil()),
        }
    }

    #[test]
    fn app_construction_without_mistral_key_keeps_degraded_mode() {
        build_app(config(), |_| None).expect("app construction does not require embeddings");
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
}

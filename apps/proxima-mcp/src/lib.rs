pub mod args;

pub use args::{ArgsError, DEFAULT_BIND, DEFAULT_DATABASE_URL, McpConfig, USAGE, parse_args};

use proxima::{
    AppInfo, FlavorApp, FlavorBundle, Proxima, ProximaError, RunningProxima, RuntimeBuilder,
};
use proxima_core::FlavorRegistry;

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
    let mut app = Proxima::<ProximaMcpApp>::app()
        .from_env()
        .database_url(config.database_url)
        .owner(config.owner)
        .mcp_bind(config.bind);
    if let Some(token) = config.master_token {
        app = app.master_token(token.to_string());
    }
    app.run().await.map_err(Into::into)
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

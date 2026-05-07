pub mod args;

pub use args::{ArgsError, McpConfig, USAGE, parse_args};
pub use proxima_mcp_server::{
    DevMcpServer, McpServerError, ToolInvocationError, default_allowlist, serve_streamable_http,
};

use proxima_core::FlavorRegistry;

/// # Errors
///
/// Returns argument, storage, migration, or MCP transport failures.
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

    let (handle, addr) = run_with_handle(config).await?;
    tracing::info!(addr = %addr, "proxima-mcp listening; POST http://{addr}/mcp");
    handle
        .await
        .map_err(|err| CliError::Transport(err.to_string()))??;
    Ok(())
}

/// # Errors
///
/// Returns argument, storage, migration, bind, or transport failures.
pub async fn run_with_handle(
    config: McpConfig,
) -> Result<
    (
        tokio::task::JoinHandle<Result<(), McpServerError>>,
        std::net::SocketAddr,
    ),
    CliError,
> {
    let pg = proxima_storage_pg::PgStorage::connect(&config.database_url)
        .await
        .map_err(McpServerError::from)?;
    pg.run_migrations().await.map_err(McpServerError::from)?;
    proxima_mcp_substrate::migrator()
        .run(pg.pool())
        .await
        .map_err(McpServerError::from)?;
    proxima_flavor_goal::migrator()
        .run(pg.pool())
        .await
        .map_err(McpServerError::from)?;

    let mut registry = FlavorRegistry::new();
    proxima_mcp_substrate::register(&mut registry);
    proxima_flavor_goal::register(&mut registry);
    let server = DevMcpServer::from_pool(
        pg.pool().clone(),
        config.owner,
        std::sync::Arc::new(registry.freeze()),
    );
    // TODO(Task 5): replace this placeholder with the engine's
    // dispatcher store. The headless `proxima-mcp` binary doesn't yet
    // run a wake dispatcher, so the resulting service will reject every
    // request with 401 until that wiring lands.
    let wake_token_store = std::sync::Arc::new(
        proxima_core::wake::token_store::WakeTokenStore::new(std::time::Duration::from_secs(300)),
    );
    Ok(serve_streamable_http(config.bind, server, default_allowlist(), wake_token_store).await?)
}

#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error(transparent)]
    Args(#[from] ArgsError),
    #[error(transparent)]
    Server(#[from] McpServerError),
    #[error("transport: {0}")]
    Transport(String),
}

impl CliError {
    #[must_use]
    pub fn is_help(&self) -> bool {
        matches!(self, Self::Args(args) if args.is_help())
    }
}

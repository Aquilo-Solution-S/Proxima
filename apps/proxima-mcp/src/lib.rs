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
    let mut registry = FlavorRegistry::new();
    proxima_mcp_substrate::register(&mut registry);
    let server =
        DevMcpServer::from_database_url(&config.database_url, config.owner, registry).await?;
    Ok(serve_streamable_http(config.bind, server, default_allowlist()).await?)
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

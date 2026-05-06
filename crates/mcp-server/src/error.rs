use thiserror::Error;

#[derive(Debug, Error)]
pub enum McpServerError {
    #[error("non-loopback bind refused: {0}")]
    NonLoopbackBind(std::net::IpAddr),
    #[error("bind: {0}")]
    Bind(#[from] std::io::Error),
    #[error("storage: {0}")]
    Storage(#[from] proxima_core::StorageError),
    #[error("migration: {0}")]
    Migration(#[from] sqlx::migrate::MigrateError),
    #[error("axum: {0}")]
    Axum(String),
}

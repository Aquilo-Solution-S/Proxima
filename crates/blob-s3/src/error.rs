/// Errors from the cited-blob service.
#[derive(Debug, thiserror::Error)]
pub enum BlobError {
    /// S3 runtime configuration is missing or invalid.
    #[error("S3 config error: {0}")]
    Config(String),
    /// An S3 operation failed.
    #[error("S3 error: {0}")]
    S3(String),
    /// A database operation failed.
    #[error("db error: {0}")]
    Db(#[from] sqlx::Error),
    /// The authorization context does not permit acting on the request Owner.
    #[error("access denied: {0}")]
    Denied(String),
    /// Upload/blob state violation (missing row, expired, wrong status).
    #[error("{0}")]
    State(String),
}

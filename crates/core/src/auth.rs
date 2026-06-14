//! Credential material consumed by the host [`Authenticator`](crate::Authenticator).
//!
//! See docs/14-protocol-surface.md §"Auth model".

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Credentials {
    /// Host token material, opaque to core; interpreted only by the
    /// host-provided `Authenticator`.
    Bearer(String),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuthError {
    #[error("authentication required")]
    AuthRequired,
    #[error("invalid credentials")]
    InvalidCredentials,
}

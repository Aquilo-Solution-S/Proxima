//! Credential material consumed by the host [`Authenticator`](crate::Authenticator).
//!
//! See docs/14-protocol-surface.md §"Auth model".

#[derive(Clone, PartialEq, Eq)]
pub enum Credentials {
    /// Host token material, opaque to core; interpreted only by the
    /// host-provided `Authenticator`.
    Bearer(String),
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bearer(_) => f.write_str("Bearer(***)"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuthError {
    #[error("authentication required")]
    AuthRequired,
    #[error("invalid credentials")]
    InvalidCredentials,
}

#[cfg(test)]
mod tests {
    use super::Credentials;

    #[test]
    fn bearer_debug_redacts_token_material() {
        let credentials = Credentials::Bearer("secret-token".to_string());
        assert_eq!(format!("{credentials:?}"), "Bearer(***)");
    }
}

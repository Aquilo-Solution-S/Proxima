//! OIDC authenticator configuration.

use std::collections::HashSet;

use proxima_core::Owner;

/// Configuration for [`crate::OidcAuthenticator`].
#[derive(Clone, Debug)]
pub struct OidcAuthConfig {
    /// Exact `iss` claim required (e.g. `https://zitadel.example.com`).
    pub issuer: String,
    /// Explicit JWKS endpoint. `None` => discover via
    /// `{issuer}/.well-known/openid-configuration`.
    pub jwks_uri: Option<String>,
    /// Required value in the token `aud` (RFC 8707 resource id).
    pub audience: String,
    /// Fixed single-tenant owner every accepted token maps to.
    pub owner: Owner,
    /// Optional `sub` allowlist; `None` => accept any valid token.
    pub allowed_subjects: Option<HashSet<String>>,
    /// Clock-skew tolerance in seconds (default 60).
    pub leeway_secs: u64,
}

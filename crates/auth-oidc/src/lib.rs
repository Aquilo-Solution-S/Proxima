//! Generic OIDC bearer-JWT host authenticator for Proxima.
//!
//! Validates a Zitadel/OIDC access token (signature via JWKS, `iss`/`aud`/
//! `exp`, optional `sub` allowlist) and returns an
//! `AuthzContext::single_owner(owner, HostBearer)`.

mod authenticator;
mod config;
mod keys;

pub use authenticator::OidcAuthenticator;
pub use config::OidcAuthConfig;
pub use keys::{HttpJwksResolver, KeyError, KeyResolver, StaticJwksResolver};

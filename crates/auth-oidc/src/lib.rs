//! Generic OIDC bearer-JWT host authenticator for Proxima.
//!
//! Validates a Zitadel/OIDC access token (signature via JWKS, `iss`/`aud`/
//! `exp`, optional `sub` allowlist). [`OidcTokenValidator`] is the
//! validation-only boundary; [`OidcAuthenticator`] composes it with host
//! identity resolution: the issuer-aware [`OidcSubjectMap`] +
//! `OwnerAccessPort` path ([`OidcAuthenticator::new`]).

mod authenticator;
mod config;
mod keys;
mod subject_map;

pub use authenticator::{OidcAuthenticator, OidcTokenValidator, ValidatedOidcClaims};
pub use config::{OidcAuthConfig, OidcConfigError};
pub use keys::{HttpJwksResolver, KeyError, KeyResolver, StaticJwksResolver};
pub use subject_map::{OidcSubjectMap, OidcSubjectMapError};

//! Generic OIDC bearer-JWT host authenticator for Proxima.
//!
//! Validates a Zitadel/OIDC access token (signature via JWKS, `iss`/`aud`/
//! `exp`, optional `sub` allowlist). [`OidcTokenValidator`] is the
//! validation-only boundary; [`OidcAuthenticator`] composes it with host
//! identity resolution: the issuer-aware [`OidcSubjectMap`] +
//! `OwnerAccessPort` path ([`OidcAuthenticator::new`]). [`OidcBindingSet`]
//! composes several validated `(issuer, audience)` bindings into one
//! fail-closed [`proxima_core::Authenticator`].

mod authenticator;
mod binding_set;
mod config;
mod keys;
mod subject_map;

pub use authenticator::{OidcAuthenticator, OidcTokenValidator, ValidatedOidcClaims};
pub use binding_set::{
    OidcBinding, OidcBindingRoute, OidcBindingSet, OidcBindingSetError, OidcRoleShape,
};
pub use config::{OidcAuthConfig, OidcConfigError};
pub use keys::{
    DEFAULT_HTTP_REQUEST_TIMEOUT, HttpJwksResolver, KeyError, KeyResolver,
    MAX_HTTP_REQUEST_TIMEOUT, StaticJwksResolver,
};
pub use subject_map::{OidcSubjectMap, OidcSubjectMapError, SubjectBinding};

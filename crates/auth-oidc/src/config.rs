//! OIDC authenticator configuration.

use std::collections::HashSet;

use proxima_core::Owner;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum OidcConfigError {
    #[error("{field} must be a valid URL: {parse_error}")]
    InvalidUrl {
        field: &'static str,
        parse_error: String,
    },
    #[error("{field} must use https: {value}")]
    InsecureUrl { field: &'static str, value: String },
}

/// Configuration for [`crate::OidcAuthenticator`].
///
/// Every accepted token maps to [`Self::owner`] with full single-tenant
/// capabilities: all roles and all tools. For network exposure, make the
/// issuer/audience boundary trusted and set [`Self::allowed_subjects`] unless
/// every valid subject for that audience should act as the single owner.
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
    /// Optional `sub` allowlist; `None` => accept any valid token, which then
    /// receives full capabilities as the configured single owner.
    pub allowed_subjects: Option<HashSet<String>>,
    /// Clock-skew tolerance in seconds (default 60).
    pub leeway_secs: u64,
}

impl OidcAuthConfig {
    /// # Errors
    ///
    /// Returns an error when the issuer or explicit JWKS endpoint is not a
    /// valid HTTPS URL. Test builds allow loopback HTTP for mock `IdPs` only.
    pub fn validate(&self) -> Result<(), OidcConfigError> {
        validate_https_url("issuer", &self.issuer)?;
        if let Some(jwks_uri) = &self.jwks_uri {
            validate_https_url("jwks_uri", jwks_uri)?;
        }
        Ok(())
    }
}

/// # Errors
///
/// Returns an error when `raw` is not a URL or uses a non-HTTPS scheme. Test
/// builds allow loopback HTTP so resolver tests can run against local Axum.
pub fn validate_https_url(field: &'static str, raw: &str) -> Result<(), OidcConfigError> {
    let url = reqwest::Url::parse(raw).map_err(|err| OidcConfigError::InvalidUrl {
        field,
        parse_error: err.to_string(),
    })?;
    if url.scheme() == "https" || test_allows_loopback_http(&url) {
        return Ok(());
    }
    Err(OidcConfigError::InsecureUrl {
        field,
        value: raw.to_string(),
    })
}

#[cfg(test)]
fn test_allows_loopback_http(url: &reqwest::Url) -> bool {
    if url.scheme() != "http" {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|addr| addr.is_loopback())
}

#[cfg(not(test))]
fn test_allows_loopback_http(_url: &reqwest::Url) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use proxima_core::{OwnerRef, UserId};

    use super::*;

    fn config(issuer: &str, jwks_uri: Option<&str>) -> OidcAuthConfig {
        OidcAuthConfig {
            issuer: issuer.to_string(),
            jwks_uri: jwks_uri.map(ToOwned::to_owned),
            audience: "proxima-api".to_string(),
            owner: OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7())),
            allowed_subjects: None,
            leeway_secs: 60,
        }
    }

    #[test]
    fn rejects_http_issuer_and_explicit_jwks_uri() {
        assert!(matches!(
            config("http://issuer.example", None).validate(),
            Err(OidcConfigError::InsecureUrl {
                field: "issuer",
                ..
            })
        ));
        assert!(matches!(
            config("https://issuer.example", Some("http://issuer.example/keys")).validate(),
            Err(OidcConfigError::InsecureUrl {
                field: "jwks_uri",
                ..
            })
        ));
    }

    #[test]
    fn test_build_allows_loopback_http() {
        config("http://127.0.0.1:4180", Some("http://localhost:4180/keys"))
            .validate()
            .expect("loopback http is test-only");
    }
}

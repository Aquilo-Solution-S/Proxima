//! OIDC authenticator configuration.

use std::collections::HashSet;

use proxima_core::{EndpointUrlError, EndpointUrlPolicy};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum OidcConfigError {
    #[error("{field} must be a valid URL: {parse_error}")]
    InvalidUrl {
        field: &'static str,
        parse_error: String,
    },
    #[error("{field} must use https: {value}")]
    InsecureUrl { field: &'static str, value: String },
    #[error("{field} must be greater than zero and at most {max_seconds} seconds")]
    InvalidTimeout {
        field: &'static str,
        max_seconds: u64,
    },
}

/// Configuration for [`crate::OidcTokenValidator`] / [`crate::OidcAuthenticator`].
///
/// Carries only the audited JWT-validation boundary (issuer, audience, JWKS,
/// clock skew) plus an optional `sub` allowlist. It carries no identity
/// mapping: [`crate::OidcAuthenticator::new`] maps the validated
/// `(iss, sub)` through a [`crate::OidcSubjectMap`] and an
/// `OwnerAccessPort`. [`Self::allowed_subjects`] is always an additional
/// allowlist gate on the token's actual `sub`, never an identity source.
#[derive(Clone, Debug)]
pub struct OidcAuthConfig {
    /// Exact `iss` claim required (e.g. `https://zitadel.example.com`).
    pub issuer: String,
    /// Explicit JWKS endpoint. `None` => discover via
    /// `{issuer}/.well-known/openid-configuration`.
    pub jwks_uri: Option<String>,
    /// Required value in the token `aud` (RFC 8707 resource id).
    pub audience: String,
    /// Optional `sub` allowlist; `None` => accept any valid token for this
    /// issuer/audience (identity is still resolved separately).
    pub allowed_subjects: Option<HashSet<String>>,
    /// Clock-skew tolerance in seconds (default 60).
    pub leeway_secs: u64,
}

impl OidcAuthConfig {
    /// # Errors
    ///
    /// Returns an error when the issuer or explicit JWKS endpoint is not a
    /// valid HTTPS URL, or a plaintext HTTP URL on a non-loopback host.
    pub fn validate(&self) -> Result<(), OidcConfigError> {
        validate_https_url("issuer", &self.issuer)?;
        if let Some(jwks_uri) = &self.jwks_uri {
            validate_jwks_url("jwks_uri", jwks_uri, &self.issuer)?;
        }
        Ok(())
    }
}

/// Validate a JWKS endpoint against the issuer that vouches for it.
///
/// Plaintext loopback is admitted only when the issuer is *itself* loopback.
/// A remote HTTPS issuer must not be able to move key resolution onto the
/// host — whether through an explicitly configured `jwks_uri` or through the
/// `jwks_uri` in its own discovery document. Without this, a compromised or
/// hostile upstream `IdP` could name `http://127.0.0.1:<port>/keys` and have
/// Proxima trust whatever happens to answer there.
///
/// # Errors
///
/// Returns an error when `raw` is not a URL, or is plaintext HTTP that this
/// issuer is not entitled to name.
pub fn validate_jwks_url(
    field: &'static str,
    raw: &str,
    issuer: &str,
) -> Result<(), OidcConfigError> {
    if proxima_core::is_loopback_endpoint(issuer) {
        return validate_https_url(field, raw);
    }
    proxima_core::validate_endpoint_url(raw, EndpointUrlPolicy::HttpsOnly).map_err(|error| {
        match error {
            EndpointUrlError::InvalidUrl(parse_error) => {
                OidcConfigError::InvalidUrl { field, parse_error }
            }
            EndpointUrlError::InsecureTransport => OidcConfigError::InsecureUrl {
                field,
                value: raw.to_string(),
            },
        }
    })
}

/// Validate an OIDC endpoint URL: HTTPS anywhere, plaintext HTTP on loopback
/// only.
///
/// The loopback carve-out exists so a self-hosted deployment can run its
/// issuer on the same machine — see `tools/dev-idp`, and any operator
/// fronting Proxima with a local `IdP`. It is the same
/// [`EndpointUrlPolicy::AllowLoopbackHttp`] the workspace already applies to
/// locally-hosted model endpoints in `proxima-llm-openai-compat`, and it is
/// narrow by construction: `validate_endpoint_url` admits plaintext only for
/// `localhost`, `127.0.0.0/8`, and `::1`, so reaching it already requires
/// code executing on the host. Every other host stays HTTPS-only, which is
/// what keeps a bearer off the wire in transit.
///
/// This is a transport check on the endpoint, not a trust decision about the
/// issuer: tokens are still verified by RS256 signature against the JWKS this
/// URL serves, and `(iss, sub)` is still mapped through an explicit subject
/// map before it becomes an identity.
///
/// # Errors
///
/// Returns an error when `raw` is not a URL, or uses a non-HTTPS scheme on a
/// non-loopback host.
pub fn validate_https_url(field: &'static str, raw: &str) -> Result<(), OidcConfigError> {
    proxima_core::validate_endpoint_url(raw, EndpointUrlPolicy::AllowLoopbackHttp).map_err(
        |error| match error {
            EndpointUrlError::InvalidUrl(parse_error) => {
                OidcConfigError::InvalidUrl { field, parse_error }
            }
            EndpointUrlError::InsecureTransport => OidcConfigError::InsecureUrl {
                field,
                value: raw.to_string(),
            },
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(issuer: &str, jwks_uri: Option<&str>) -> OidcAuthConfig {
        OidcAuthConfig {
            issuer: issuer.to_string(),
            jwks_uri: jwks_uri.map(ToOwned::to_owned),
            audience: "proxima-api".to_string(),
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
    fn allows_loopback_http_for_self_hosted_issuers() {
        config("http://127.0.0.1:4180", Some("http://localhost:4180/keys"))
            .validate()
            .expect("loopback http issuer");
        config("http://[::1]:4180", None)
            .validate()
            .expect("ipv6 loopback issuer");
    }

    /// A loopback issuer may name its own loopback JWKS — that is the whole
    /// self-hosted case. A remote issuer may not: it would let an upstream
    /// `IdP` move key resolution onto this host.
    #[test]
    fn only_a_loopback_issuer_may_name_a_loopback_jwks() {
        validate_jwks_url(
            "jwks_uri",
            "http://127.0.0.1:4180/keys",
            "http://127.0.0.1:4180",
        )
        .expect("loopback issuer may name its own loopback jwks");

        assert!(matches!(
            validate_jwks_url(
                "jwks_uri",
                "http://127.0.0.1:4180/keys",
                "https://idp.example"
            ),
            Err(OidcConfigError::InsecureUrl {
                field: "jwks_uri",
                ..
            })
        ));
        assert!(matches!(
            config("https://idp.example", Some("http://localhost:4180/keys")).validate(),
            Err(OidcConfigError::InsecureUrl {
                field: "jwks_uri",
                ..
            })
        ));

        // A loopback issuer still cannot reach off-host over plaintext.
        assert!(matches!(
            validate_jwks_url(
                "jwks_uri",
                "http://idp.example/keys",
                "http://127.0.0.1:4180"
            ),
            Err(OidcConfigError::InsecureUrl {
                field: "jwks_uri",
                ..
            })
        ));
        // HTTPS is always fine, whoever names it.
        validate_jwks_url(
            "jwks_uri",
            "https://idp.example/keys",
            "http://127.0.0.1:4180",
        )
        .expect("https jwks from a loopback issuer");
    }

    /// The loopback carve-out must stay pinned to loopback. A host that
    /// merely *looks* local — a hostname resolving to 127.0.0.1, a private
    /// LAN address, a subdomain of `localhost` — is still a network hop, and
    /// admitting it would put the bearer on the wire in plaintext.
    #[test]
    fn loopback_allowance_does_not_leak_to_lookalike_hosts() {
        for issuer in [
            "http://localhost.evil.example",
            "http://10.0.0.1:4180",
            "http://192.168.1.10:4180",
            "http://127.0.0.1.evil.example",
            "http://[::ffff:127.0.0.1]:4180",
        ] {
            assert!(
                matches!(
                    config(issuer, None).validate(),
                    Err(OidcConfigError::InsecureUrl { .. })
                ),
                "{issuer} must not be treated as loopback"
            );
        }
    }
}

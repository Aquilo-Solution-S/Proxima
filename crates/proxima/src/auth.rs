//! Wiring an OIDC-authenticated MCP host from the environment.
//!
//! WHY THIS IS IN THE FACADE and not left to each host. Serving MCP requires
//! an `Authenticator` — `allow_insecure_single_owner` is refused for MCP on
//! purpose, so there is no development shortcut that reaches the transport.
//! The only authenticator that ships lives in `proxima-auth-oidc`, and an
//! out-of-tree flavor takes exactly ONE dependency on this repository (the
//! facade) so its lockfile cannot drift between two revisions of the same
//! tree. The env parse therefore lives here: `Proxima::app().authenticator(..)`
//! would otherwise take an argument no out-of-tree caller can construct.
//!
//! IT IS THE ENV CONTRACT, NOT A CONVENIENCE. Single implementation of
//! `PROXIMA_OIDC_*`; `apps/proxima-mcp` delegates here rather than keeping
//! a second copy that could answer differently for the same variables.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use proxima_core::Authenticator;
use proxima_mcp_server::ResourceServerMetadata;

use crate::OwnerAccessPort;
use crate::runtime_config::ProximaError;

/// Exactly one of these two names the subject map, and one of them must be
/// set whenever an issuer is.
const SUBJECT_MAP_JSON: &str = "PROXIMA_OIDC_SUBJECT_MAP_JSON";
const SUBJECT_MAP_LEGACY: &str = "PROXIMA_OIDC_SUBJECT_MAP";

/// Complete-request timeout for OIDC discovery and JWKS HTTP requests.
const OIDC_HTTP_TIMEOUT_SECONDS: &str = "PROXIMA_OIDC_HTTP_TIMEOUT_SECONDS";

/// How much clock skew a token may carry, in seconds.
const LEEWAY_SECS: u64 = 60;

/// What a host needs to serve authenticated MCP: who validates a bearer
/// token, and what to advertise at the protected-resource metadata endpoint.
pub type OidcBundle = (Arc<dyn Authenticator>, ResourceServerMetadata);

/// Low-level OIDC primitives a host authenticator composes. Not a second
/// authenticator: `oidc_from_env` stays the one-audience env path.
pub use proxima_auth_oidc::{
    DEFAULT_HTTP_REQUEST_TIMEOUT, HttpJwksResolver, KeyError, KeyResolver,
    MAX_HTTP_REQUEST_TIMEOUT, OidcAuthConfig, OidcConfigError, OidcTokenValidator,
    ValidatedOidcClaims,
};
pub use proxima_core::{AccessError, OwnerRoles};

/// Build the OIDC authenticator and resource metadata from the process
/// environment.
///
/// Returns `Ok(None)` when `PROXIMA_OIDC_ISSUER` is unset — a host with no
/// issuer is not misconfigured, it simply is not serving authenticated MCP,
/// and an embedded host using the Host API needs no issuer at all. Once an
/// issuer IS set, every companion variable becomes required and its absence
/// is an error rather than a silent downgrade to something less
/// authenticated than the operator asked for.
///
/// Variables:
/// - `PROXIMA_OIDC_ISSUER` — the issuer URL; presence switches this on.
/// - `PROXIMA_OIDC_AUDIENCE` — the audience every token must carry.
/// - `PROXIMA_PUBLIC_URL` — this server's own URL, advertised to clients.
/// - `PROXIMA_OIDC_SUBJECT_MAP_JSON` or `PROXIMA_OIDC_SUBJECT_MAP` — which
///   subject maps to which owner. Mutually exclusive.
/// - `PROXIMA_OIDC_JWKS_URI` — optional override; discovered otherwise.
/// - `PROXIMA_OIDC_HTTP_TIMEOUT_SECONDS` — optional complete-request timeout;
///   default 10 seconds, maximum 300 seconds.
/// - `PROXIMA_OIDC_ALLOWED_SUBJECTS` — optional comma-separated allowlist.
///
/// # Errors
///
/// Returns [`ProximaError::Config`] when an issuer is set without its
/// companions, when both subject-map spellings are set, when the subject map
/// will not parse, or when the issuer or JWKS URI is not a secure URL.
pub fn oidc_from_env(
    owner_access: Arc<dyn OwnerAccessPort>,
) -> Result<Option<OidcBundle>, ProximaError> {
    oidc_from_lookup(&proxima_core::process_env, owner_access)
}

/// [`oidc_from_env`] against an arbitrary lookup, so a host with its own
/// configuration source — or a test — does not have to mutate the process
/// environment to use the same contract.
///
/// # Errors
///
/// As [`oidc_from_env`].
pub fn oidc_from_lookup(
    lookup: &impl Fn(&str) -> Option<String>,
    owner_access: Arc<dyn OwnerAccessPort>,
) -> Result<Option<OidcBundle>, ProximaError> {
    let Some(issuer) = non_empty(lookup, "PROXIMA_OIDC_ISSUER") else {
        return Ok(None);
    };
    let audience = non_empty(lookup, "PROXIMA_OIDC_AUDIENCE").ok_or_else(|| {
        ProximaError::Config("PROXIMA_OIDC_ISSUER set without PROXIMA_OIDC_AUDIENCE".into())
    })?;
    let public_url = non_empty(lookup, "PROXIMA_PUBLIC_URL").ok_or_else(|| {
        ProximaError::Config("PROXIMA_OIDC_ISSUER set without PROXIMA_PUBLIC_URL".into())
    })?;
    let jwks_uri = non_empty(lookup, "PROXIMA_OIDC_JWKS_URI");
    let allowed_subjects = non_empty(lookup, "PROXIMA_OIDC_ALLOWED_SUBJECTS").map(|raw| {
        raw.split(',')
            .map(str::trim)
            .filter(|subject| !subject.is_empty())
            .map(ToOwned::to_owned)
            .collect::<HashSet<String>>()
    });

    let config = proxima_auth_oidc::OidcAuthConfig {
        issuer: issuer.clone(),
        jwks_uri,
        audience,
        allowed_subjects,
        leeway_secs: LEEWAY_SECS,
    };
    // Keep the URL security boundary ahead of non-security companion parsing.
    // `with_request_timeout` validates it again so its standalone low-level
    // contract does not depend on this facade.
    config
        .validate()
        .map_err(|err| ProximaError::Config(err.to_string()))?;
    let request_timeout = oidc_http_timeout(lookup)?;
    // The issuer/JWKS URL boundary is validated BEFORE the subject map and
    // before storage, so an insecure-URL rejection short-circuits rather
    // than being reported after two other things have already been parsed.
    let resolver = proxima_auth_oidc::HttpJwksResolver::with_request_timeout(
        issuer.clone(),
        config.jwks_uri.clone(),
        request_timeout,
    )
    .map_err(|err| ProximaError::Config(err.to_string()))?;
    let subject_map = subject_map(lookup, &issuer)?;

    let authenticator = proxima_auth_oidc::OidcAuthenticator::new(
        config,
        Arc::new(resolver),
        subject_map,
        owner_access,
    )
    .map_err(|err| ProximaError::Config(err.to_string()))?;
    Ok(Some((
        Arc::new(authenticator),
        ResourceServerMetadata {
            public_url,
            authorization_servers: vec![issuer],
        },
    )))
}

fn oidc_http_timeout(lookup: &impl Fn(&str) -> Option<String>) -> Result<Duration, ProximaError> {
    let Some(raw) = non_empty(lookup, OIDC_HTTP_TIMEOUT_SECONDS) else {
        return Ok(proxima_auth_oidc::DEFAULT_HTTP_REQUEST_TIMEOUT);
    };
    let seconds = raw.parse::<u64>().map_err(|_| {
        ProximaError::Config(format!(
            "{OIDC_HTTP_TIMEOUT_SECONDS} must be integer seconds, got {raw:?}"
        ))
    })?;
    if seconds == 0 || seconds > proxima_auth_oidc::MAX_HTTP_REQUEST_TIMEOUT.as_secs() {
        return Err(ProximaError::Config(format!(
            "{OIDC_HTTP_TIMEOUT_SECONDS} must be between 1 and {} seconds, got {raw:?}",
            proxima_auth_oidc::MAX_HTTP_REQUEST_TIMEOUT.as_secs()
        )));
    }
    Ok(Duration::from_secs(seconds))
}

/// Parse the issuer-aware subject map.
///
/// Exactly one spelling must be set. Both being set is an error rather than
/// a precedence rule: the two disagree about which owner a subject resolves
/// to, and silently preferring one would decide an access-control question
/// by the order of an `if`.
fn subject_map(
    lookup: &impl Fn(&str) -> Option<String>,
    issuer: &str,
) -> Result<proxima_auth_oidc::OidcSubjectMap, ProximaError> {
    match (
        non_empty(lookup, SUBJECT_MAP_JSON),
        non_empty(lookup, SUBJECT_MAP_LEGACY),
    ) {
        (Some(_), Some(_)) => Err(ProximaError::Config(format!(
            "{SUBJECT_MAP_JSON} and {SUBJECT_MAP_LEGACY} are mutually exclusive"
        ))),
        (Some(json), None) => proxima_auth_oidc::OidcSubjectMap::from_json(&json)
            .map_err(|err| ProximaError::Config(format!("{SUBJECT_MAP_JSON}: {err}"))),
        (None, Some(legacy)) => {
            proxima_auth_oidc::OidcSubjectMap::from_legacy_shorthand(&legacy, &[issuer.to_owned()])
                .map_err(|err| ProximaError::Config(format!("{SUBJECT_MAP_LEGACY}: {err}")))
        }
        (None, None) => Err(ProximaError::Config(format!(
            "PROXIMA_OIDC_ISSUER set without {SUBJECT_MAP_JSON} or {SUBJECT_MAP_LEGACY}"
        ))),
    }
}

fn non_empty(lookup: &impl Fn(&str) -> Option<String>, key: &str) -> Option<String> {
    lookup(key)
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use super::{non_empty, oidc_from_lookup, oidc_http_timeout};

    /// A lookup over a fixed map, so none of this touches the process
    /// environment — which is what lets these run in parallel with every
    /// other test in the crate.
    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect();
        move |key: &str| map.get(key).cloned()
    }

    /// Lazy, so it never opens a connection — but constructing a lazy
    /// sqlx pool still requires a Tokio context, which is why these are
    /// `#[tokio::test]` and not `#[test]`.
    fn owner_access() -> Arc<dyn crate::OwnerAccessPort> {
        Arc::new(
            proxima_storage_pg::PgOwnerAccessResolver::connect_lazy(
                "postgres://proxima:proxima@127.0.0.1:5432/proxima",
            )
            .expect("a lazy pool never connects"),
        )
    }

    /// No issuer is not a misconfiguration. An embedded host using the Host
    /// API serves no MCP and needs no issuer, and must still boot.
    #[tokio::test]
    async fn no_issuer_means_no_authenticator_and_no_error() {
        let resolved =
            oidc_from_lookup(&env(&[]), owner_access()).expect("absence is not an error");
        assert!(resolved.is_none());
    }

    #[test]
    fn oidc_http_timeout_has_a_finite_default_and_parses_an_override() {
        assert_eq!(
            oidc_http_timeout(&env(&[])).expect("default timeout"),
            proxima_auth_oidc::DEFAULT_HTTP_REQUEST_TIMEOUT
        );
        assert_eq!(
            oidc_http_timeout(&env(&[("PROXIMA_OIDC_HTTP_TIMEOUT_SECONDS", "17")]))
                .expect("valid override"),
            std::time::Duration::from_secs(17)
        );
    }

    #[tokio::test]
    async fn oidc_http_timeout_override_is_wired_and_invalid_values_are_refused() {
        let configured = [
            ("PROXIMA_OIDC_ISSUER", "https://idp.test"),
            ("PROXIMA_OIDC_AUDIENCE", "proxima-mcp"),
            ("PROXIMA_PUBLIC_URL", "https://mcp.test"),
            (
                "PROXIMA_OIDC_SUBJECT_MAP",
                "sub:0195a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b",
            ),
            ("PROXIMA_OIDC_HTTP_TIMEOUT_SECONDS", "17"),
        ];
        oidc_from_lookup(&env(&configured), owner_access())
            .expect("valid timeout reaches resolver construction")
            .expect("issuer config produces a bundle");

        for invalid in ["0", "301", "not-a-number"] {
            let invalid_config = [
                ("PROXIMA_OIDC_ISSUER", "https://idp.test"),
                ("PROXIMA_OIDC_AUDIENCE", "proxima-mcp"),
                ("PROXIMA_PUBLIC_URL", "https://mcp.test"),
                (
                    "PROXIMA_OIDC_SUBJECT_MAP",
                    "sub:0195a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b",
                ),
                ("PROXIMA_OIDC_HTTP_TIMEOUT_SECONDS", invalid),
            ];
            let Err(err) = oidc_from_lookup(&env(&invalid_config), owner_access()) else {
                panic!("configured invalid timeout must fail boot");
            };
            assert!(
                err.to_string()
                    .contains("PROXIMA_OIDC_HTTP_TIMEOUT_SECONDS"),
                "message: {err}"
            );
        }
    }

    /// Once an issuer IS set every companion becomes required. A silent
    /// downgrade here would serve MCP with less authentication than the
    /// operator asked for, which is the one outcome worth failing to boot
    /// over.
    #[tokio::test]
    async fn an_issuer_without_its_companions_is_refused() {
        for partial in [
            vec![("PROXIMA_OIDC_ISSUER", "https://idp.test")],
            vec![
                ("PROXIMA_OIDC_ISSUER", "https://idp.test"),
                ("PROXIMA_OIDC_AUDIENCE", "proxima-mcp"),
            ],
            vec![
                ("PROXIMA_OIDC_ISSUER", "https://idp.test"),
                ("PROXIMA_OIDC_AUDIENCE", "proxima-mcp"),
                ("PROXIMA_PUBLIC_URL", "https://mcp.test"),
            ],
        ] {
            assert!(
                oidc_from_lookup(&env(&partial), owner_access()).is_err(),
                "an issuer with {} companion(s) must not resolve",
                partial.len() - 1
            );
        }
    }

    /// The two subject-map spellings disagree about which owner a subject
    /// resolves to, so preferring one would settle an access-control
    /// question by the order of an `if`.
    #[tokio::test]
    async fn both_subject_map_spellings_at_once_is_refused() {
        let both = env(&[
            ("PROXIMA_OIDC_ISSUER", "https://idp.test"),
            ("PROXIMA_OIDC_AUDIENCE", "proxima-mcp"),
            ("PROXIMA_PUBLIC_URL", "https://mcp.test"),
            (
                "PROXIMA_OIDC_SUBJECT_MAP",
                "sub:0195a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b",
            ),
            ("PROXIMA_OIDC_SUBJECT_MAP_JSON", "{}"),
        ]);
        assert!(oidc_from_lookup(&both, owner_access()).is_err());
    }

    /// A plaintext issuer is refused before anything else is parsed, so the
    /// error names the insecure URL rather than whatever came after it.
    #[tokio::test]
    async fn a_plaintext_issuer_is_refused() {
        let insecure = env(&[
            ("PROXIMA_OIDC_ISSUER", "http://idp.test"),
            ("PROXIMA_OIDC_AUDIENCE", "proxima-mcp"),
            ("PROXIMA_PUBLIC_URL", "https://mcp.test"),
            (
                "PROXIMA_OIDC_SUBJECT_MAP",
                "sub:0195a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b",
            ),
        ]);
        assert!(oidc_from_lookup(&insecure, owner_access()).is_err());
    }

    /// A variable set to whitespace is set to nothing. Otherwise an empty
    /// value in a compose file or a k8s manifest reads as "configured" and
    /// fails much later with a confusing message.
    #[test]
    fn whitespace_reads_as_absent() {
        assert!(non_empty(&env(&[("KEY", "   ")]), "KEY").is_none());
        assert_eq!(
            non_empty(&env(&[("KEY", " value ")]), "KEY"),
            Some("value".to_owned())
        );
    }
}

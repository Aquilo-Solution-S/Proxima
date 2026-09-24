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

/// Where verification keys come from: fetched from the issuer (discovered, or
/// at `JWKS_URI`), or pinned in configuration (`JWKS_JSON`). At most one.
const JWKS_URI: &str = "PROXIMA_OIDC_JWKS_URI";
const JWKS_JSON: &str = "PROXIMA_OIDC_JWKS_JSON";

/// Complete-request timeout for OIDC discovery and JWKS HTTP requests.
const OIDC_HTTP_TIMEOUT_SECONDS: &str = "PROXIMA_OIDC_HTTP_TIMEOUT_SECONDS";

/// How much clock skew a token may carry, in seconds.
const LEEWAY_SECS: u64 = 60;

/// Every variable [`oidc_from_lookup`] reads. The runtime environment layer
/// captures exactly these, so the authenticator it builds at resolve sees
/// the environment the builder was given.
pub(crate) const OIDC_ENV_KEYS: [&str; 9] = [
    "PROXIMA_OIDC_ISSUER",
    "PROXIMA_OIDC_AUDIENCE",
    "PROXIMA_PUBLIC_URL",
    JWKS_URI,
    JWKS_JSON,
    "PROXIMA_OIDC_ALLOWED_SUBJECTS",
    OIDC_HTTP_TIMEOUT_SECONDS,
    SUBJECT_MAP_JSON,
    SUBJECT_MAP_LEGACY,
];

/// What a host needs to serve authenticated MCP: who validates a bearer
/// token, and what to advertise at the protected-resource metadata endpoint.
pub type OidcBundle = (Arc<dyn Authenticator>, ResourceServerMetadata);

/// Low-level OIDC primitives a host authenticator composes, and the two
/// stock authenticators (`OidcAuthenticator` one route, `OidcBindingSet`
/// several) with their constructor inputs. `oidc_from_env` stays the
/// one-audience env path.
pub use proxima_auth_oidc::{
    DEFAULT_HTTP_REQUEST_TIMEOUT, HttpJwksResolver, KeyError, KeyResolver,
    MAX_HTTP_REQUEST_TIMEOUT, OidcAuthConfig, OidcAuthenticator, OidcBinding, OidcBindingRoute,
    OidcBindingSet, OidcBindingSetError, OidcConfigError, OidcRoleShape, OidcSubjectMap,
    OidcSubjectMapError, OidcTokenValidator, StaticJwksResolver, SubjectBinding,
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
/// - `PROXIMA_OIDC_JWKS_JSON` — optional JWKS document pinned in config. When
///   set nothing is fetched: the issuer is only matched against `iss`, for a
///   host with no network path to it. Mutually exclusive with `JWKS_URI`.
/// - `PROXIMA_OIDC_HTTP_TIMEOUT_SECONDS` — optional complete-request timeout;
///   default 10 seconds, maximum 300 seconds.
/// - `PROXIMA_OIDC_ALLOWED_SUBJECTS` — optional comma-separated allowlist.
///
/// # Errors
///
/// Returns [`ProximaError::Config`] when an issuer is set without its
/// companions, when both subject-map spellings or both JWKS sources are set,
/// when the subject map or pinned JWKS will not parse, or when the issuer or
/// JWKS URI is not a secure URL.
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
    let jwks_uri = non_empty(lookup, JWKS_URI);
    let jwks_json = non_empty(lookup, JWKS_JSON);
    // Both is an error rather than a precedence rule: one says fetch keys
    // from the issuer, the other says never contact it.
    if jwks_uri.is_some() && jwks_json.is_some() {
        return Err(ProximaError::Config(format!(
            "{JWKS_JSON} and {JWKS_URI} are mutually exclusive"
        )));
    }
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
    let resolver: Arc<dyn KeyResolver> = match jwks_json {
        Some(json) => Arc::new(
            StaticJwksResolver::from_jwks_json(&json)
                .map_err(|err| ProximaError::Config(format!("{JWKS_JSON}: {err}")))?,
        ),
        None => Arc::new(
            HttpJwksResolver::with_request_timeout(
                issuer.clone(),
                config.jwks_uri.clone(),
                request_timeout,
            )
            .map_err(|err| ProximaError::Config(err.to_string()))?,
        ),
    };
    let subject_map = subject_map(lookup, &issuer)?;

    let authenticator =
        proxima_auth_oidc::OidcAuthenticator::new(config, resolver, subject_map, owner_access)
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

    /// The runtime environment layer snapshots `OIDC_ENV_KEYS` and replays
    /// them at resolve; a key read here but missing there would silently
    /// resolve as unset.
    #[tokio::test]
    async fn every_variable_read_is_captured_by_the_runtime_env_layer() {
        let asked = std::sync::Mutex::new(Vec::new());
        let full = env(&[
            ("PROXIMA_OIDC_ISSUER", "https://issuer.test"),
            ("PROXIMA_OIDC_AUDIENCE", "proxima"),
            ("PROXIMA_PUBLIC_URL", "https://mcp.test"),
            ("PROXIMA_OIDC_JWKS_URI", "https://issuer.test/jwks"),
            ("PROXIMA_OIDC_ALLOWED_SUBJECTS", "a,b"),
            ("PROXIMA_OIDC_HTTP_TIMEOUT_SECONDS", "5"),
            (
                "PROXIMA_OIDC_SUBJECT_MAP",
                "a:00000000-0000-7000-8000-000000000001",
            ),
        ]);
        let recording = |key: &str| {
            asked.lock().unwrap().push(key.to_owned());
            full(key)
        };
        oidc_from_lookup(&recording, owner_access()).expect("valid env");
        for key in asked.into_inner().unwrap() {
            assert!(
                super::OIDC_ENV_KEYS.contains(&key.as_str()),
                "{key} is read but not in OIDC_ENV_KEYS"
            );
        }
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

    /// The pack-call shape: tokens minted by an issuer the host has no
    /// network path to, verified against a key set shipped in its config.
    mod pinned_jwks {
        use std::io::ErrorKind;
        use std::net::TcpListener;
        use std::sync::Arc;
        use std::time::{SystemTime, UNIX_EPOCH};

        use async_trait::async_trait;
        use aws_lc_rs::rand::SystemRandom;
        use aws_lc_rs::rsa::KeySize;
        use aws_lc_rs::signature::{KeyPair as _, RSA_PKCS1_SHA256, RsaKeyPair};
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        use proxima_core::{
            AccessError, AuthError, Authenticator, Credentials, OwnerAccessPort, OwnerRoles, UserId,
        };
        use uuid::Uuid;

        use super::env;
        use crate::auth::{JWKS_JSON, JWKS_URI, oidc_from_lookup};
        use crate::runtime_config::ProximaError;

        const KID: &str = "pack-call-1";
        const FLEET: &str = "pack-fleet";
        const CALLER: &str = "centauri";

        /// An issuer that never answers. Plaintext loopback passes the
        /// issuer URL policy, and any discovery or JWKS fetch aimed at it
        /// completes its handshake into this listener's backlog, where
        /// [`Self::assert_never_contacted`] finds it.
        struct SilentIssuer {
            listener: TcpListener,
            url: String,
        }

        impl SilentIssuer {
            fn bind() -> Self {
                let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback issuer");
                listener.set_nonblocking(true).expect("non-blocking accept");
                let url = format!("http://{}", listener.local_addr().expect("bound address"));
                Self { listener, url }
            }

            fn assert_never_contacted(&self) {
                match self.listener.accept() {
                    Err(err) if err.kind() == ErrorKind::WouldBlock => {}
                    other => panic!("the static path contacted the issuer: {other:?}"),
                }
            }
        }

        struct OneSubject(UserId);

        #[async_trait]
        impl OwnerAccessPort for OneSubject {
            async fn resolve_roles_for_subject(
                &self,
                subject: UserId,
            ) -> Result<OwnerRoles, AccessError> {
                if subject != self.0 {
                    return Err(AccessError::Resolution("unknown subject".into()));
                }
                OwnerRoles::for_subject(subject, [])
            }
        }

        fn signing_key() -> RsaKeyPair {
            RsaKeyPair::generate(KeySize::Rsa2048).expect("generate test RSA key")
        }

        fn jwks(kid: &str, key: &RsaKeyPair) -> String {
            let public = key.public_key();
            serde_json::json!({ "keys": [{
                "kty": "RSA",
                "kid": kid,
                "alg": "RS256",
                "use": "sig",
                "n": URL_SAFE_NO_PAD.encode(public.modulus().big_endian_without_leading_zero()),
                "e": URL_SAFE_NO_PAD.encode(public.exponent().big_endian_without_leading_zero()),
            }]})
            .to_string()
        }

        fn token(key: &RsaKeyPair, kid: &str, issuer: &str) -> Credentials {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock after epoch")
                .as_secs();
            let header = serde_json::json!({ "alg": "RS256", "kid": kid, "typ": "JWT" });
            let claims = serde_json::json!({
                "iss": issuer,
                "aud": FLEET,
                "sub": CALLER,
                "exp": now + 60,
            });
            let signing_input = format!(
                "{}.{}",
                URL_SAFE_NO_PAD.encode(header.to_string()),
                URL_SAFE_NO_PAD.encode(claims.to_string())
            );
            let mut signature = vec![0; key.public_modulus_len()];
            key.sign(
                &RSA_PKCS1_SHA256,
                &SystemRandom::new(),
                signing_input.as_bytes(),
                &mut signature,
            )
            .expect("sign jwt");
            Credentials::Bearer(format!(
                "{signing_input}.{}",
                URL_SAFE_NO_PAD.encode(signature)
            ))
        }

        fn pinned(
            issuer: &str,
            jwks: &str,
            user: Uuid,
        ) -> Result<Option<crate::auth::OidcBundle>, ProximaError> {
            let subject_map = format!("{CALLER}:{user}");
            oidc_from_lookup(
                &env(&[
                    ("PROXIMA_OIDC_ISSUER", issuer),
                    ("PROXIMA_OIDC_AUDIENCE", FLEET),
                    ("PROXIMA_PUBLIC_URL", "https://pack.test"),
                    ("PROXIMA_OIDC_SUBJECT_MAP", &subject_map),
                    (JWKS_JSON, jwks),
                    // A regression to the HTTP resolver stalls on the silent
                    // issuer; make it fail in one second rather than ten.
                    ("PROXIMA_OIDC_HTTP_TIMEOUT_SECONDS", "1"),
                ]),
                Arc::new(OneSubject(UserId::new(user))),
            )
        }

        fn authenticator(issuer: &str, jwks: &str, user: Uuid) -> Arc<dyn Authenticator> {
            let (authenticator, metadata) = pinned(issuer, jwks, user)
                .expect("pinned jwks config")
                .expect("an issuer yields a bundle");
            assert_eq!(metadata.authorization_servers, [issuer]);
            authenticator
        }

        #[tokio::test]
        async fn a_token_signed_by_a_pinned_key_authenticates_without_contacting_the_issuer() {
            let issuer = SilentIssuer::bind();
            let key = signing_key();
            let user = Uuid::now_v7();
            let authenticator = authenticator(&issuer.url, &jwks(KID, &key), user);

            let ctx = authenticator
                .authenticate(&token(&key, KID, &issuer.url))
                .await
                .expect("pinned key verifies its token");

            assert_eq!(ctx.subject(), Some(UserId::new(user)));
            issuer.assert_never_contacted();
        }

        /// An unknown kid is where the HTTP resolver refetches, so it is the
        /// case that proves the pinned set is the whole set.
        #[tokio::test]
        async fn a_token_with_an_unknown_kid_is_refused_without_contacting_the_issuer() {
            let issuer = SilentIssuer::bind();
            let pinned_key = signing_key();
            let other_key = signing_key();
            let user = Uuid::now_v7();
            let authenticator = authenticator(&issuer.url, &jwks(KID, &pinned_key), user);

            for credentials in [
                token(&other_key, "rotated-in", &issuer.url),
                token(&pinned_key, "rotated-in", &issuer.url),
            ] {
                assert_eq!(
                    authenticator.authenticate(&credentials).await.err(),
                    Some(AuthError::InvalidCredentials)
                );
            }
            issuer.assert_never_contacted();
        }

        /// One source says fetch keys from the issuer, the other says never
        /// contact it; neither is allowed to win silently.
        #[tokio::test]
        async fn both_jwks_sources_at_once_is_refused() {
            let user = Uuid::now_v7();
            let both = env(&[
                ("PROXIMA_OIDC_ISSUER", "https://centauri.test"),
                ("PROXIMA_OIDC_AUDIENCE", FLEET),
                ("PROXIMA_PUBLIC_URL", "https://pack.test"),
                ("PROXIMA_OIDC_SUBJECT_MAP", &format!("{CALLER}:{user}")),
                (JWKS_URI, "https://centauri.test/keys"),
                (JWKS_JSON, &jwks(KID, &signing_key())),
            ]);
            let Err(ProximaError::Config(message)) =
                oidc_from_lookup(&both, Arc::new(OneSubject(UserId::new(user))))
            else {
                panic!("both JWKS sources must be a config error");
            };
            assert!(
                message.contains(JWKS_JSON) && message.contains(JWKS_URI),
                "message: {message}"
            );
        }

        #[tokio::test]
        async fn a_malformed_pinned_jwks_is_refused_at_boot() {
            let key = signing_key();
            let mut no_kid: serde_json::Value =
                serde_json::from_str(&jwks(KID, &key)).expect("jwks json");
            no_kid["keys"][0]
                .as_object_mut()
                .expect("jwk object")
                .remove("kid");
            let mut ec: serde_json::Value =
                serde_json::from_str(&jwks(KID, &key)).expect("jwks json");
            ec["keys"][0]["kty"] = "EC".into();

            for (case, raw) in [
                ("malformed JSON", "{\"keys\": [".to_owned()),
                ("no keys", r#"{"keys":[]}"#.to_owned()),
                ("no kid", no_kid.to_string()),
                ("non-RSA key", ec.to_string()),
            ] {
                let Err(ProximaError::Config(message)) =
                    pinned("https://centauri.test", &raw, Uuid::now_v7())
                else {
                    panic!("{case} must be a config error");
                };
                assert!(message.contains(JWKS_JSON), "{case}: {message}");
            }
        }
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

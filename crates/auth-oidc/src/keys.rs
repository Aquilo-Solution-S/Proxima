//! JWKS key resolution.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use jsonwebtoken::DecodingKey;
use tokio::sync::{Mutex, RwLock};

use crate::config::{OidcConfigError, validate_issuer_url, validate_jwks_url};

/// Minimum spacing between JWKS refetches. Bounds the outbound-fetch rate so a
/// flood of tokens carrying random unknown `kid`s cannot amplify into one
/// upstream JWKS request per inbound request.
const JWKS_REFRESH_COOLDOWN: Duration = Duration::from_mins(1);

/// Maximum age of a cached JWKS before a hit opportunistically refreshes. Picks
/// up key rotation even when every presented `kid` is already cached (so the
/// unknown-kid miss path never fires). Rate-bounded by the same `last_refresh`
/// clock, so a stale hit refetches at most once per age window.
const JWKS_MAX_AGE: Duration = Duration::from_hours(1);

/// Complete-request timeout used by [`HttpJwksResolver::new`]. It covers DNS,
/// connection establishment, response headers, and response-body reads for
/// both discovery and JWKS requests.
pub const DEFAULT_HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Largest complete-request timeout accepted by [`HttpJwksResolver`].
pub const MAX_HTTP_REQUEST_TIMEOUT: Duration = Duration::from_mins(5);

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum KeyError {
    #[error("unknown key id")]
    UnknownKid,
    #[error("jwks fetch failed: {0}")]
    Fetch(String),
    #[error("jwks parse failed: {0}")]
    Parse(String),
    #[error("jwks config invalid: {0}")]
    Config(String),
}

/// Resolves a signing key by `kid`.
#[async_trait]
pub trait KeyResolver: Send + Sync {
    /// # Errors
    ///
    /// Returns [`KeyError::UnknownKid`] when `kid` is unavailable, or a
    /// fetch/parse error when remote JWKS loading fails.
    async fn key_for(&self, kid: &str) -> Result<Arc<DecodingKey>, KeyError>;
}

/// In-memory resolver for tests / pre-shared keys.
pub struct StaticJwksResolver {
    keys: HashMap<String, Arc<DecodingKey>>,
}

impl std::fmt::Debug for StaticJwksResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StaticJwksResolver")
            .field("kids", &self.keys.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl StaticJwksResolver {
    #[must_use]
    pub fn new(keys: HashMap<String, Arc<DecodingKey>>) -> Self {
        Self { keys }
    }
}

#[async_trait]
impl KeyResolver for StaticJwksResolver {
    async fn key_for(&self, kid: &str) -> Result<Arc<DecodingKey>, KeyError> {
        self.keys.get(kid).cloned().ok_or(KeyError::UnknownKid)
    }
}

#[derive(serde::Deserialize)]
struct OpenIdConfig {
    issuer: String,
    jwks_uri: String,
}

#[derive(serde::Deserialize)]
struct Jwk {
    kid: String,
    /// Key type. Missing or non-`RSA` entries are skipped, not errored, so one
    /// EC/OKP key in the set can't fail the whole JWKS parse.
    #[serde(default)]
    kty: String,
    /// RSA modulus / exponent. `Option` so a non-RSA entry (which omits them)
    /// deserializes instead of failing the set.
    n: Option<String>,
    e: Option<String>,
}

#[derive(serde::Deserialize)]
struct JwkSet {
    keys: Vec<Jwk>,
}

/// Production resolver: discovers the JWKS endpoint and caches keys by kid,
/// refreshing once on an unknown kid (key rotation).
/// Discovery metadata must name the configured issuer exactly before its
/// JWKS endpoint can be used.
pub struct HttpJwksResolver {
    issuer: String,
    jwks_uri: Option<String>,
    http: reqwest::Client,
    request_timeout: Duration,
    cache: RwLock<HashMap<String, Arc<DecodingKey>>>,
    /// Last successful JWKS refetch; drives TTL staleness checks.
    last_refresh: RwLock<Option<Instant>>,
    /// Last refetch attempt (success or failure); drives unknown-kid cooldown.
    last_attempt: RwLock<Option<Instant>>,
    /// Serializes the cooldown check, attempt mark, and outbound JWKS fetch.
    refresh_gate: Mutex<()>,
}

impl std::fmt::Debug for HttpJwksResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpJwksResolver")
            .field("issuer", &self.issuer)
            .field("request_timeout", &self.request_timeout)
            .finish_non_exhaustive()
    }
}

impl HttpJwksResolver {
    /// Build a resolver whose discovery and JWKS redirects obey the same
    /// issuer-aware URL policy as the initial endpoints.
    ///
    /// # Errors
    ///
    /// Returns an error when the issuer is not HTTPS or loopback HTTP, or
    /// when the JWKS endpoint is plaintext HTTP that this issuer is not
    /// entitled to name — only a loopback issuer may point at a loopback
    /// JWKS, so a remote provider cannot move key resolution onto the host.
    /// Issuer query and fragment components are rejected.
    pub fn new(issuer: String, jwks_uri: Option<String>) -> Result<Self, OidcConfigError> {
        Self::with_request_timeout(issuer, jwks_uri, DEFAULT_HTTP_REQUEST_TIMEOUT)
    }

    /// Construct a resolver with an explicit complete-request timeout.
    ///
    /// # Errors
    ///
    /// Returns the same URL errors as [`Self::new`], or an error when
    /// `request_timeout` is zero or exceeds [`MAX_HTTP_REQUEST_TIMEOUT`].
    pub fn with_request_timeout(
        issuer: String,
        jwks_uri: Option<String>,
        request_timeout: Duration,
    ) -> Result<Self, OidcConfigError> {
        let http = build_http_client(&issuer, reqwest::Client::builder());
        Self::with_http_client(issuer, jwks_uri, http, request_timeout)
    }

    /// Construct a resolver with an injected HTTP client and an explicit
    /// complete-request timeout. The timeout is applied to each request, so a
    /// client with a weaker or absent default cannot make discovery or JWKS
    /// body reads unbounded.
    ///
    /// The host owns the injected client's redirect policy. It must disable
    /// redirects or validate every target against the issuer's HTTPS/loopback
    /// policy before following it. Unlike the default constructors, this
    /// method cannot replace the policy on an already-built client.
    ///
    /// # Errors
    ///
    /// Returns the same URL and timeout errors as [`Self::with_request_timeout`].
    pub fn with_http_client(
        issuer: String,
        jwks_uri: Option<String>,
        http: reqwest::Client,
        request_timeout: Duration,
    ) -> Result<Self, OidcConfigError> {
        validate_issuer_url(&issuer)?;
        if let Some(uri) = &jwks_uri {
            validate_jwks_url("jwks_uri", uri, &issuer)?;
        }
        validate_request_timeout(request_timeout)?;
        Ok(Self {
            issuer,
            jwks_uri,
            http,
            request_timeout,
            cache: RwLock::new(HashMap::new()),
            last_refresh: RwLock::new(None),
            last_attempt: RwLock::new(None),
            refresh_gate: Mutex::new(()),
        })
    }

    /// Complete-request timeout applied to discovery and JWKS requests.
    #[must_use]
    pub const fn request_timeout(&self) -> Duration {
        self.request_timeout
    }

    async fn jwks_endpoint(&self) -> Result<String, KeyError> {
        if let Some(uri) = &self.jwks_uri {
            return Ok(uri.clone());
        }
        let url = format!(
            "{}/.well-known/openid-configuration",
            self.issuer.trim_end_matches('/')
        );
        let cfg: OpenIdConfig = self
            .http
            .get(url)
            .timeout(self.request_timeout)
            .send()
            .await
            .map_err(|e| KeyError::Fetch(e.to_string()))?
            .error_for_status()
            .map_err(|e| KeyError::Fetch(e.to_string()))?
            .json()
            .await
            .map_err(|e| KeyError::Parse(e.to_string()))?;
        if cfg.issuer != self.issuer {
            return Err(KeyError::Config(
                "discovered issuer does not match configured issuer".into(),
            ));
        }
        validate_jwks_url("discovered jwks_uri", &cfg.jwks_uri, &self.issuer)
            .map_err(|err| KeyError::Config(err.to_string()))?;
        Ok(cfg.jwks_uri)
    }

    async fn refresh(&self) -> Result<(), KeyError> {
        let endpoint = self.jwks_endpoint().await?;
        let set: JwkSet = self
            .http
            .get(endpoint)
            .timeout(self.request_timeout)
            .send()
            .await
            .map_err(|e| KeyError::Fetch(e.to_string()))?
            .error_for_status()
            .map_err(|e| KeyError::Fetch(e.to_string()))?
            .json()
            .await
            .map_err(|e| KeyError::Parse(e.to_string()))?;
        let mut next = HashMap::new();
        for jwk in set.keys {
            // Tolerant parse: only RSA keys are materialized (the verifier pins
            // the RSA family). Skip EC/OKP or component-less entries so a mixed
            // set still yields its RSA keys instead of erroring wholesale.
            // Providers that omit `kty` but publish `n`/`e` are treated as RSA.
            if !is_rsa_jwk(&jwk) {
                continue;
            }
            let (Some(n), Some(e)) = (&jwk.n, &jwk.e) else {
                continue;
            };
            let key = DecodingKey::from_rsa_components(n, e)
                .map_err(|e| KeyError::Parse(e.to_string()))?;
            next.insert(jwk.kid, Arc::new(key));
        }
        if next.is_empty() {
            return Err(KeyError::Parse("jwks contained no RSA keys".into()));
        }
        *self.cache.write().await = next;
        Ok(())
    }

    /// Whether an optional clock is unset or at least `min_age` old.
    async fn clock_due(clock: &RwLock<Option<Instant>>, min_age: Duration) -> bool {
        match *clock.read().await {
            Some(last) => last.elapsed() >= min_age,
            None => true,
        }
    }

    async fn mark_attempt(&self) {
        *self.last_attempt.write().await = Some(Instant::now());
    }
}

fn build_http_client(issuer: &str, builder: reqwest::ClientBuilder) -> reqwest::Client {
    let issuer = issuer.to_owned();
    let policy = reqwest::redirect::Policy::custom(move |attempt| {
        if let Err(error) = validate_jwks_url("redirect target", attempt.url().as_str(), &issuer) {
            return attempt.error(error);
        }
        reqwest::redirect::Policy::default().redirect(attempt)
    });
    builder
        .redirect(policy)
        .build()
        .expect("initialize OIDC HTTP client")
}

fn validate_request_timeout(request_timeout: Duration) -> Result<(), OidcConfigError> {
    if request_timeout.is_zero() || request_timeout > MAX_HTTP_REQUEST_TIMEOUT {
        return Err(OidcConfigError::InvalidTimeout {
            field: "OIDC HTTP request timeout",
            max_seconds: MAX_HTTP_REQUEST_TIMEOUT.as_secs(),
        });
    }
    Ok(())
}

/// True when a JWK entry should be materialized as RSA.
fn is_rsa_jwk(jwk: &Jwk) -> bool {
    if jwk.kty.eq_ignore_ascii_case("RSA") {
        return true;
    }
    jwk.kty.is_empty() && jwk.n.is_some() && jwk.e.is_some()
}

#[async_trait]
impl KeyResolver for HttpJwksResolver {
    async fn key_for(&self, kid: &str) -> Result<Arc<DecodingKey>, KeyError> {
        // Bind (not `if let` on the guard directly) so the read guard drops
        // here — refresh() below takes the write lock and would self-deadlock
        // if the read guard were still held across it.
        let cached = self.cache.read().await.get(kid).cloned();
        if let Some(k) = cached {
            // Cache hit. If the cached set is older than JWKS_MAX_AGE, refresh
            // opportunistically (rate-bounded by the same last_refresh clock).
            // On success, resolve strictly against the fresh set so a rotated-
            // out kid stops validating; on a refresh error, degrade to the
            // still-cached key rather than failing an otherwise-valid request.
            if Self::clock_due(&self.last_refresh, JWKS_MAX_AGE).await {
                let _refresh_guard = self.refresh_gate.lock().await;

                // Another task may have refreshed while this task waited for
                // the gate. Resolve against that fresh set, including a kid
                // that disappeared during rotation.
                if !Self::clock_due(&self.last_refresh, JWKS_MAX_AGE).await {
                    return self
                        .cache
                        .read()
                        .await
                        .get(kid)
                        .cloned()
                        .ok_or(KeyError::UnknownKid);
                }
                if !Self::clock_due(&self.last_attempt, JWKS_REFRESH_COOLDOWN).await {
                    tracing::warn!("jwks cache past max age but refresh throttled");
                    return Ok(k);
                }
                self.mark_attempt().await;
                match self.refresh().await {
                    Ok(()) => {
                        *self.last_refresh.write().await = Some(Instant::now());
                        return self
                            .cache
                            .read()
                            .await
                            .get(kid)
                            .cloned()
                            .ok_or(KeyError::UnknownKid);
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "jwks ttl refresh failed; using stale cached key");
                        return Ok(k);
                    }
                }
            }
            return Ok(k);
        }
        // Cache miss: refetch at most once per cooldown so a stream of
        // unknown-kid tokens can't drive one upstream JWKS fetch per request.
        let _refresh_guard = self.refresh_gate.lock().await;

        // Another task may have fetched this kid while this task waited.
        if let Some(key) = self.cache.read().await.get(kid).cloned() {
            return Ok(key);
        }
        if !Self::clock_due(&self.last_attempt, JWKS_REFRESH_COOLDOWN).await {
            return Err(KeyError::UnknownKid);
        }
        self.mark_attempt().await;
        self.refresh().await?;
        *self.last_refresh.write().await = Some(Instant::now());
        self.cache
            .read()
            .await
            .get(kid)
            .cloned()
            .ok_or(KeyError::UnknownKid)
    }
}

#[cfg(test)]
mod http_tests {
    use std::convert::Infallible;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use axum::{Router, body::Body, http::StatusCode, response::Response, routing::get};
    use tokio::sync::Barrier;

    use crate::config::{OidcConfigError, validate_https_url};

    use super::{
        DEFAULT_HTTP_REQUEST_TIMEOUT, HttpJwksResolver, JWKS_MAX_AGE, JWKS_REFRESH_COOLDOWN,
        KeyError, KeyResolver, MAX_HTTP_REQUEST_TIMEOUT, build_http_client,
    };

    // Static 2048-bit RSA public key as JWK n/e (base64url). Baked so this
    // test needs neither `rsa` nor `rand` (RUSTSEC-2023-0071: the `rsa` crate
    // ships an unfixed Marvin timing sidechannel). The test only serves the
    // public JWK from a mock IdP and resolves it; nothing signs here.
    const TEST_JWK_N: &str = "vcvNMtDvpJExXOyytyqUOWhX2sxa-Xtxd4KmfJ05-iPgT_RiyZzx3UoTuJYtvDCCRcXKU13Rn8cIc0ushWlKpLDW08U4r9bBVctcajpnOumCcuIvnM1_HEiM-WuYPRFk0I5h--ueLA0KhIfPs0ORLpqsvF0XIuL6_uZtObrH9wxPMmG4r5Hh7h3Gm5PchY0R8H7VrEOm79fnra7OGg5nh7XkmStnZnwozODW0FFnpW-kMeCK2-2fzmSWg1A_clFdicji1-xIvk7Wog9CVsZZK9iRHgAIxmsU-Iawb_Wwlwuu-_gIZWFkund24iA2qLktFx_39CORZqfFRNiIsHSvIQ";
    const TEST_JWK_E: &str = "AQAB";

    fn test_key() -> Arc<jsonwebtoken::DecodingKey> {
        Arc::new(
            jsonwebtoken::DecodingKey::from_rsa_components(TEST_JWK_N, TEST_JWK_E)
                .expect("valid baked test key"),
        )
    }

    #[test]
    fn default_request_timeout_is_finite_and_explicit() {
        assert_eq!(DEFAULT_HTTP_REQUEST_TIMEOUT, Duration::from_secs(10));
        let resolver = HttpJwksResolver::new("http://127.0.0.1:4180".into(), None)
            .expect("loopback issuer and default timeout are valid");

        assert_eq!(resolver.request_timeout(), DEFAULT_HTTP_REQUEST_TIMEOUT);
        assert!(!resolver.request_timeout().is_zero());
        assert!(resolver.request_timeout() <= MAX_HTTP_REQUEST_TIMEOUT);
    }

    #[test]
    fn explicit_request_timeout_rejects_zero_and_out_of_range_values() {
        for invalid in [
            Duration::ZERO,
            MAX_HTTP_REQUEST_TIMEOUT + Duration::from_nanos(1),
        ] {
            assert!(matches!(
                HttpJwksResolver::with_request_timeout(
                    "http://127.0.0.1:4180".into(),
                    None,
                    invalid
                ),
                Err(OidcConfigError::InvalidTimeout { .. })
            ));
        }
    }

    #[test]
    fn constructors_reject_issuer_query_or_fragment_components() {
        let http = reqwest::Client::new();
        for base in [
            "https://issuer.example/tenant",
            "http://127.0.0.1:4180/tenant",
        ] {
            for suffix in ["?region=eu", "?", "#anchor", "#"] {
                let issuer = format!("{base}{suffix}");
                for jwks_uri in [None, Some("https://issuer.example/keys?version=1".into())] {
                    for result in [
                        HttpJwksResolver::new(issuer.clone(), jwks_uri.clone()),
                        HttpJwksResolver::with_request_timeout(
                            issuer.clone(),
                            jwks_uri.clone(),
                            DEFAULT_HTTP_REQUEST_TIMEOUT,
                        ),
                        HttpJwksResolver::with_http_client(
                            issuer.clone(),
                            jwks_uri.clone(),
                            http.clone(),
                            DEFAULT_HTTP_REQUEST_TIMEOUT,
                        ),
                    ] {
                        assert!(
                            matches!(
                                result,
                                Err(OidcConfigError::InvalidUrl {
                                    field: "issuer",
                                    ..
                                })
                            ),
                            "issuer component must be rejected: {issuer}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn constructors_preserve_path_issuers_and_jwks_queries() {
        for issuer in [
            "https://issuer.example/tenant/",
            "https://issuer.example/tenant%3Fblue%23anchor",
            "http://127.0.0.1:4180/tenant",
        ] {
            let jwks_uri = "https://issuer.example/keys?version=1";
            let resolver = HttpJwksResolver::new(issuer.into(), Some(jwks_uri.into()))
                .expect("path issuer and JWKS query allowed");

            assert_eq!(resolver.issuer, issuer, "issuer identity is not normalized");
            assert_eq!(resolver.jwks_uri.as_deref(), Some(jwks_uri));
        }
    }

    async fn stalled_body() -> Response {
        let body =
            Body::from_stream(futures::stream::pending::<Result<&'static [u8], Infallible>>());
        Response::builder()
            .header("content-type", "application/json")
            .body(body)
            .expect("static stalled response")
    }

    async fn spawn_stalling_idp() -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test listener");
        let addr = listener.local_addr().expect("read listener addr");
        let issuer = format!("http://{addr}");
        let app = Router::new()
            .route("/.well-known/openid-configuration", get(stalled_body))
            .route("/keys", get(stalled_body));
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("mock idp server failed");
        });
        (issuer, server)
    }

    #[tokio::test]
    async fn stalled_discovery_and_jwks_bodies_time_out() {
        let (issuer, server) = spawn_stalling_idp().await;
        let request_timeout = Duration::from_millis(50);

        for jwks_uri in [None, Some(format!("{issuer}/keys"))] {
            let resolver =
                HttpJwksResolver::with_request_timeout(issuer.clone(), jwks_uri, request_timeout)
                    .expect("short explicit timeout is valid");
            let result = tokio::time::timeout(Duration::from_secs(2), resolver.key_for("k1"))
                .await
                .expect("resolver's request timeout must fire before the test guard");
            assert!(
                matches!(result, Err(KeyError::Parse(_))),
                "stalled response body must terminate through the existing parse-error path: {result:?}"
            );
        }

        server.abort();
    }

    async fn seed_stale_key(
        resolver: &HttpJwksResolver,
        kid: &str,
    ) -> Arc<jsonwebtoken::DecodingKey> {
        let key = test_key();
        resolver
            .cache
            .write()
            .await
            .insert(kid.to_string(), Arc::clone(&key));
        *resolver.last_refresh.write().await = Instant::now()
            .checked_sub(JWKS_MAX_AGE)
            .and_then(|time| time.checked_sub(Duration::from_secs(1)));
        key
    }

    #[tokio::test]
    async fn discovers_and_loads_jwks() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test listener");
        let addr = listener.local_addr().expect("read listener addr");
        let issuer = format!("http://{addr}");
        let jwks_uri = format!("{issuer}/keys");
        let openid = serde_json::json!({ "issuer": issuer, "jwks_uri": jwks_uri }).to_string();
        let jwks = serde_json::json!({
            "keys": [{ "kid": "k1", "kty": "RSA", "n": TEST_JWK_N, "e": TEST_JWK_E }]
        })
        .to_string();

        let app = Router::new()
            .route(
                "/.well-known/openid-configuration",
                get({
                    let openid = openid.clone();
                    move || async move { openid.clone() }
                }),
            )
            .route(
                "/keys",
                get({
                    let jwks = jwks.clone();
                    move || async move { jwks.clone() }
                }),
            );
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("mock idp server failed");
        });

        let resolver = HttpJwksResolver::new(issuer, None).expect("loopback http allowed in tests");
        assert!(resolver.key_for("k1").await.is_ok());
        assert!(matches!(
            resolver.key_for("missing").await,
            Err(KeyError::UnknownKid)
        ));

        server.abort();
    }

    struct DiscoveryServer {
        issuer: String,
        jwks_uri: String,
        discovery_fetches: Arc<AtomicUsize>,
        jwks_fetches: Arc<AtomicUsize>,
        task: tokio::task::JoinHandle<()>,
    }

    impl Drop for DiscoveryServer {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    async fn spawn_discovery_idp(
        metadata_issuer: impl FnOnce(&str) -> Option<serde_json::Value>,
    ) -> DiscoveryServer {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test listener");
        let addr = listener.local_addr().expect("listener address");
        let issuer = format!("http://{addr}/tenant/");
        let jwks_uri = format!("http://{addr}/keys?version=1");
        let mut metadata = serde_json::json!({ "jwks_uri": jwks_uri });
        if let Some(value) = metadata_issuer(&issuer) {
            metadata["issuer"] = value;
        }
        let metadata = metadata.to_string();
        let jwks = serde_json::json!({
            "keys": [{ "kid": "k1", "kty": "RSA", "n": TEST_JWK_N, "e": TEST_JWK_E }]
        })
        .to_string();
        let discovery_fetches = Arc::new(AtomicUsize::new(0));
        let jwks_fetches = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route(
                "/tenant/.well-known/openid-configuration",
                get({
                    let fetches = Arc::clone(&discovery_fetches);
                    move || {
                        let fetches = Arc::clone(&fetches);
                        let metadata = metadata.clone();
                        async move {
                            fetches.fetch_add(1, Ordering::SeqCst);
                            metadata
                        }
                    }
                }),
            )
            .route(
                "/keys",
                get({
                    let fetches = Arc::clone(&jwks_fetches);
                    move |query: axum::extract::RawQuery| {
                        let fetches = Arc::clone(&fetches);
                        let jwks = jwks.clone();
                        async move {
                            fetches.fetch_add(1, Ordering::SeqCst);
                            assert_eq!(query.0.as_deref(), Some("version=1"));
                            jwks
                        }
                    }
                }),
            );
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("mock discovery server");
        });
        DiscoveryServer {
            issuer,
            jwks_uri,
            discovery_fetches,
            jwks_fetches,
            task,
        }
    }

    #[tokio::test]
    async fn discovery_issuer_must_match_exactly_before_jwks_fetch() {
        let mismatches: [fn(&str) -> String; 3] = [
            |_| "http://another.example".into(),
            |issuer| issuer.trim_end_matches('/').into(),
            |issuer| issuer.replace("/tenant/", "/TENANT/"),
        ];
        for mismatch in mismatches {
            let server =
                spawn_discovery_idp(|issuer| Some(serde_json::json!(mismatch(issuer)))).await;
            let resolver = HttpJwksResolver::new(server.issuer.clone(), None).expect("issuer");

            assert!(matches!(
                resolver.key_for("k1").await,
                Err(KeyError::Config(_))
            ));
            assert_eq!(server.discovery_fetches.load(Ordering::SeqCst), 1);
            assert_eq!(server.jwks_fetches.load(Ordering::SeqCst), 0);
            assert!(resolver.cache.read().await.is_empty());
        }
    }

    #[tokio::test]
    async fn discovery_issuer_is_a_required_string() {
        for issuer in [
            None,
            Some(serde_json::Value::Null),
            Some(serde_json::json!(7)),
        ] {
            let server = spawn_discovery_idp(|_| issuer).await;
            let resolver = HttpJwksResolver::new(server.issuer.clone(), None).expect("issuer");

            assert!(matches!(
                resolver.key_for("k1").await,
                Err(KeyError::Parse(_))
            ));
            assert_eq!(server.discovery_fetches.load(Ordering::SeqCst), 1);
            assert_eq!(server.jwks_fetches.load(Ordering::SeqCst), 0);
            assert!(resolver.cache.read().await.is_empty());
        }
    }

    #[tokio::test]
    async fn matching_discovery_issuer_preserves_path_and_jwks_query() {
        let server = spawn_discovery_idp(|issuer| Some(serde_json::json!(issuer))).await;
        let resolver = HttpJwksResolver::new(server.issuer.clone(), None).expect("issuer");

        assert!(resolver.key_for("k1").await.is_ok());
        assert_eq!(server.discovery_fetches.load(Ordering::SeqCst), 1);
        assert_eq!(server.jwks_fetches.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn explicit_jwks_endpoint_skips_discovery_metadata() {
        let server =
            spawn_discovery_idp(|_| Some(serde_json::json!("http://another.example"))).await;
        let resolver = HttpJwksResolver::new(server.issuer.clone(), Some(server.jwks_uri.clone()))
            .expect("explicit JWKS");

        assert!(resolver.key_for("k1").await.is_ok());
        assert_eq!(server.discovery_fetches.load(Ordering::SeqCst), 0);
        assert_eq!(server.jwks_fetches.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn discovery_issuer_mismatch_preserves_only_previously_cached_keys() {
        let server =
            spawn_discovery_idp(|_| Some(serde_json::json!("http://another.example"))).await;
        let resolver = HttpJwksResolver::new(server.issuer.clone(), None).expect("issuer");
        let cached = seed_stale_key(&resolver, "old").await;

        let returned = resolver.key_for("old").await.expect("trusted cached key");

        assert!(Arc::ptr_eq(&returned, &cached));
        assert_eq!(server.discovery_fetches.load(Ordering::SeqCst), 1);
        assert_eq!(server.jwks_fetches.load(Ordering::SeqCst), 0);
        assert!(matches!(
            resolver.key_for("k1").await,
            Err(KeyError::UnknownKid)
        ));
        assert_eq!(resolver.cache.read().await.len(), 1);
        assert!(resolver.cache.read().await.contains_key("old"));
    }

    #[tokio::test]
    async fn plaintext_nonloopback_redirect_is_rejected_before_request() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test listener");
        let addr = listener.local_addr().expect("read listener addr");
        let issuer = format!("http://{addr}");
        let destination = format!("http://redirect.example:{}/redirect-target", addr.port());
        let fetches = Arc::new(AtomicUsize::new(0));
        let jwks = serde_json::json!({
            "keys": [{ "kid": "k1", "kty": "RSA", "n": TEST_JWK_N, "e": TEST_JWK_E }]
        })
        .to_string();
        let redirect = move || async move { axum::response::Redirect::temporary(&destination) };
        let intermediate = || async { axum::response::Redirect::temporary("/intermediate") };
        let app = Router::new()
            .route("/keys", get(intermediate))
            .route("/.well-known/openid-configuration", get(intermediate))
            .route("/intermediate", get(redirect))
            .route(
                "/redirect-target",
                get({
                    let fetches = Arc::clone(&fetches);
                    move || {
                        let fetches = Arc::clone(&fetches);
                        let jwks = jwks.clone();
                        async move {
                            fetches.fetch_add(1, Ordering::SeqCst);
                            jwks
                        }
                    }
                }),
            );
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("mock idp server");
        });
        let http = build_http_client(
            &issuer,
            reqwest::Client::builder()
                .no_proxy()
                .resolve("redirect.example", addr),
        );
        for jwks_uri in [None, Some(format!("{issuer}/keys"))] {
            let resolver = HttpJwksResolver::with_http_client(
                issuer.clone(),
                jwks_uri,
                http.clone(),
                DEFAULT_HTTP_REQUEST_TIMEOUT,
            )
            .expect("loopback issuer");

            let result = resolver.key_for("k1").await;

            assert_eq!(fetches.load(Ordering::SeqCst), 0);
            assert!(matches!(result, Err(KeyError::Fetch(_))));
        }
        server.abort();
    }

    /// Serves `jwks` from a loopback mock `IdP`, counting `/keys` fetches.
    /// Returns `(issuer, fetch_counter, server_handle)`.
    async fn spawn_mock_idp(
        jwks: String,
    ) -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test listener");
        let addr = listener.local_addr().expect("read listener addr");
        let issuer = format!("http://{addr}");
        let openid = serde_json::json!({
            "issuer": issuer,
            "jwks_uri": format!("{issuer}/keys")
        })
        .to_string();
        let fetches = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route(
                "/.well-known/openid-configuration",
                get(move || async move { openid.clone() }),
            )
            .route(
                "/keys",
                get({
                    let fetches = Arc::clone(&fetches);
                    move || {
                        let jwks = jwks.clone();
                        let fetches = Arc::clone(&fetches);
                        async move {
                            fetches.fetch_add(1, Ordering::SeqCst);
                            jwks
                        }
                    }
                }),
            )
            .route(
                "/redirect/keys",
                get(|| async { axum::response::Redirect::temporary("/keys") }),
            )
            .route(
                "/redirect/loop",
                get({
                    let fetches = Arc::clone(&fetches);
                    move || {
                        let fetches = Arc::clone(&fetches);
                        async move {
                            fetches.fetch_add(1, Ordering::SeqCst);
                            axum::response::Redirect::temporary("/redirect/loop")
                        }
                    }
                }),
            );
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("mock idp server failed");
        });
        (issuer, fetches, server)
    }

    #[tokio::test]
    async fn default_client_follows_valid_loopback_redirect() {
        let jwks = serde_json::json!({
            "keys": [{ "kid": "k1", "kty": "RSA", "n": TEST_JWK_N, "e": TEST_JWK_E }]
        })
        .to_string();
        let (issuer, fetches, server) = spawn_mock_idp(jwks).await;
        let resolver =
            HttpJwksResolver::new(issuer.clone(), Some(format!("{issuer}/redirect/keys")))
                .expect("loopback issuer");

        assert!(resolver.key_for("k1").await.is_ok());
        assert_eq!(fetches.load(Ordering::SeqCst), 1);
        server.abort();
    }

    #[tokio::test]
    async fn default_client_limits_redirect_loops() {
        let (issuer, fetches, server) = spawn_mock_idp(String::new()).await;
        let resolver =
            HttpJwksResolver::new(issuer.clone(), Some(format!("{issuer}/redirect/loop")))
                .expect("loopback issuer");

        assert!(matches!(
            resolver.key_for("k1").await,
            Err(KeyError::Fetch(_))
        ));
        assert_eq!(fetches.load(Ordering::SeqCst), 11);
        server.abort();
    }

    /// Serves a failing explicit JWKS endpoint and counts fetch attempts.
    async fn spawn_failing_jwks() -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test listener");
        let addr = listener.local_addr().expect("read listener addr");
        let issuer = format!("http://{addr}");
        let fetches = Arc::new(AtomicUsize::new(0));
        let app = Router::new().route(
            "/keys",
            get({
                let fetches = Arc::clone(&fetches);
                move || {
                    let fetches = Arc::clone(&fetches);
                    async move {
                        fetches.fetch_add(1, Ordering::SeqCst);
                        StatusCode::SERVICE_UNAVAILABLE
                    }
                }
            }),
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("mock idp server failed");
        });
        (issuer, fetches, server)
    }

    #[tokio::test]
    async fn non_rsa_entry_does_not_break_rsa_resolution() {
        // A mixed set: an EC key and an OKP key (each lacking n/e) alongside
        // the RSA key. The tolerant parse must skip the non-RSA entries and
        // still resolve the RSA kid rather than erroring the whole set.
        let jwks = serde_json::json!({
            "keys": [
                { "kid": "ec1", "kty": "EC", "crv": "P-256", "x": "AQ", "y": "AQ" },
                { "kid": "okp1", "kty": "OKP", "crv": "Ed25519", "x": "AQ" },
                { "kid": "k1", "kty": "RSA", "n": TEST_JWK_N, "e": TEST_JWK_E }
            ]
        })
        .to_string();
        let (issuer, _fetches, server) = spawn_mock_idp(jwks).await;

        let resolver = HttpJwksResolver::new(issuer, None).expect("loopback http allowed in tests");
        assert!(resolver.key_for("k1").await.is_ok());
        assert!(matches!(
            resolver.key_for("ec1").await,
            Err(KeyError::UnknownKid)
        ));

        server.abort();
    }

    #[tokio::test]
    async fn rsa_jwk_without_kty_is_materialized() {
        let jwks = serde_json::json!({
            "keys": [{ "kid": "k1", "n": TEST_JWK_N, "e": TEST_JWK_E }]
        })
        .to_string();
        let (issuer, _fetches, server) = spawn_mock_idp(jwks).await;

        let resolver = HttpJwksResolver::new(issuer, None).expect("loopback http allowed in tests");
        assert!(resolver.key_for("k1").await.is_ok());

        server.abort();
    }

    #[tokio::test]
    async fn stale_cache_past_max_age_refetches_on_hit() {
        let jwks = serde_json::json!({
            "keys": [{ "kid": "k1", "kty": "RSA", "n": TEST_JWK_N, "e": TEST_JWK_E }]
        })
        .to_string();
        let (issuer, fetches, server) = spawn_mock_idp(jwks).await;

        let resolver = HttpJwksResolver::new(issuer, None).expect("loopback http allowed in tests");
        // Prime the cache: one fetch, then a plain hit does NOT refetch.
        assert!(resolver.key_for("k1").await.is_ok());
        assert_eq!(fetches.load(Ordering::SeqCst), 1);
        assert!(resolver.key_for("k1").await.is_ok());
        assert_eq!(fetches.load(Ordering::SeqCst), 1);

        // Age the cache past JWKS_MAX_AGE and clear the refresh cooldown so the
        // ttl-driven refetch is not blocked by the initial miss-path fetch.
        let stale = Instant::now().checked_sub(JWKS_MAX_AGE + Duration::from_secs(1));
        *resolver.last_refresh.write().await = stale;
        *resolver.last_attempt.write().await = stale.and_then(|t| {
            t.checked_sub(JWKS_REFRESH_COOLDOWN)
                .and_then(|t| t.checked_sub(Duration::from_secs(1)))
        });

        // A hit on the stale set triggers exactly one refetch and still resolves.
        assert!(resolver.key_for("k1").await.is_ok());
        assert_eq!(fetches.load(Ordering::SeqCst), 2);

        server.abort();
    }

    #[tokio::test]
    async fn stale_cache_uses_cached_key_when_refresh_fails() {
        let (issuer, fetches, server) = spawn_failing_jwks().await;
        let resolver = HttpJwksResolver::new(issuer.clone(), Some(format!("{issuer}/keys")))
            .expect("loopback http allowed in tests");
        let cached = seed_stale_key(&resolver, "k1").await;
        *resolver.last_attempt.write().await = Instant::now()
            .checked_sub(JWKS_REFRESH_COOLDOWN)
            .and_then(|time| time.checked_sub(Duration::from_secs(1)));

        let returned_key = resolver
            .key_for("k1")
            .await
            .expect("stale cached key must survive an IdP outage");

        assert!(Arc::ptr_eq(&returned_key, &cached));
        assert_eq!(fetches.load(Ordering::SeqCst), 1);
        server.abort();
    }

    #[tokio::test]
    async fn stale_cache_uses_cached_key_when_refresh_has_no_rsa_keys() {
        let jwks = serde_json::json!({
            "keys": [{ "kid": "ec1", "kty": "EC", "crv": "P-256", "x": "AQ", "y": "AQ" }]
        })
        .to_string();
        let (issuer, fetches, server) = spawn_mock_idp(jwks).await;
        let resolver = HttpJwksResolver::new(issuer.clone(), Some(format!("{issuer}/keys")))
            .expect("loopback http allowed in tests");
        let cached = seed_stale_key(&resolver, "k1").await;
        *resolver.last_attempt.write().await = Instant::now()
            .checked_sub(JWKS_REFRESH_COOLDOWN)
            .and_then(|time| time.checked_sub(Duration::from_secs(1)));

        let returned_key = resolver
            .key_for("k1")
            .await
            .expect("stale cached key must survive an empty RSA refresh");

        assert!(Arc::ptr_eq(&returned_key, &cached));
        assert!(resolver.cache.read().await.contains_key("k1"));
        assert_eq!(fetches.load(Ordering::SeqCst), 1);
        server.abort();
    }

    #[tokio::test]
    async fn stale_cache_uses_cached_key_when_refresh_is_throttled() {
        let jwks = serde_json::json!({ "keys": [] }).to_string();
        let (issuer, fetches, server) = spawn_mock_idp(jwks).await;
        let resolver = HttpJwksResolver::new(issuer.clone(), Some(format!("{issuer}/keys")))
            .expect("loopback http allowed in tests");
        let cached = seed_stale_key(&resolver, "k1").await;
        *resolver.last_attempt.write().await = Some(Instant::now());

        let returned_key = resolver
            .key_for("k1")
            .await
            .expect("throttling must not discard a stale cached key");

        assert!(Arc::ptr_eq(&returned_key, &cached));
        assert_eq!(fetches.load(Ordering::SeqCst), 0);
        server.abort();
    }

    #[tokio::test]
    async fn cache_miss_returns_fetch_error_when_refresh_fails() {
        let (issuer, fetches, server) = spawn_failing_jwks().await;
        let resolver = HttpJwksResolver::new(issuer.clone(), Some(format!("{issuer}/keys")))
            .expect("loopback http allowed in tests");

        assert!(matches!(
            resolver.key_for("missing").await,
            Err(KeyError::Fetch(_))
        ));
        assert_eq!(fetches.load(Ordering::SeqCst), 1);
        server.abort();
    }

    #[tokio::test]
    async fn concurrent_cache_misses_share_one_refresh_attempt() {
        const CALLERS: usize = 16;

        let jwks = serde_json::json!({
            "keys": [{ "kid": "k1", "kty": "RSA", "n": TEST_JWK_N, "e": TEST_JWK_E }]
        })
        .to_string();
        let (issuer, fetches, server) = spawn_mock_idp(jwks).await;
        let resolver = Arc::new(
            HttpJwksResolver::new(issuer.clone(), Some(format!("{issuer}/keys")))
                .expect("loopback http allowed in tests"),
        );
        let barrier = Arc::new(Barrier::new(CALLERS + 1));
        let mut tasks = Vec::with_capacity(CALLERS);
        for _ in 0..CALLERS {
            let resolver = Arc::clone(&resolver);
            let barrier = Arc::clone(&barrier);
            tasks.push(tokio::spawn(async move {
                barrier.wait().await;
                resolver.key_for("missing").await
            }));
        }

        barrier.wait().await;
        for task in tasks {
            assert!(matches!(
                task.await.expect("key lookup task completed"),
                Err(KeyError::UnknownKid)
            ));
        }
        assert_eq!(fetches.load(Ordering::SeqCst), 1);
        server.abort();
    }

    #[test]
    fn rejects_discovered_http_jwks_uri() {
        assert!(matches!(
            validate_https_url("discovered jwks_uri", "http://issuer.example/keys"),
            Err(OidcConfigError::InsecureUrl {
                field: "discovered jwks_uri",
                ..
            })
        ));
    }
}

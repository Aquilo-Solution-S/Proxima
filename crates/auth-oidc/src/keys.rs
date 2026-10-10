//! JWKS key resolution.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::{Algorithm, AlgorithmFamily, DecodingKey};
use serde::Deserialize as _;
use tokio::sync::{Mutex, RwLock};

use crate::authenticator::verified_algorithms;
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

/// In-memory resolver for pre-shared keys: tests, and a host verifying
/// against a JWKS pinned in its configuration ([`Self::from_jwks_json`]).
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

    /// Build a resolver from a JWKS document shipped in configuration, so a
    /// host with no network path to its issuer still verifies its tokens.
    ///
    /// Strict where [`HttpJwksResolver`] is tolerant. A fetched set is the
    /// issuer's to publish, and one unsupported key in it must not take down
    /// the others beside it. A pinned set is the operator's, and an entry this
    /// resolver skipped would be a key the operator believes is trusted and
    /// is not, so every entry must be a named verification key of one of
    /// three types: `kty: RSA` (RS256/RS384/RS512), `kty: EC` with
    /// `crv: P-256` (ES256), or `kty: OKP` with `crv: Ed25519` (`EdDSA`).
    ///
    /// # Errors
    ///
    /// [`KeyError::Parse`] when the document is not a JWKS or an RSA key's
    /// `n`/`e` do not decode. [`KeyError::Config`] when the set is empty, or an
    /// entry has no `kid`, repeats one, has another `kty`, lacks `n`/`e` (RSA),
    /// `crv`/`x`/`y` (EC) or `crv`/`x` (OKP), names another curve, has a
    /// coordinate that is not 32 bytes of base64url, names an `alg` that does
    /// not belong to its `kty` or a `use` other than `sig`, or carries the
    /// private part `d`.
    pub fn from_jwks_json(raw: &str) -> Result<Self, KeyError> {
        let set: JwkSet =
            serde_json::from_str(raw).map_err(|err| KeyError::Parse(err.to_string()))?;
        if set.keys.is_empty() {
            return Err(KeyError::Config("jwks contains no keys".into()));
        }
        let mut keys = HashMap::with_capacity(set.keys.len());
        for (index, entry) in set.keys.into_iter().enumerate() {
            let jwk = serde_json::from_value::<Jwk>(entry).map_err(|err| {
                KeyError::Config(format!("key {index} is not a well-formed JWK: {err}"))
            })?;
            let Some(kid) = jwk.kid.clone().filter(|kid| !kid.trim().is_empty()) else {
                return Err(KeyError::Config(format!("key {index} has no kid")));
            };
            let refuse = |reason: &str| Err(KeyError::Config(format!("key {kid:?} {reason}")));
            let key = if jwk.kty == "RSA" {
                if let Err(reason) = jwk.check_public_signing_key(AlgorithmFamily::Rsa) {
                    return refuse(&reason);
                }
                let (Some(n), Some(e)) = (&jwk.n, &jwk.e) else {
                    return refuse("lacks n or e");
                };
                DecodingKey::from_rsa_components(n, e)
                    .map_err(|err| KeyError::Parse(format!("key {kid:?}: {err}")))?
            } else {
                match jwk.ec_or_okp_key() {
                    Ok(key) => key,
                    Err(reason) => return refuse(&reason),
                }
            };
            if keys.contains_key(&kid) {
                return refuse("appears twice");
            }
            keys.insert(kid, Arc::new(key));
        }
        Ok(Self::new(keys))
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

/// One JWKS entry, as published by an issuer or shipped in configuration.
/// Every member is optional so a refusal names what is missing instead of a
/// serde position.
#[derive(serde::Deserialize)]
struct Jwk {
    /// Optional in JWK sets; an unnamed key cannot satisfy a kid lookup.
    kid: Option<String>,
    /// Key type: `RSA`, `EC` or `OKP`. Anything else, or a missing one, is an
    /// unsupported entry (skipped when fetched, refused when pinned), so one
    /// such key in the set can't fail the whole JWKS parse.
    #[serde(default)]
    kty: String,
    /// RSA modulus / exponent. `Option` so an EC/OKP entry (which omits them)
    /// deserializes instead of failing the set.
    n: Option<String>,
    e: Option<String>,
    /// EC/OKP curve and public coordinates (base64url): `x` and `y` for EC,
    /// `x` alone for OKP.
    crv: Option<String>,
    x: Option<String>,
    y: Option<String>,
    alg: Option<String>,
    #[serde(rename = "use")]
    key_use: Option<String>,
    /// Private part. Never read, only noticed.
    d: Option<serde::de::IgnoredAny>,
}

/// Length of a P-256 coordinate and of an Ed25519 public key.
const COORDINATE_LEN: usize = 32;

impl Jwk {
    /// The members every verification key shares, whatever its type: an `alg`
    /// that belongs to the key's family, `use` of `sig` if named, and no
    /// private part. A key is never widened by its `alg`; this only refuses a
    /// contradiction.
    fn check_public_signing_key(&self, family: AlgorithmFamily) -> Result<(), String> {
        if let Some(alg) = &self.alg
            && !alg
                .parse::<Algorithm>()
                .is_ok_and(|alg| verified_algorithms(family).contains(&alg))
        {
            return Err(format!(
                "has alg {alg:?}; kty {:?} verifies only {:?}",
                self.kty,
                verified_algorithms(family)
            ));
        }
        if let Some(key_use) = &self.key_use
            && key_use != "sig"
        {
            return Err(format!("has use {key_use:?}; only sig"));
        }
        // Only the public half belongs in a JWKS. A private key in a pinned
        // one would let anyone who can read the config mint tokens.
        if self.d.is_some() {
            return Err("carries private key material (d)".into());
        }
        Ok(())
    }

    /// A P-256 (`kty: EC`) or Ed25519 (`kty: OKP`) public key, or the reason
    /// this entry is none. The key's family comes from `kty` alone.
    fn ec_or_okp_key(&self) -> Result<DecodingKey, String> {
        let key = match self.kty.as_str() {
            "EC" => {
                self.check_public_signing_key(AlgorithmFamily::Ec)?;
                self.check_curve("P-256")?;
                let x = coordinate("x", self.x.as_deref())?;
                let y = coordinate("y", self.y.as_deref())?;
                DecodingKey::from_ec_components(x, y)
            }
            "OKP" => {
                self.check_public_signing_key(AlgorithmFamily::Ed)?;
                self.check_curve("Ed25519")?;
                DecodingKey::from_ed_components(coordinate("x", self.x.as_deref())?)
            }
            other => {
                return Err(format!(
                    "has kty {other:?}; only RSA, EC and OKP are verified"
                ));
            }
        };
        key.map_err(|err| format!("has an unusable key: {err}"))
    }

    fn check_curve(&self, supported: &str) -> Result<(), String> {
        match self.crv.as_deref() {
            Some(crv) if crv == supported => Ok(()),
            Some(crv) => Err(format!(
                "has crv {crv:?}; kty {:?} verifies only {supported}",
                self.kty
            )),
            None => Err("lacks crv".into()),
        }
    }
}

/// The members a fetched entry is first read by, before anything else is
/// asked of it: all an RSA key needs, and what the HTTP resolver has always
/// read. Other members, wrong-typed ones included, are ignored here.
#[derive(serde::Deserialize)]
struct FetchedJwk {
    kid: Option<String>,
    #[serde(default)]
    kty: String,
    n: Option<String>,
    e: Option<String>,
}

impl FetchedJwk {
    /// True when the entry should be materialized as RSA.
    fn is_rsa(&self) -> bool {
        self.kty.eq_ignore_ascii_case("RSA")
            || (self.kty.is_empty() && self.n.is_some() && self.e.is_some())
    }
}

/// A public coordinate: present, base64url, and exactly [`COORDINATE_LEN`]
/// bytes once decoded (RFC 7518 section 6.2.1.2 keeps leading zeros).
fn coordinate<'a>(name: &str, value: Option<&'a str>) -> Result<&'a str, String> {
    let value = value.ok_or_else(|| format!("lacks {name}"))?;
    match URL_SAFE_NO_PAD.decode(value) {
        Ok(bytes) if bytes.len() == COORDINATE_LEN => Ok(value),
        Ok(bytes) => Err(format!(
            "has an {name} of {} bytes; {COORDINATE_LEN} required",
            bytes.len()
        )),
        Err(_) => Err(format!("has an {name} that is not base64url")),
    }
}

/// A JWKS document. Entries stay raw JSON and are read into a [`Jwk`] one by
/// one, so an entry with a wrong-typed member fails only itself, never the
/// keys beside it: the HTTP resolver skips it, a pinned set refuses it.
#[derive(serde::Deserialize)]
struct JwkSet {
    keys: Vec<serde_json::Value>,
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
        for (index, entry) in set.keys.into_iter().enumerate() {
            // Tolerant parse: an unnamed, unsupported or malformed entry (a
            // wrong-typed member included) is skipped, so one such key in the
            // issuer's set doesn't fail the others. An RSA entry is read from
            // its four members alone, so members it never needed cannot sink
            // it. The other key types are P-256 EC and Ed25519 OKP; such an
            // entry with any fault (another curve, a coordinate that is not 32
            // bytes, an `alg` that contradicts its `kty`, a `use` other than
            // `sig`, private material) is skipped, never half-trusted.
            // Providers that omit `kty` but publish `n`/`e` are treated as RSA.
            let Ok(fetched) = FetchedJwk::deserialize(&entry) else {
                tracing::debug!(index, "jwks entry is not a well-formed JWK; skipped");
                continue;
            };
            let Some(kid) = fetched.kid.clone() else {
                continue;
            };
            let key = if fetched.is_rsa() {
                let (Some(n), Some(e)) = (&fetched.n, &fetched.e) else {
                    continue;
                };
                DecodingKey::from_rsa_components(n, e)
                    .map_err(|e| KeyError::Parse(e.to_string()))?
            } else {
                match Jwk::deserialize(&entry)
                    .map_err(|err| err.to_string())
                    .and_then(|jwk| jwk.ec_or_okp_key())
                {
                    Ok(key) => key,
                    Err(reason) => {
                        tracing::debug!(%kid, %reason, "jwks entry is not a usable verification key; skipped");
                        continue;
                    }
                }
            };
            next.insert(kid, Arc::new(key));
        }
        if next.is_empty() {
            return Err(KeyError::Parse(
                "jwks contained no named RSA, P-256 or Ed25519 keys".into(),
            ));
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
mod fixtures {
    //! JWK entries shared by both resolvers' tests, so "the pinned set
    //! refuses it" and "the HTTP resolver skips it" are about the same
    //! entries.

    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use serde_json::{Value, json};

    // Public example keys from RFC 7517 appendix A.1 (P-256) and RFC 8037
    // appendix A.2 (Ed25519). No private half is shipped here.
    pub(super) const EC_X: &str = "f83OJ3D2xF1Bg8vub9tLe1gHMzV76e8Tus9uPHvRVEU";
    pub(super) const EC_Y: &str = "x_FEzRu9m36HLN_tue659LNpXW6pCyStikYjKIWI5a0";
    pub(super) const OKP_X: &str = "11qYAYKxCrfVS_7TyWQHOg7hcvPapiMlrwIaaPcHURo";

    pub(super) fn ec_key(kid: &str) -> Value {
        json!({
            "kty": "EC", "kid": kid, "crv": "P-256", "alg": "ES256", "use": "sig",
            "x": EC_X, "y": EC_Y
        })
    }

    pub(super) fn okp_key(kid: &str) -> Value {
        json!({
            "kty": "OKP", "kid": kid, "crv": "Ed25519", "alg": "EdDSA", "use": "sig",
            "x": OKP_X
        })
    }

    pub(super) fn with(mut key: Value, member: &str, value: Value) -> Value {
        key[member] = value;
        key
    }

    pub(super) fn without(mut key: Value, member: &str) -> Value {
        key.as_object_mut().expect("jwk object").remove(member);
        key
    }

    /// `len` bytes as base64url.
    fn coordinate(len: usize) -> Value {
        json!(URL_SAFE_NO_PAD.encode(vec![1_u8; len]))
    }

    /// Entries whose members have the wrong JSON type, one member each: not
    /// JWKs at all, so neither resolver can read them into a key.
    pub(super) fn mistyped_entries() -> Vec<Value> {
        let mistyped = |entry: Value, member: &str| with(entry, member, json!(7));
        vec![
            mistyped(ec_key("ec-crv-7"), "crv"),
            mistyped(ec_key("ec-x-7"), "x"),
            mistyped(ec_key("ec-y-7"), "y"),
            mistyped(ec_key("ec-alg-7"), "alg"),
            mistyped(ec_key("ec-use-7"), "use"),
            mistyped(ec_key("ec-kty-7"), "kty"),
            mistyped(ec_key("ec-kid-7"), "kid"),
            mistyped(okp_key("okp-x-7"), "x"),
            mistyped(json!({ "kid": "rsa-n-7", "kty": "RSA", "e": "AQAB" }), "n"),
            with(ec_key("ec-all-mistyped"), "crv", json!(["P-256"])),
            json!("not an object"),
            json!(7),
            json!(null),
        ]
    }

    /// EC and OKP entries with exactly one fault each, named by their `kid`:
    /// everything a key-type-specific check must refuse.
    pub(super) fn faulty_ec_and_okp_entries() -> Vec<Value> {
        let ec = |fault: &str| ec_key(fault);
        let okp = |fault: &str| okp_key(fault);
        vec![
            without(ec("ec-no-crv"), "crv"),
            without(ec("ec-no-x"), "x"),
            without(ec("ec-no-y"), "y"),
            with(ec("ec-p384"), "crv", json!("P-384")),
            with(ec("ec-ed25519-curve"), "crv", json!("Ed25519")),
            with(ec("ec-lowercase-curve"), "crv", json!("p-256")),
            with(ec("ec-short-x"), "x", coordinate(31)),
            with(ec("ec-long-x"), "x", coordinate(33)),
            with(ec("ec-short-y"), "y", coordinate(31)),
            with(ec("ec-long-y"), "y", coordinate(33)),
            with(with(ec("ec-truncated"), "x", json!("AQ")), "y", json!("AQ")),
            with(ec("ec-not-base64"), "x", json!("!!not base64url!!")),
            with(ec("ec-alg-rs256"), "alg", json!("RS256")),
            with(ec("ec-alg-eddsa"), "alg", json!("EdDSA")),
            with(ec("ec-alg-es384"), "alg", json!("ES384")),
            with(ec("ec-alg-hs256"), "alg", json!("HS256")),
            with(ec("ec-alg-none"), "alg", json!("none")),
            with(ec("ec-use-enc"), "use", json!("enc")),
            with(ec("ec-private"), "d", json!("AQAB")),
            without(okp("okp-no-crv"), "crv"),
            without(okp("okp-no-x"), "x"),
            with(
                with(okp("okp-ed448"), "crv", json!("Ed448")),
                "x",
                coordinate(57),
            ),
            with(okp("okp-x25519"), "crv", json!("X25519")),
            with(okp("okp-p256-curve"), "crv", json!("P-256")),
            with(okp("okp-short-x"), "x", coordinate(31)),
            with(okp("okp-long-x"), "x", coordinate(33)),
            with(okp("okp-truncated"), "x", json!("AQ")),
            with(okp("okp-not-base64"), "x", json!("!!not base64url!!")),
            with(okp("okp-alg-es256"), "alg", json!("ES256")),
            with(okp("okp-alg-rs256"), "alg", json!("RS256")),
            with(okp("okp-alg-hs256"), "alg", json!("HS256")),
            with(okp("okp-use-enc"), "use", json!("enc")),
            with(okp("okp-private"), "d", json!("AQAB")),
        ]
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
    pub(super) const TEST_JWK_N: &str = "vcvNMtDvpJExXOyytyqUOWhX2sxa-Xtxd4KmfJ05-iPgT_RiyZzx3UoTuJYtvDCCRcXKU13Rn8cIc0ushWlKpLDW08U4r9bBVctcajpnOumCcuIvnM1_HEiM-WuYPRFk0I5h--ueLA0KhIfPs0ORLpqsvF0XIuL6_uZtObrH9wxPMmG4r5Hh7h3Gm5PchY0R8H7VrEOm79fnra7OGg5nh7XkmStnZnwozODW0FFnpW-kMeCK2-2fzmSWg1A_clFdicji1-xIvk7Wog9CVsZZK9iRHgAIxmsU-Iawb_Wwlwuu-_gIZWFkund24iA2qLktFx_39CORZqfFRNiIsHSvIQ";
    pub(super) const TEST_JWK_E: &str = "AQAB";

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
    async fn well_formed_ec_and_okp_entries_are_materialized_by_family() {
        use jsonwebtoken::AlgorithmFamily;

        use super::fixtures::{ec_key, okp_key, without};

        let jwks = serde_json::json!({
            "keys": [
                { "kid": "k1", "kty": "RSA", "n": TEST_JWK_N, "e": TEST_JWK_E },
                ec_key("ec"),
                okp_key("okp"),
                without(without(ec_key("ec-bare"), "alg"), "use"),
                without(without(okp_key("okp-bare"), "alg"), "use"),
            ]
        })
        .to_string();
        let (issuer, _fetches, server) = spawn_mock_idp(jwks).await;
        let resolver = HttpJwksResolver::new(issuer, None).expect("loopback issuer");

        for (kid, family) in [
            ("k1", AlgorithmFamily::Rsa),
            ("ec", AlgorithmFamily::Ec),
            ("okp", AlgorithmFamily::Ed),
            ("ec-bare", AlgorithmFamily::Ec),
            ("okp-bare", AlgorithmFamily::Ed),
        ] {
            let key = resolver.key_for(kid).await.expect("resolves");
            assert_eq!(key.family(), family, "{kid}");
        }
        server.abort();
    }

    /// One fault in an EC or OKP entry skips that entry and nothing else: the
    /// good keys beside it still resolve.
    #[tokio::test]
    async fn faulty_ec_and_okp_entries_are_skipped_and_the_rest_resolve() {
        use super::fixtures::{ec_key, faulty_ec_and_okp_entries, okp_key};

        let mut keys = vec![
            serde_json::json!({ "kid": "k1", "kty": "RSA", "n": TEST_JWK_N, "e": TEST_JWK_E }),
            ec_key("ec"),
            okp_key("okp"),
        ];
        let faulty = faulty_ec_and_okp_entries();
        assert!(!faulty.is_empty());
        keys.extend(faulty);
        let jwks = serde_json::json!({ "keys": keys }).to_string();
        let (issuer, fetches, server) = spawn_mock_idp(jwks).await;
        let resolver = HttpJwksResolver::new(issuer, None).expect("loopback issuer");

        for kid in ["k1", "ec", "okp"] {
            assert!(resolver.key_for(kid).await.is_ok(), "{kid}");
        }
        let mut cached = resolver
            .cache
            .read()
            .await
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        cached.sort();
        assert_eq!(cached, ["ec", "k1", "okp"], "only the well-formed keys");
        assert_eq!(fetches.load(Ordering::SeqCst), 1);
        server.abort();
    }

    #[tokio::test]
    async fn a_set_whose_only_ec_and_okp_entries_are_faulty_is_still_an_error() {
        use super::fixtures::faulty_ec_and_okp_entries;

        let jwks = serde_json::json!({ "keys": faulty_ec_and_okp_entries() }).to_string();
        let (issuer, fetches, server) = spawn_mock_idp(jwks).await;
        let resolver = HttpJwksResolver::new(issuer, None).expect("loopback issuer");

        assert!(matches!(
            resolver.key_for("ec-p384").await,
            Err(KeyError::Parse(_))
        ));
        assert!(resolver.cache.read().await.is_empty());
        assert_eq!(fetches.load(Ordering::SeqCst), 1);
        server.abort();
    }

    /// A wrong-typed member sinks only its own entry: the RSA key beside it
    /// still resolves, whichever member is mistyped.
    #[tokio::test]
    async fn a_mistyped_entry_does_not_fail_the_set() {
        use super::fixtures::mistyped_entries;

        let mut keys = vec![serde_json::json!({
            "kid": "k1", "kty": "RSA", "n": TEST_JWK_N, "e": TEST_JWK_E
        })];
        keys.extend(mistyped_entries());
        let jwks = serde_json::json!({ "keys": keys }).to_string();
        let (issuer, fetches, server) = spawn_mock_idp(jwks).await;
        let resolver = HttpJwksResolver::new(issuer, None).expect("loopback issuer");

        assert!(resolver.key_for("k1").await.is_ok());
        assert_eq!(resolver.cache.read().await.len(), 1, "only the RSA key");
        assert_eq!(fetches.load(Ordering::SeqCst), 1);
        server.abort();
    }

    /// An RSA entry is read from `kid`, `kty`, `n` and `e` alone: members
    /// the resolver never read, wrong-typed or not, do not sink it.
    #[tokio::test]
    async fn an_rsa_entry_with_mistyped_foreign_members_still_resolves() {
        let jwks = serde_json::json!({
            "keys": [
                { "kid": "k1", "kty": "RSA", "n": TEST_JWK_N, "e": TEST_JWK_E,
                  "alg": 7, "use": 7, "crv": [], "x": 1, "y": {}, "d": 7 },
                { "kid": "k2", "n": TEST_JWK_N, "e": TEST_JWK_E, "alg": 7, "x": 1 },
            ]
        })
        .to_string();
        let (issuer, _fetches, server) = spawn_mock_idp(jwks).await;
        let resolver = HttpJwksResolver::new(issuer, None).expect("loopback issuer");

        for kid in ["k1", "k2"] {
            assert!(resolver.key_for(kid).await.is_ok(), "{kid}");
        }
        server.abort();
    }

    /// The document itself must still be `{ "keys": [..] }`.
    #[tokio::test]
    async fn a_document_that_is_not_a_jwks_is_a_parse_error() {
        for body in ["[]", r#"{"keys":{}}"#, r#"{"keys":"k1"}"#, "{}", "not json"] {
            let (issuer, _fetches, server) = spawn_mock_idp(body.to_owned()).await;
            let resolver = HttpJwksResolver::new(issuer, None).expect("loopback issuer");

            assert!(
                matches!(resolver.key_for("k1").await, Err(KeyError::Parse(_))),
                "{body}"
            );
            server.abort();
        }
    }

    #[tokio::test]
    async fn jwks_entries_without_kid_do_not_break_named_rsa_resolution() {
        // Public example keys from RFC 7517 section 3 and RFC 8037 A.2.
        // RFC 7517 section 4.5 makes kid optional; these unsupported keys
        // must not prevent the named RSA signing key from being loaded.
        let unnamed_keys = [
            serde_json::json!({
                "kty": "EC", "crv": "P-256",
                "x": "f83OJ3D2xF1Bg8vub9tLe1gHMzV76e8Tus9uPHvRVEU",
                "y": "x_FEzRu9m36HLN_tue659LNpXW6pCyStikYjKIWI5a0"
            }),
            serde_json::json!({
                "kty": "OKP", "crv": "Ed25519",
                "x": "11qYAYKxCrfVS_7TyWQHOg7hcvPapiMlrwIaaPcHURo"
            }),
            serde_json::json!({ "kty": "RSA", "n": TEST_JWK_N, "e": TEST_JWK_E }),
        ];
        for unnamed in unnamed_keys {
            let jwks = serde_json::json!({
                "keys": [unnamed, {
                    "kid": "k1", "kty": "RSA", "n": TEST_JWK_N, "e": TEST_JWK_E
                }]
            })
            .to_string();
            let (issuer, fetches, server) = spawn_mock_idp(jwks).await;
            let resolver = HttpJwksResolver::new(issuer, None).expect("loopback issuer");
            let named = resolver.key_for("k1").await;
            let absent = resolver.key_for("").await;
            server.abort();

            assert!(named.is_ok(), "named RSA key must load: {:?}", named.err());
            assert!(matches!(absent, Err(KeyError::UnknownKid)));
            assert_eq!(fetches.load(Ordering::SeqCst), 1);
            assert_eq!(resolver.cache.read().await.len(), 1);
        }
    }

    #[tokio::test]
    async fn jwks_without_addressable_rsa_keys_is_rejected() {
        for keys in [
            serde_json::json!([]),
            serde_json::json!([{ "kty": "RSA", "n": TEST_JWK_N, "e": TEST_JWK_E }]),
        ] {
            let (issuer, fetches, server) =
                spawn_mock_idp(serde_json::json!({ "keys": keys }).to_string()).await;
            let resolver = HttpJwksResolver::new(issuer, None).expect("loopback issuer");
            let result = resolver.key_for("k1").await;
            let absent = resolver.key_for("").await;
            server.abort();

            assert!(matches!(result, Err(KeyError::Parse(_))));
            assert!(matches!(absent, Err(KeyError::UnknownKid)));
            assert!(resolver.cache.read().await.is_empty());
            assert_eq!(fetches.load(Ordering::SeqCst), 1);
        }
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

#[cfg(test)]
mod pinned_jwks_tests {
    use serde_json::{Value, json};

    use super::fixtures::{
        ec_key, faulty_ec_and_okp_entries, mistyped_entries, okp_key, with, without,
    };
    use super::http_tests::{TEST_JWK_E, TEST_JWK_N};
    use super::{KeyError, KeyResolver, StaticJwksResolver};

    fn rsa_key(kid: &str) -> Value {
        json!({ "kty": "RSA", "kid": kid, "alg": "RS256", "use": "sig", "n": TEST_JWK_N, "e": TEST_JWK_E })
    }

    fn parse(keys: &[Value]) -> Result<StaticJwksResolver, KeyError> {
        StaticJwksResolver::from_jwks_json(&json!({ "keys": keys }).to_string())
    }

    #[tokio::test]
    async fn a_pinned_set_resolves_each_key_by_kid_and_nothing_else() {
        let resolver = parse(&[rsa_key("k1"), without(without(rsa_key("k2"), "alg"), "use")])
            .expect("named RSA keys with and without alg/use");

        assert!(resolver.key_for("k1").await.is_ok());
        assert!(resolver.key_for("k2").await.is_ok());
        assert_eq!(
            resolver.key_for("k3").await.err(),
            Some(KeyError::UnknownKid)
        );
    }

    /// An entry the HTTP resolver would skip is refused here: a pinned key
    /// silently dropped is one the operator believes is trusted and is not.
    #[test]
    fn every_entry_must_be_a_named_public_verification_key() {
        let refused = [
            ("empty set", vec![]),
            ("no kid", vec![without(rsa_key("k1"), "kid")]),
            ("blank kid", vec![rsa_key(" ")]),
            (
                "EC key without crv",
                vec![with(rsa_key("k1"), "kty", json!("EC"))],
            ),
            ("oct key", vec![with(rsa_key("k1"), "kty", json!("oct"))]),
            ("no kty", vec![without(rsa_key("k1"), "kty")]),
            ("no n", vec![without(rsa_key("k1"), "n")]),
            ("no e", vec![without(rsa_key("k1"), "e")]),
            ("HS256", vec![with(rsa_key("k1"), "alg", json!("HS256"))]),
            ("none", vec![with(rsa_key("k1"), "alg", json!("none"))]),
            ("PS256", vec![with(rsa_key("k1"), "alg", json!("PS256"))]),
            (
                "EdDSA on RSA",
                vec![with(rsa_key("k1"), "alg", json!("EdDSA"))],
            ),
            (
                "ES256 on RSA",
                vec![with(rsa_key("k1"), "alg", json!("ES256"))],
            ),
            ("enc use", vec![with(rsa_key("k1"), "use", json!("enc"))]),
            ("private d", vec![with(rsa_key("k1"), "d", json!("AQAB"))]),
            ("duplicate kid", vec![rsa_key("k1"), rsa_key("k1")]),
            (
                "one bad key among good",
                vec![rsa_key("k1"), with(rsa_key("k2"), "kty", json!("OKP"))],
            ),
        ];
        for (case, keys) in refused {
            assert!(
                matches!(parse(&keys), Err(KeyError::Config(_))),
                "{case} must be a config error"
            );
        }
    }

    #[tokio::test]
    async fn p256_and_ed25519_keys_are_pinned_beside_rsa_keys() {
        use jsonwebtoken::AlgorithmFamily;

        let resolver = parse(&[
            rsa_key("rsa"),
            ec_key("ec"),
            okp_key("okp"),
            without(without(ec_key("ec-bare"), "alg"), "use"),
            without(without(okp_key("okp-bare"), "alg"), "use"),
        ])
        .expect("RSA, P-256 and Ed25519 keys");

        for (kid, family) in [
            ("rsa", AlgorithmFamily::Rsa),
            ("ec", AlgorithmFamily::Ec),
            ("okp", AlgorithmFamily::Ed),
            ("ec-bare", AlgorithmFamily::Ec),
            ("okp-bare", AlgorithmFamily::Ed),
        ] {
            let key = resolver.key_for(kid).await.expect("resolves");
            assert_eq!(key.family(), family, "{kid}");
        }
    }

    /// Every EC/OKP fault the HTTP resolver skips is a load error here, and a
    /// bad entry among good ones fails the whole set.
    #[test]
    fn a_faulty_ec_or_okp_entry_is_a_config_error() {
        let faulty = faulty_ec_and_okp_entries();
        assert!(!faulty.is_empty());
        for entry in faulty {
            let kid = entry["kid"].as_str().expect("kid").to_owned();
            assert!(
                matches!(
                    parse(std::slice::from_ref(&entry)),
                    Err(KeyError::Config(_))
                ),
                "{kid} must be a config error"
            );
            assert!(
                matches!(
                    parse(&[rsa_key("k1"), ec_key("ec"), entry]),
                    Err(KeyError::Config(_))
                ),
                "{kid} among good keys must fail the set"
            );
        }
    }

    /// An entry with a wrong-typed member is not a JWK: the pinned set is
    /// refused at load as a config error, alone or beside a good key.
    #[test]
    fn a_mistyped_entry_is_a_config_error() {
        let entries = mistyped_entries();
        assert!(!entries.is_empty());
        for entry in entries {
            assert!(
                matches!(
                    parse(std::slice::from_ref(&entry)),
                    Err(KeyError::Config(_))
                ),
                "{entry} must be a config error"
            );
            assert!(
                matches!(
                    parse(&[rsa_key("k1"), entry.clone()]),
                    Err(KeyError::Config(_))
                ),
                "{entry} beside a good key must fail the set"
            );
        }
    }

    #[test]
    fn a_duplicate_kid_across_key_types_is_refused() {
        assert!(matches!(
            parse(&[ec_key("k1"), okp_key("k1")]),
            Err(KeyError::Config(_))
        ));
    }

    #[test]
    fn a_document_that_is_not_a_jwks_is_a_parse_error() {
        for raw in ["", "not json", "{}", r#"{"keys":{}}"#, r#"[{"kid":"k1"}]"#] {
            assert!(
                matches!(
                    StaticJwksResolver::from_jwks_json(raw),
                    Err(KeyError::Parse(_))
                ),
                "{raw:?} must be a parse error"
            );
        }
        assert!(matches!(
            parse(&[with(rsa_key("k1"), "n", json!("!!not base64url!!"))]),
            Err(KeyError::Parse(_))
        ));
    }
}

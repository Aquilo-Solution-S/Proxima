//! JWKS key resolution.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use jsonwebtoken::DecodingKey;
use tokio::sync::RwLock;

use crate::config::{OidcConfigError, validate_https_url};

/// Minimum spacing between JWKS refetches. Bounds the outbound-fetch rate so a
/// flood of tokens carrying random unknown `kid`s cannot amplify into one
/// upstream JWKS request per inbound request.
const JWKS_REFRESH_COOLDOWN: Duration = Duration::from_mins(1);

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
    jwks_uri: String,
}

#[derive(serde::Deserialize)]
struct Jwk {
    kid: String,
    n: String,
    e: String,
}

#[derive(serde::Deserialize)]
struct JwkSet {
    keys: Vec<Jwk>,
}

/// Production resolver: discovers the JWKS endpoint and caches keys by kid,
/// refreshing once on an unknown kid (key rotation).
pub struct HttpJwksResolver {
    issuer: String,
    jwks_uri: Option<String>,
    http: reqwest::Client,
    cache: RwLock<HashMap<String, Arc<DecodingKey>>>,
    /// Last time a refetch was attempted; gates the unknown-kid refresh.
    last_refresh: RwLock<Option<Instant>>,
}

impl std::fmt::Debug for HttpJwksResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpJwksResolver")
            .field("issuer", &self.issuer)
            .finish_non_exhaustive()
    }
}

impl HttpJwksResolver {
    /// # Errors
    ///
    /// Returns an error when the issuer or explicit JWKS endpoint is not
    /// HTTPS. Test builds allow loopback HTTP for mock `IdPs`.
    pub fn new(issuer: String, jwks_uri: Option<String>) -> Result<Self, OidcConfigError> {
        validate_https_url("issuer", &issuer)?;
        if let Some(uri) = &jwks_uri {
            validate_https_url("jwks_uri", uri)?;
        }
        Ok(Self {
            issuer,
            jwks_uri,
            http: reqwest::Client::new(),
            cache: RwLock::new(HashMap::new()),
            last_refresh: RwLock::new(None),
        })
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
            .send()
            .await
            .map_err(|e| KeyError::Fetch(e.to_string()))?
            .error_for_status()
            .map_err(|e| KeyError::Fetch(e.to_string()))?
            .json()
            .await
            .map_err(|e| KeyError::Parse(e.to_string()))?;
        validate_https_url("discovered jwks_uri", &cfg.jwks_uri)
            .map_err(|err| KeyError::Config(err.to_string()))?;
        Ok(cfg.jwks_uri)
    }

    async fn refresh(&self) -> Result<(), KeyError> {
        let endpoint = self.jwks_endpoint().await?;
        let set: JwkSet = self
            .http
            .get(endpoint)
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
            let key = DecodingKey::from_rsa_components(&jwk.n, &jwk.e)
                .map_err(|e| KeyError::Parse(e.to_string()))?;
            next.insert(jwk.kid, Arc::new(key));
        }
        *self.cache.write().await = next;
        Ok(())
    }
}

#[async_trait]
impl KeyResolver for HttpJwksResolver {
    async fn key_for(&self, kid: &str) -> Result<Arc<DecodingKey>, KeyError> {
        if let Some(k) = self.cache.read().await.get(kid).cloned() {
            return Ok(k);
        }
        // Cache miss: refetch at most once per cooldown so a stream of
        // unknown-kid tokens can't drive one upstream JWKS fetch per request.
        if let Some(last) = *self.last_refresh.read().await
            && last.elapsed() < JWKS_REFRESH_COOLDOWN
        {
            return Err(KeyError::UnknownKid);
        }
        *self.last_refresh.write().await = Some(Instant::now());
        self.refresh().await?;
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
    use axum::{Router, routing::get};

    use crate::config::{OidcConfigError, validate_https_url};

    use super::{HttpJwksResolver, KeyError, KeyResolver};

    // Static 2048-bit RSA public key as JWK n/e (base64url). Baked so this
    // test needs neither `rsa` nor `rand` (RUSTSEC-2023-0071: the `rsa` crate
    // ships an unfixed Marvin timing sidechannel). The test only serves the
    // public JWK from a mock IdP and resolves it; nothing signs here.
    const TEST_JWK_N: &str = "vcvNMtDvpJExXOyytyqUOWhX2sxa-Xtxd4KmfJ05-iPgT_RiyZzx3UoTuJYtvDCCRcXKU13Rn8cIc0ushWlKpLDW08U4r9bBVctcajpnOumCcuIvnM1_HEiM-WuYPRFk0I5h--ueLA0KhIfPs0ORLpqsvF0XIuL6_uZtObrH9wxPMmG4r5Hh7h3Gm5PchY0R8H7VrEOm79fnra7OGg5nh7XkmStnZnwozODW0FFnpW-kMeCK2-2fzmSWg1A_clFdicji1-xIvk7Wog9CVsZZK9iRHgAIxmsU-Iawb_Wwlwuu-_gIZWFkund24iA2qLktFx_39CORZqfFRNiIsHSvIQ";
    const TEST_JWK_E: &str = "AQAB";

    #[tokio::test]
    async fn discovers_and_loads_jwks() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test listener");
        let addr = listener.local_addr().expect("read listener addr");
        let issuer = format!("http://{addr}");
        let jwks_uri = format!("{issuer}/keys");
        let openid = serde_json::json!({ "jwks_uri": jwks_uri }).to_string();
        let jwks = serde_json::json!({
            "keys": [{ "kid": "k1", "n": TEST_JWK_N, "e": TEST_JWK_E }]
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

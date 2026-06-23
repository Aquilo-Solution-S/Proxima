//! OIDC bearer-JWT authenticator implementing [`proxima_core::Authenticator`].

use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use async_trait::async_trait;
use jsonwebtoken::{Algorithm, Validation, decode, decode_header};
use proxima_core::{AuthError, AuthPath, Authenticator, AuthzContext, Credentials};
use serde::Deserialize;

use crate::config::OidcAuthConfig;
use crate::keys::KeyResolver;

#[derive(Debug, Deserialize)]
struct Claims {
    sub: String,
    exp: u64,
}

/// Validates Zitadel/OIDC bearer JWTs and maps them to the configured owner.
pub struct OidcAuthenticator {
    config: OidcAuthConfig,
    keys: Arc<dyn KeyResolver>,
}

impl std::fmt::Debug for OidcAuthenticator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OidcAuthenticator")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl OidcAuthenticator {
    #[must_use]
    pub fn new(config: OidcAuthConfig, keys: Arc<dyn KeyResolver>) -> Self {
        Self { config, keys }
    }
}

#[async_trait]
impl Authenticator for OidcAuthenticator {
    async fn authenticate(&self, creds: &Credentials) -> Result<AuthzContext, AuthError> {
        let Credentials::Bearer(token) = creds;
        let header = decode_header(token).map_err(|_| AuthError::InvalidCredentials)?;
        let kid = header.kid.ok_or(AuthError::InvalidCredentials)?;
        let key = self
            .keys
            .key_for(&kid)
            .await
            .map_err(|_| AuthError::InvalidCredentials)?;

        // Pin the verification algorithm to the RSA family (the only key type
        // the JWKS resolver materializes). Never derive it from the
        // attacker-controlled token header — that enables alg-confusion
        // (e.g. forging an HS256 token signed with the public RSA key, or
        // `alg: none`).
        let mut validation = Validation::new(Algorithm::RS256);
        validation.algorithms = vec![Algorithm::RS256, Algorithm::RS384, Algorithm::RS512];
        validation.set_issuer(&[&self.config.issuer]);
        validation.set_audience(&[&self.config.audience]);
        validation.set_required_spec_claims(&["exp", "aud", "iss", "sub"]);
        validation.leeway = self.config.leeway_secs;

        let data = decode::<Claims>(token, &key, &validation)
            .map_err(|_| AuthError::InvalidCredentials)?;

        if let Some(allow) = &self.config.allowed_subjects
            && !allow.contains(&data.claims.sub)
        {
            return Err(AuthError::InvalidCredentials);
        }

        tracing::debug!(sub = %data.claims.sub, "oidc token accepted");
        let mut ctx = AuthzContext::single_owner(&self.config.owner, AuthPath::HostBearer);
        ctx.identity.expires_at = Some(UNIX_EPOCH + Duration::from_secs(data.claims.exp));
        Ok(ctx)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, encode};
    use proxima_core::{Owner, Principal, UserId};
    use rand::rngs::OsRng;
    use rsa::{
        RsaPrivateKey,
        pkcs1::{EncodeRsaPrivateKey, EncodeRsaPublicKey, LineEnding},
    };
    use serde::Serialize;
    use uuid::Uuid;

    use super::*;
    use crate::keys::StaticJwksResolver;

    const ISSUER: &str = "https://issuer.example";
    const AUDIENCE: &str = "proxima-api";
    const KID: &str = "k1";

    struct TestKeys {
        encoding: EncodingKey,
        decoding: DecodingKey,
    }

    #[derive(Debug, Serialize)]
    struct TestClaims {
        sub: String,
        iss: String,
        aud: String,
        exp: u64,
    }

    fn test_owner() -> Owner {
        Principal::User(UserId::new(Uuid::new_v4()))
    }

    fn test_keys() -> TestKeys {
        let private_key = RsaPrivateKey::new(&mut OsRng, 2048).expect("generate rsa key");
        let private_pem = private_key
            .to_pkcs1_pem(LineEnding::LF)
            .expect("encode private key pem");
        let public_der = private_key
            .to_public_key()
            .to_pkcs1_der()
            .expect("encode public key der");
        TestKeys {
            encoding: EncodingKey::from_rsa_pem(private_pem.as_bytes())
                .expect("build encoding key"),
            decoding: DecodingKey::from_rsa_der(public_der.as_bytes()),
        }
    }

    fn token(
        keys: &TestKeys,
        kid: &str,
        issuer: &str,
        audience: &str,
        sub: &str,
        exp: u64,
    ) -> String {
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(kid.to_owned());
        let claims = TestClaims {
            sub: sub.to_owned(),
            iss: issuer.to_owned(),
            aud: audience.to_owned(),
            exp,
        };
        encode(&header, &claims, &keys.encoding).expect("encode jwt")
    }

    fn future_exp() -> u64 {
        jsonwebtoken::get_current_timestamp() + 3_600
    }

    fn config(owner: Owner) -> OidcAuthConfig {
        OidcAuthConfig {
            issuer: ISSUER.to_owned(),
            jwks_uri: None,
            audience: AUDIENCE.to_owned(),
            owner,
            allowed_subjects: None,
            leeway_secs: 0,
        }
    }

    fn authenticator(
        config: OidcAuthConfig,
        kid: &str,
        decoding: DecodingKey,
    ) -> OidcAuthenticator {
        OidcAuthenticator::new(
            config,
            Arc::new(StaticJwksResolver::new(HashMap::from([(
                kid.to_owned(),
                Arc::new(decoding),
            )]))),
        )
    }

    #[tokio::test]
    async fn valid_token_maps_to_single_owner_host_bearer() {
        let keys = test_keys();
        let owner = test_owner();
        let auth = authenticator(config(owner.clone()), KID, keys.decoding.clone());
        let token = token(&keys, KID, ISSUER, AUDIENCE, "subject-1", future_exp());

        let ctx = auth
            .authenticate(&Credentials::Bearer(token))
            .await
            .expect("authenticate valid token");

        assert_eq!(ctx.auth_path, AuthPath::HostBearer);
        assert_eq!(ctx.identity.principal, owner);
        assert!(ctx.identity.expires_at.is_some());
    }

    #[tokio::test]
    async fn rejects_wrong_audience() {
        let keys = test_keys();
        let auth = authenticator(config(test_owner()), KID, keys.decoding.clone());
        let token = token(
            &keys,
            KID,
            ISSUER,
            "wrong-audience",
            "subject-1",
            future_exp(),
        );

        assert_eq!(
            auth.authenticate(&Credentials::Bearer(token)).await,
            Err(AuthError::InvalidCredentials)
        );
    }

    #[tokio::test]
    async fn rejects_wrong_issuer() {
        let keys = test_keys();
        let auth = authenticator(config(test_owner()), KID, keys.decoding.clone());
        let token = token(
            &keys,
            KID,
            "https://wrong-issuer.example",
            AUDIENCE,
            "subject-1",
            future_exp(),
        );

        assert_eq!(
            auth.authenticate(&Credentials::Bearer(token)).await,
            Err(AuthError::InvalidCredentials)
        );
    }

    #[tokio::test]
    async fn rejects_expired_token() {
        let keys = test_keys();
        let auth = authenticator(config(test_owner()), KID, keys.decoding.clone());
        let token = token(
            &keys,
            KID,
            ISSUER,
            AUDIENCE,
            "subject-1",
            jsonwebtoken::get_current_timestamp() - 3_600,
        );

        assert_eq!(
            auth.authenticate(&Credentials::Bearer(token)).await,
            Err(AuthError::InvalidCredentials)
        );
    }

    #[tokio::test]
    async fn rejects_subject_not_in_allowlist() {
        let keys = test_keys();
        let owner = test_owner();
        let mut config = config(owner);
        config.allowed_subjects = Some(HashSet::from(["allowed-subject".to_owned()]));
        let auth = authenticator(config, KID, keys.decoding.clone());
        let token = token(&keys, KID, ISSUER, AUDIENCE, "subject-1", future_exp());

        assert_eq!(
            auth.authenticate(&Credentials::Bearer(token)).await,
            Err(AuthError::InvalidCredentials)
        );
    }

    #[tokio::test]
    async fn rejects_unknown_kid() {
        let keys = test_keys();
        let auth = OidcAuthenticator::new(
            config(test_owner()),
            Arc::new(StaticJwksResolver::new(HashMap::new())),
        );
        let token = token(&keys, KID, ISSUER, AUDIENCE, "subject-1", future_exp());

        assert_eq!(
            auth.authenticate(&Credentials::Bearer(token)).await,
            Err(AuthError::InvalidCredentials)
        );
    }
}

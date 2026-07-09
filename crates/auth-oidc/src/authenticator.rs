//! OIDC bearer-JWT validation and authentication.
//!
//! [`OidcTokenValidator`] is the audited, authz-free boundary: JWKS
//! signature verification (RSA-family pinned against alg-confusion),
//! `iss`/`aud`/`exp` checks. Custom hosts that need more than one identity
//! class (e.g. branching on `aud`) compose several validators sharing one
//! [`KeyResolver`] and shape their own [`AuthzContext`] from
//! [`ValidatedOidcClaims`] — see `tests/custom_host_validation.rs`.
//!
//! [`OidcAuthenticator`] is the default [`Authenticator`] built on top of
//! it: [`OidcAuthenticator::new`] maps the validated `(iss, sub)` through an
//! issuer-aware [`OidcSubjectMap`] to a [`UserId`] and resolves
//! [`proxima_core::OwnerRoles`] through an `OwnerAccessPort`, returning
//! [`AuthzContext::server_resolved`].

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use jsonwebtoken::{Algorithm, Validation, decode, decode_header};
use proxima_core::{
    AuthError, AuthPath, Authenticator, AuthzContext, Credentials, OwnerAccessPort,
};
use serde::Deserialize;

use crate::config::{OidcAuthConfig, OidcConfigError};
use crate::keys::KeyResolver;
use crate::subject_map::OidcSubjectMap;

#[derive(Debug, Deserialize)]
struct Claims {
    sub: String,
    exp: u64,
}

/// The `(iss, aud, sub, exp)` an [`OidcTokenValidator`] confirmed for a
/// bearer token. No authz shaping: the caller decides identity mapping and
/// capabilities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedOidcClaims {
    pub issuer: String,
    pub audience: String,
    pub subject: String,
    pub expires_at: SystemTime,
}

/// Audited OIDC bearer-JWT validation with no authz shaping.
pub struct OidcTokenValidator {
    issuer: String,
    audience: String,
    leeway_secs: u64,
    keys: Arc<dyn KeyResolver>,
}

impl std::fmt::Debug for OidcTokenValidator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OidcTokenValidator")
            .field("issuer", &self.issuer)
            .field("audience", &self.audience)
            .finish_non_exhaustive()
    }
}

impl OidcTokenValidator {
    /// # Errors
    ///
    /// Returns an error when the OIDC issuer or explicit JWKS endpoint is
    /// not HTTPS. Test builds allow loopback HTTP for mock `IdPs`.
    pub fn new(
        config: OidcAuthConfig,
        keys: Arc<dyn KeyResolver>,
    ) -> Result<Self, OidcConfigError> {
        config.validate()?;
        Ok(Self {
            issuer: config.issuer,
            audience: config.audience,
            leeway_secs: config.leeway_secs,
            keys,
        })
    }

    /// # Errors
    ///
    /// `AuthError::InvalidCredentials` for any malformed, unsigned,
    /// wrong-issuer, wrong-audience, or expired token, or an unresolvable
    /// key id.
    pub async fn validate(&self, token: &str) -> Result<ValidatedOidcClaims, AuthError> {
        let header = decode_header(token).map_err(|_| AuthError::InvalidCredentials)?;
        let kid = header.kid.ok_or(AuthError::InvalidCredentials)?;
        let key = self
            .keys
            .key_for(&kid)
            .await
            .map_err(|_| AuthError::InvalidCredentials)?;

        // Pin the verification algorithm to the RSA family (the only key
        // type the JWKS resolver materializes). Never derive it from the
        // attacker-controlled token header — that enables alg-confusion
        // (e.g. forging an HS256 token signed with the public RSA key, or
        // `alg: none`).
        let mut validation = Validation::new(Algorithm::RS256);
        validation.algorithms = vec![Algorithm::RS256, Algorithm::RS384, Algorithm::RS512];
        validation.set_issuer(&[&self.issuer]);
        validation.set_audience(&[&self.audience]);
        validation.set_required_spec_claims(&["exp", "aud", "iss", "sub"]);
        // Reject a token presented before its `nbf` (honoring `leeway`). `nbf`
        // stays out of required_spec_claims, so tokens that omit it are still
        // accepted; only a present-and-future `nbf` fails.
        validation.validate_nbf = true;
        validation.leeway = self.leeway_secs;

        let data = decode::<Claims>(token, &key, &validation)
            .map_err(|_| AuthError::InvalidCredentials)?;

        Ok(ValidatedOidcClaims {
            issuer: self.issuer.clone(),
            audience: self.audience.clone(),
            subject: data.claims.sub,
            expires_at: UNIX_EPOCH + Duration::from_secs(data.claims.exp),
        })
    }
}

/// Validates Zitadel/OIDC bearer JWTs and shapes an [`AuthzContext`].
pub struct OidcAuthenticator {
    validator: OidcTokenValidator,
    allowed_subjects: Option<HashSet<String>>,
    subject_map: OidcSubjectMap,
    owner_access: Arc<dyn OwnerAccessPort>,
}

impl std::fmt::Debug for OidcAuthenticator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OidcAuthenticator")
            .field("validator", &self.validator)
            .finish_non_exhaustive()
    }
}

impl OidcAuthenticator {
    /// Default host-resolved path: the validated `(iss, sub)` maps through
    /// `subject_map` to a `UserId`, whose `OwnerRoles` are resolved through
    /// `owner_access`. Returns
    /// `AuthzContext::server_resolved(owner_roles, AuthPath::HostBearer)`.
    ///
    /// # Errors
    ///
    /// Returns an error when the OIDC issuer or explicit JWKS endpoint is
    /// not HTTPS.
    pub fn new(
        mut config: OidcAuthConfig,
        keys: Arc<dyn KeyResolver>,
        subject_map: OidcSubjectMap,
        owner_access: Arc<dyn OwnerAccessPort>,
    ) -> Result<Self, OidcConfigError> {
        let allowed_subjects = std::mem::take(&mut config.allowed_subjects);
        let validator = OidcTokenValidator::new(config, keys)?;
        Ok(Self {
            validator,
            allowed_subjects,
            subject_map,
            owner_access,
        })
    }
}

#[async_trait]
impl Authenticator for OidcAuthenticator {
    async fn authenticate(&self, creds: &Credentials) -> Result<AuthzContext, AuthError> {
        let Credentials::Bearer(token) = creds;
        let claims = self.validator.validate(token).await?;

        if let Some(allow) = &self.allowed_subjects
            && !allow.contains(&claims.subject)
        {
            return Err(AuthError::InvalidCredentials);
        }

        let Some(subject) = self.subject_map.resolve(&claims.issuer, &claims.subject) else {
            tracing::debug!(
                sub = %claims.subject,
                iss = %claims.issuer,
                "oidc auth: token subject not in subject map"
            );
            return Err(AuthError::InvalidCredentials);
        };
        let roles = self
            .owner_access
            .resolve_roles_for_subject(subject)
            .await
            .map_err(|err| {
                tracing::warn!(error = %err, "oidc auth: owner-access resolution failed");
                AuthError::InvalidCredentials
            })?;
        tracing::debug!(sub = %claims.subject, "oidc token accepted (host-resolved)");
        Ok(AuthzContext::server_resolved(roles, AuthPath::HostBearer)
            .with_expires_at(Some(claims.expires_at)))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use async_trait::async_trait;
    use aws_lc_rs::rand::SystemRandom;
    use aws_lc_rs::rsa::KeySize;
    use aws_lc_rs::signature::{KeyPair as _, RSA_PKCS1_SHA256, RsaKeyPair};
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use jsonwebtoken::DecodingKey;
    use proxima_core::{AccessError, GroupId, Owner, OwnerRef, OwnerRoles, Role, UserId};
    use serde::Serialize;
    use uuid::Uuid;

    use super::*;
    use crate::keys::StaticJwksResolver;

    const ISSUER: &str = "https://issuer.example";
    const AUDIENCE: &str = "proxima-api";
    const KID: &str = "k1";

    struct TestKeys {
        signing: RsaKeyPair,
        decoding: DecodingKey,
    }

    #[derive(Debug, Serialize)]
    struct TestClaims {
        sub: String,
        iss: String,
        aud: String,
        exp: u64,
    }

    fn test_keys() -> TestKeys {
        let signing = RsaKeyPair::generate(KeySize::Rsa2048).expect("generate test RSA key");
        let decoding = DecodingKey::from_rsa_der(signing.public_key().as_ref());
        TestKeys { signing, decoding }
    }

    fn token(
        keys: &TestKeys,
        kid: &str,
        issuer: &str,
        audience: &str,
        sub: &str,
        exp: u64,
    ) -> String {
        let header = serde_json::json!({"alg": "RS256", "kid": kid, "typ": "JWT"});
        let claims = TestClaims {
            sub: sub.to_owned(),
            iss: issuer.to_owned(),
            aud: audience.to_owned(),
            exp,
        };
        let signing_input = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).expect("serialize header")),
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).expect("serialize claims"))
        );
        let mut signature = vec![0; keys.signing.public_modulus_len()];
        keys.signing
            .sign(
                &RSA_PKCS1_SHA256,
                &SystemRandom::new(),
                signing_input.as_bytes(),
                &mut signature,
            )
            .expect("sign jwt");
        format!("{}.{}", signing_input, URL_SAFE_NO_PAD.encode(signature))
    }

    fn future_exp() -> u64 {
        jsonwebtoken::get_current_timestamp() + 3_600
    }

    /// Signs a token whose `nbf` is in the future (with a valid `exp`) so it is
    /// not yet valid. Built directly (not via `token`) because [`TestClaims`]
    /// carries no `nbf` field.
    fn future_nbf_token(keys: &TestKeys) -> String {
        let now = jsonwebtoken::get_current_timestamp();
        let header = serde_json::json!({"alg": "RS256", "kid": KID, "typ": "JWT"});
        let claims = serde_json::json!({
            "sub": "subject-1",
            "iss": ISSUER,
            "aud": AUDIENCE,
            "exp": now + 3_600,
            "nbf": now + 1_800,
        });
        let signing_input = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).expect("serialize header")),
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).expect("serialize claims"))
        );
        let mut signature = vec![0; keys.signing.public_modulus_len()];
        keys.signing
            .sign(
                &RSA_PKCS1_SHA256,
                &SystemRandom::new(),
                signing_input.as_bytes(),
                &mut signature,
            )
            .expect("sign jwt");
        format!("{}.{}", signing_input, URL_SAFE_NO_PAD.encode(signature))
    }

    fn config() -> OidcAuthConfig {
        OidcAuthConfig {
            issuer: ISSUER.to_owned(),
            jwks_uri: None,
            audience: AUDIENCE.to_owned(),
            allowed_subjects: None,
            leeway_secs: 0,
        }
    }

    fn resolver(kid: &str, decoding: DecodingKey) -> Arc<dyn KeyResolver> {
        Arc::new(StaticJwksResolver::new(HashMap::from([(
            kid.to_owned(),
            Arc::new(decoding),
        )])))
    }

    fn mapped_authenticator(
        config: OidcAuthConfig,
        kid: &str,
        decoding: DecodingKey,
        token_subject: &str,
        subject: UserId,
        roles: Vec<(Owner, Role)>,
    ) -> OidcAuthenticator {
        let mut map = OidcSubjectMap::new();
        map.insert(ISSUER, token_subject, subject).expect("insert");
        let owner_access: Arc<dyn OwnerAccessPort> = Arc::new(StaticOwnerAccess { subject, roles });
        OidcAuthenticator::new(config, resolver(kid, decoding), map, owner_access)
            .expect("valid oidc config")
    }

    /// In-memory `OwnerAccessPort` stub for tests: returns fixed roles for
    /// one subject, `AccessError::Resolution` for anyone else.
    struct StaticOwnerAccess {
        subject: UserId,
        roles: Vec<(Owner, Role)>,
    }

    #[async_trait]
    impl OwnerAccessPort for StaticOwnerAccess {
        async fn resolve_roles_for_subject(
            &self,
            subject: UserId,
        ) -> Result<OwnerRoles, AccessError> {
            if subject != self.subject {
                return Err(AccessError::Resolution("unknown subject".into()));
            }
            OwnerRoles::for_subject(subject, self.roles.clone())
        }
    }

    #[tokio::test]
    async fn accepted_token_uses_issuer_subject_map_and_owner_roles() {
        let keys = test_keys();
        let subject = UserId::new(Uuid::now_v7());
        let group = GroupId::new(Uuid::now_v7());
        let mut map = OidcSubjectMap::new();
        map.insert(ISSUER, "subject-1", subject).expect("insert");
        let owner_access: Arc<dyn OwnerAccessPort> = Arc::new(StaticOwnerAccess {
            subject,
            roles: vec![(OwnerRef::Group(group), Role::admin())],
        });
        let auth = OidcAuthenticator::new(
            config(),
            resolver(KID, keys.decoding.clone()),
            map,
            owner_access,
        )
        .expect("valid oidc config");
        let token = token(&keys, KID, ISSUER, AUDIENCE, "subject-1", future_exp());

        let ctx = auth
            .authenticate(&Credentials::Bearer(token))
            .await
            .expect("authenticate valid token");

        assert_eq!(ctx.auth_path(), AuthPath::HostBearer);
        assert_eq!(ctx.subject(), Some(subject));
        assert!(ctx.may_manage(&OwnerRef::Group(group)));
        assert!(ctx.expires_at().is_some());
    }

    #[tokio::test]
    async fn valid_token_with_unknown_issuer_subject_is_denied() {
        let keys = test_keys();
        let subject = UserId::new(Uuid::now_v7());
        let map = OidcSubjectMap::new(); // deliberately empty
        let owner_access: Arc<dyn OwnerAccessPort> = Arc::new(StaticOwnerAccess {
            subject,
            roles: Vec::new(),
        });
        let auth = OidcAuthenticator::new(
            config(),
            resolver(KID, keys.decoding.clone()),
            map,
            owner_access,
        )
        .expect("valid oidc config");
        let token = token(
            &keys,
            KID,
            ISSUER,
            AUDIENCE,
            "unmapped-subject",
            future_exp(),
        );

        assert_eq!(
            auth.authenticate(&Credentials::Bearer(token)).await,
            Err(AuthError::InvalidCredentials)
        );
    }

    #[tokio::test]
    async fn validation_only_api_returns_claims_without_authz_context() {
        let keys = test_keys();
        let validator = OidcTokenValidator::new(config(), resolver(KID, keys.decoding.clone()))
            .expect("valid oidc config");
        let exp = future_exp();
        let token = token(&keys, KID, ISSUER, AUDIENCE, "subject-1", exp);

        let claims = validator.validate(&token).await.expect("validates");

        assert_eq!(claims.issuer, ISSUER);
        assert_eq!(claims.audience, AUDIENCE);
        assert_eq!(claims.subject, "subject-1");
        assert_eq!(claims.expires_at, UNIX_EPOCH + Duration::from_secs(exp));
    }

    #[tokio::test]
    async fn mapped_subject_authenticates_with_personal_owner_access() {
        let keys = test_keys();
        let subject = UserId::new(Uuid::now_v7());
        let owner = OwnerRef::Personal(subject);
        let auth = mapped_authenticator(
            config(),
            KID,
            keys.decoding.clone(),
            "subject-1",
            subject,
            Vec::new(),
        );
        let token = token(&keys, KID, ISSUER, AUDIENCE, "subject-1", future_exp());

        let ctx = auth
            .authenticate(&Credentials::Bearer(token))
            .await
            .expect("authenticate valid token");

        assert_eq!(ctx.auth_path(), AuthPath::HostBearer);
        assert_eq!(ctx.subject(), Some(subject));
        assert_eq!(ctx.principal(), owner);
        assert!(ctx.can_access_owner(&owner));
        assert!(ctx.expires_at().is_some());
    }

    #[tokio::test]
    async fn rejects_wrong_audience() {
        let keys = test_keys();
        let auth = mapped_authenticator(
            config(),
            KID,
            keys.decoding.clone(),
            "subject-1",
            UserId::new(Uuid::now_v7()),
            Vec::new(),
        );
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
        let auth = mapped_authenticator(
            config(),
            KID,
            keys.decoding.clone(),
            "subject-1",
            UserId::new(Uuid::now_v7()),
            Vec::new(),
        );
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
        let auth = mapped_authenticator(
            config(),
            KID,
            keys.decoding.clone(),
            "subject-1",
            UserId::new(Uuid::now_v7()),
            Vec::new(),
        );
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
    async fn rejects_token_before_nbf() {
        let keys = test_keys();
        let auth = mapped_authenticator(
            config(),
            KID,
            keys.decoding.clone(),
            "subject-1",
            UserId::new(Uuid::now_v7()),
            Vec::new(),
        );
        // Valid signature/iss/aud/exp, but `nbf` is 30 minutes in the future
        // and leeway is 0 — must be rejected as not-yet-valid.
        let token = future_nbf_token(&keys);

        assert_eq!(
            auth.authenticate(&Credentials::Bearer(token)).await,
            Err(AuthError::InvalidCredentials)
        );
    }

    #[tokio::test]
    async fn rejects_subject_not_in_allowlist() {
        let keys = test_keys();
        let mut config = config();
        config.allowed_subjects = Some(HashSet::from(["allowed-subject".to_owned()]));
        let auth = mapped_authenticator(
            config,
            KID,
            keys.decoding.clone(),
            "subject-1",
            UserId::new(Uuid::now_v7()),
            Vec::new(),
        );
        let token = token(&keys, KID, ISSUER, AUDIENCE, "subject-1", future_exp());

        assert_eq!(
            auth.authenticate(&Credentials::Bearer(token)).await,
            Err(AuthError::InvalidCredentials)
        );
    }

    #[tokio::test]
    async fn rejects_unknown_kid() {
        let keys = test_keys();
        let subject = UserId::new(Uuid::now_v7());
        let mut map = OidcSubjectMap::new();
        map.insert(ISSUER, "subject-1", subject).expect("insert");
        let owner_access: Arc<dyn OwnerAccessPort> = Arc::new(StaticOwnerAccess {
            subject,
            roles: Vec::new(),
        });
        let auth = OidcAuthenticator::new(
            config(),
            Arc::new(StaticJwksResolver::new(HashMap::new())),
            map,
            owner_access,
        );
        let auth = auth.expect("valid oidc config");
        let token = token(&keys, KID, ISSUER, AUDIENCE, "subject-1", future_exp());

        assert_eq!(
            auth.authenticate(&Credentials::Bearer(token)).await,
            Err(AuthError::InvalidCredentials)
        );
    }
}

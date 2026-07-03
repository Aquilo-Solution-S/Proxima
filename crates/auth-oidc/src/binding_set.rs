//! Multi-binding OIDC authenticator composition.
//!
//! Hosts with several OIDC audiences can register one binding per
//! `(issuer, audience)` route. Authentication succeeds only when exactly one
//! binding validates the bearer token.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use proxima_core::{
    AuthError, AuthPath, Authenticator, AuthzContext, Credentials, OwnerAccessPort, ToolScope,
};

use crate::{
    KeyResolver, OidcAuthConfig, OidcConfigError, OidcSubjectMap, OidcTokenValidator,
    ValidatedOidcClaims,
};

/// Static route owned by one OIDC binding.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OidcBindingRoute {
    pub issuer: String,
    pub audience: String,
}

/// Authz shaping applied after a binding validates `(iss, aud, sub)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OidcRoleShape {
    /// Resolve roles through `OwnerAccessPort` and return
    /// `AuthzContext::server_resolved(..., HostBearer)`.
    ServerResolved,
    /// Same as [`Self::ServerResolved`], then attach a tool palette/scope.
    ServerResolvedWithToolScope(ToolScope),
}

impl OidcRoleShape {
    fn apply(&self, ctx: AuthzContext) -> AuthzContext {
        match self {
            Self::ServerResolved => ctx,
            Self::ServerResolvedWithToolScope(scope) => ctx.with_tool_scope(scope.clone()),
        }
    }
}

/// One OIDC route: validator, identity map, owner-role resolver, authz shape.
pub struct OidcBinding {
    route: OidcBindingRoute,
    validator: OidcTokenValidator,
    allowed_subjects: Option<HashSet<String>>,
    subject_map: OidcSubjectMap,
    owner_access: Arc<dyn OwnerAccessPort>,
    role_shape: OidcRoleShape,
}

impl std::fmt::Debug for OidcBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OidcBinding")
            .field("route", &self.route)
            .field("role_shape", &self.role_shape)
            .finish_non_exhaustive()
    }
}

impl OidcBinding {
    /// Build a default host-resolved binding.
    ///
    /// # Errors
    ///
    /// Returns an OIDC config error when the issuer/JWKS URL is invalid.
    pub fn new(
        config: OidcAuthConfig,
        keys: Arc<dyn KeyResolver>,
        subject_map: OidcSubjectMap,
        owner_access: Arc<dyn OwnerAccessPort>,
    ) -> Result<Self, OidcConfigError> {
        Self::with_role_shape(
            config,
            keys,
            subject_map,
            owner_access,
            OidcRoleShape::ServerResolved,
        )
    }

    /// Build a binding with an explicit authz shape.
    ///
    /// # Errors
    ///
    /// Returns an OIDC config error when the issuer/JWKS URL is invalid.
    pub fn with_role_shape(
        mut config: OidcAuthConfig,
        keys: Arc<dyn KeyResolver>,
        subject_map: OidcSubjectMap,
        owner_access: Arc<dyn OwnerAccessPort>,
        role_shape: OidcRoleShape,
    ) -> Result<Self, OidcConfigError> {
        let route = OidcBindingRoute {
            issuer: config.issuer.clone(),
            audience: config.audience.clone(),
        };
        let allowed_subjects = std::mem::take(&mut config.allowed_subjects);
        let validator = OidcTokenValidator::new(config, keys)?;
        Ok(Self {
            route,
            validator,
            allowed_subjects,
            subject_map,
            owner_access,
            role_shape,
        })
    }

    #[must_use]
    pub fn route(&self) -> &OidcBindingRoute {
        &self.route
    }

    async fn authz_for_claims(
        &self,
        claims: ValidatedOidcClaims,
    ) -> Result<AuthzContext, AuthError> {
        if let Some(allow) = &self.allowed_subjects
            && !allow.contains(&claims.subject)
        {
            return Err(AuthError::InvalidCredentials);
        }

        let Some(subject) = self.subject_map.resolve(&claims.issuer, &claims.subject) else {
            tracing::debug!(
                sub = %claims.subject,
                iss = %claims.issuer,
                aud = %claims.audience,
                "oidc binding set: token subject not in subject map"
            );
            return Err(AuthError::InvalidCredentials);
        };
        let roles = self
            .owner_access
            .resolve_roles_for_subject(subject)
            .await
            .map_err(|err| {
                tracing::warn!(error = %err, "oidc binding set: owner-access resolution failed");
                AuthError::InvalidCredentials
            })?;
        let ctx = AuthzContext::server_resolved(roles, AuthPath::HostBearer)
            .with_expires_at(Some(claims.expires_at));
        Ok(self.role_shape.apply(ctx))
    }
}

/// Construction errors for [`OidcBindingSet`].
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum OidcBindingSetError {
    #[error("OIDC binding set must contain at least one binding")]
    Empty,
    #[error("duplicate OIDC binding route for issuer {issuer:?} audience {audience:?}")]
    DuplicateRoute { issuer: String, audience: String },
}

/// Multi-route host authenticator.
pub struct OidcBindingSet {
    bindings: Vec<OidcBinding>,
}

impl std::fmt::Debug for OidcBindingSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OidcBindingSet")
            .field("bindings", &self.bindings.len())
            .finish_non_exhaustive()
    }
}

impl OidcBindingSet {
    /// # Errors
    ///
    /// Returns [`OidcBindingSetError::Empty`] for no bindings and
    /// [`OidcBindingSetError::DuplicateRoute`] for duplicate
    /// `(issuer, audience)` routes.
    pub fn new(
        bindings: impl IntoIterator<Item = OidcBinding>,
    ) -> Result<Self, OidcBindingSetError> {
        let bindings = bindings.into_iter().collect::<Vec<_>>();
        if bindings.is_empty() {
            return Err(OidcBindingSetError::Empty);
        }
        let mut seen = HashSet::new();
        for binding in &bindings {
            if !seen.insert(binding.route.clone()) {
                return Err(OidcBindingSetError::DuplicateRoute {
                    issuer: binding.route.issuer.clone(),
                    audience: binding.route.audience.clone(),
                });
            }
        }
        Ok(Self { bindings })
    }

    #[must_use]
    pub fn bindings(&self) -> &[OidcBinding] {
        &self.bindings
    }
}

#[async_trait]
impl Authenticator for OidcBindingSet {
    async fn authenticate(&self, creds: &Credentials) -> Result<AuthzContext, AuthError> {
        let Credentials::Bearer(token) = creds;
        let mut matches = Vec::new();
        for binding in &self.bindings {
            if let Ok(claims) = binding.validator.validate(token).await {
                matches.push((binding, claims));
            }
        }

        match matches.len() {
            0 => Err(AuthError::InvalidCredentials),
            1 => {
                let (binding, claims) = matches.pop().expect("one match");
                binding.authz_for_claims(claims).await
            }
            _ => {
                tracing::warn!(
                    matches = matches.len(),
                    "oidc binding set: token matched multiple bindings"
                );
                Err(AuthError::InvalidCredentials)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use async_trait::async_trait;
    use aws_lc_rs::rand::SystemRandom;
    use aws_lc_rs::rsa::KeySize;
    use aws_lc_rs::signature::{KeyPair as _, RSA_PKCS1_SHA256, RsaKeyPair};
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use jsonwebtoken::DecodingKey;
    use proxima_core::{
        AccessError, AuthPath, OwnerRef, OwnerRoles, Role, UserId, access::AccessKind,
    };
    use serde::Serialize;
    use uuid::Uuid;

    use super::*;
    use crate::StaticJwksResolver;

    const ISSUER: &str = "https://issuer.example";
    const AGENT_AUD: &str = "centauri-agent";
    const OWNER_AUD: &str = "centauri-owner";
    const KID: &str = "binding-key";

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

    #[derive(Debug)]
    struct StaticOwnerAccess {
        agent: UserId,
        owner: UserId,
        agent_group: proxima_core::GroupId,
        owner_group: proxima_core::GroupId,
    }

    #[async_trait]
    impl OwnerAccessPort for StaticOwnerAccess {
        async fn resolve_roles_for_subject(
            &self,
            subject: UserId,
        ) -> Result<OwnerRoles, AccessError> {
            if subject == self.agent {
                return OwnerRoles::for_subject(
                    subject,
                    [(OwnerRef::Group(self.agent_group), Role::editor())],
                );
            }
            if subject == self.owner {
                return OwnerRoles::for_subject(
                    subject,
                    [(OwnerRef::Group(self.owner_group), Role::admin())],
                );
            }
            OwnerRoles::for_subject(subject, [])
        }
    }

    fn test_keys() -> TestKeys {
        let signing = RsaKeyPair::generate(KeySize::Rsa2048).expect("generate test RSA key");
        let decoding = DecodingKey::from_rsa_der(signing.public_key().as_ref());
        TestKeys { signing, decoding }
    }

    fn resolver(decoding: DecodingKey) -> Arc<dyn KeyResolver> {
        Arc::new(StaticJwksResolver::new(HashMap::from([(
            KID.to_string(),
            Arc::new(decoding),
        )])))
    }

    fn token(keys: &TestKeys, audience: &str, subject: &str) -> String {
        let header = serde_json::json!({"alg": "RS256", "kid": KID, "typ": "JWT"});
        let claims = TestClaims {
            sub: subject.to_owned(),
            iss: ISSUER.to_owned(),
            aud: audience.to_owned(),
            exp: jsonwebtoken::get_current_timestamp() + 3_600,
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

    fn config(audience: &str) -> OidcAuthConfig {
        OidcAuthConfig {
            issuer: ISSUER.to_string(),
            jwks_uri: None,
            audience: audience.to_string(),
            allowed_subjects: None,
            leeway_secs: 0,
        }
    }

    fn subject_map(subject: &str, user_id: UserId) -> OidcSubjectMap {
        let mut map = OidcSubjectMap::new();
        map.insert(ISSUER, subject, user_id).expect("subject map");
        map
    }

    fn binding_set(keys: &TestKeys) -> (OidcBindingSet, proxima_core::GroupId) {
        let agent = UserId::new(Uuid::from_u128(0xA9E1));
        let owner = UserId::new(Uuid::from_u128(0x0E1E));
        let agent_group = proxima_core::GroupId::new(Uuid::now_v7());
        let owner_group = proxima_core::GroupId::new(Uuid::now_v7());
        let owner_access: Arc<dyn OwnerAccessPort> = Arc::new(StaticOwnerAccess {
            agent,
            owner,
            agent_group,
            owner_group,
        });
        let agent_binding = OidcBinding::with_role_shape(
            config(AGENT_AUD),
            resolver(keys.decoding.clone()),
            subject_map("agent-sub", agent),
            owner_access.clone(),
            OidcRoleShape::ServerResolvedWithToolScope(ToolScope::Palette(vec![
                "core_goal:set".to_string(),
            ])),
        )
        .expect("agent binding");
        let owner_binding = OidcBinding::new(
            config(OWNER_AUD),
            resolver(keys.decoding.clone()),
            subject_map("owner-sub", owner),
            owner_access,
        )
        .expect("owner binding");
        (
            OidcBindingSet::new([agent_binding, owner_binding]).expect("binding set"),
            agent_group,
        )
    }

    #[tokio::test]
    async fn two_binding_routing_uses_matching_audience() {
        let keys = test_keys();
        let (bindings, agent_group) = binding_set(&keys);
        let token = token(&keys, AGENT_AUD, "agent-sub");

        let ctx = bindings
            .authenticate(&Credentials::Bearer(token))
            .await
            .expect("agent binding authenticates");

        assert_eq!(ctx.auth_path(), AuthPath::HostBearer);
        assert!(ctx.may_write(&OwnerRef::Group(agent_group), AccessKind::Perspective));
        assert!(ctx.tool_scope().allows_action("core_goal", "set"));
        assert!(!ctx.tool_scope().allows("core_membership"));
        assert!(
            !ctx.tool_scope()
                .allows_action("core_publish", "publish_to_world")
        );
    }

    #[tokio::test]
    async fn unknown_audience_is_denied() {
        let keys = test_keys();
        let (bindings, _) = binding_set(&keys);
        let token = token(&keys, "unknown-api", "agent-sub");

        let err = bindings
            .authenticate(&Credentials::Bearer(token))
            .await
            .expect_err("unknown audience rejected");

        assert_eq!(err, AuthError::InvalidCredentials);
    }

    #[test]
    fn duplicate_route_is_boot_error() {
        let keys = test_keys();
        let owner_access: Arc<dyn OwnerAccessPort> = Arc::new(StaticOwnerAccess {
            agent: UserId::new(Uuid::from_u128(0xA9E1)),
            owner: UserId::new(Uuid::from_u128(0x0E1E)),
            agent_group: proxima_core::GroupId::new(Uuid::now_v7()),
            owner_group: proxima_core::GroupId::new(Uuid::now_v7()),
        });
        let user = UserId::new(Uuid::now_v7());
        let first = OidcBinding::new(
            config(AGENT_AUD),
            resolver(keys.decoding.clone()),
            subject_map("subject-a", user),
            owner_access.clone(),
        )
        .expect("first binding");
        let second = OidcBinding::new(
            config(AGENT_AUD),
            resolver(keys.decoding),
            subject_map("subject-b", user),
            owner_access,
        )
        .expect("second binding");

        let err = OidcBindingSet::new([first, second]).expect_err("duplicate route rejected");

        assert_eq!(
            err,
            OidcBindingSetError::DuplicateRoute {
                issuer: ISSUER.to_string(),
                audience: AGENT_AUD.to_string(),
            }
        );
    }
}

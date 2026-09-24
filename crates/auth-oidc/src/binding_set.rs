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
    ValidatedOidcToken,
};

/// Every verified claim of a token, as [`OidcRoleShaper`] sees them.
pub type OidcClaimMap = serde_json::Map<String, serde_json::Value>;

/// Host authz shaping for one binding: runs after the binding validated the
/// token and resolved the subject's owner roles.
///
/// It receives the server-resolved context and every verified claim, and
/// returns the context to use. It may narrow (a tool palette, a default
/// owner, publication extensions, fewer owners or rights) or refuse. A
/// returned context with another subject, auth path or trusted model id, or
/// any owner right the resolved roles lack, refuses the token; its tool scope
/// is intersected with the given one and its expiry clamped to the token's.
pub trait OidcRoleShaper: Send + Sync + std::fmt::Debug {
    /// # Errors
    ///
    /// An [`AuthError`] refuses the token; the binding set fails closed.
    fn shape(
        &self,
        context: AuthzContext,
        token: &ValidatedOidcToken<OidcClaimMap>,
    ) -> Result<AuthzContext, AuthError>;
}

/// Static route owned by one OIDC binding.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OidcBindingRoute {
    pub issuer: String,
    pub audience: String,
}

/// Authz shaping applied after a binding validates `(iss, aud, sub)`.
#[derive(Debug, Clone)]
pub enum OidcRoleShape {
    /// Resolve roles through `OwnerAccessPort` and return
    /// `AuthzContext::server_resolved(..., HostBearer)`.
    ServerResolved,
    /// Same as [`Self::ServerResolved`], then attach a tool palette/scope.
    ServerResolvedWithToolScope(ToolScope),
    /// Same as [`Self::ServerResolved`], then the host's [`OidcRoleShaper`]
    /// with every verified claim.
    Host(Arc<dyn OidcRoleShaper>),
}

/// `Host` shapes are equal when they are the same shaper instance.
impl PartialEq for OidcRoleShape {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::ServerResolved, Self::ServerResolved) => true,
            (Self::ServerResolvedWithToolScope(a), Self::ServerResolvedWithToolScope(b)) => a == b,
            (Self::Host(a), Self::Host(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}

impl Eq for OidcRoleShape {}

impl OidcRoleShape {
    fn apply(
        &self,
        ctx: AuthzContext,
        token: &ValidatedOidcToken<OidcClaimMap>,
    ) -> Result<AuthzContext, AuthError> {
        match self {
            Self::ServerResolved => Ok(ctx),
            Self::ServerResolvedWithToolScope(scope) => Ok(ctx.with_tool_scope(scope.clone())),
            Self::Host(shaper) => {
                let host_context = shaper.shape(ctx.clone(), token)?;
                narrowed_by_host(&ctx, host_context, token.claims.expires_at)
            }
        }
    }
}

/// Accept a host shaper's context only when it narrows the one it was given:
/// the same `HostBearer` subject and trusted model id, no owner, kind or
/// manage right the resolved roles lack. The tool scope is intersected with
/// the given one and the expiry clamped to the token's, so neither can
/// widen either.
fn narrowed_by_host(
    resolved: &AuthzContext,
    shaped: AuthzContext,
    token_expires_at: std::time::SystemTime,
) -> Result<AuthzContext, AuthError> {
    let rights_widened = proxima_core::AccessKind::ALL.into_iter().any(|kind| {
        shaped.readable_owners(kind).iter().any(|owner| {
            !resolved.may_read(owner, kind)
                || (shaped.may_manage(owner) && !resolved.may_manage(owner))
        }) || shaped
            .writable_owners(kind)
            .iter()
            .any(|owner| !resolved.may_write(owner, kind))
    });
    if shaped.auth_path() != AuthPath::HostBearer
        || shaped.subject() != resolved.subject()
        || shaped.trusted_model_id() != resolved.trusted_model_id()
        || rights_widened
    {
        tracing::warn!("oidc binding set: host role shape widened the resolved context; refused");
        return Err(AuthError::InvalidCredentials);
    }
    let expires_at = shaped
        .expires_at()
        .map_or(token_expires_at, |at| at.min(token_expires_at));
    let tool_scope = shaped.tool_scope().intersect(resolved.tool_scope());
    Ok(shaped
        .with_expires_at(Some(expires_at))
        .with_tool_scope(tool_scope))
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

    async fn authz_for_token(
        &self,
        token: ValidatedOidcToken<OidcClaimMap>,
    ) -> Result<AuthzContext, AuthError> {
        let claims = &token.claims;
        if let Some(allow) = &self.allowed_subjects
            && !allow.contains(&claims.subject)
        {
            return Err(AuthError::InvalidCredentials);
        }

        let Some(binding) = self
            .subject_map
            .resolve_binding(&claims.issuer, &claims.subject)
        else {
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
            .resolve_roles_for_subject(binding.user_id)
            .await
            .map_err(|err| {
                tracing::warn!(error = %err, "oidc binding set: owner-access resolution failed");
                AuthError::InvalidCredentials
            })?;
        let ctx = crate::authenticator::bind_trusted_model_id(
            AuthzContext::server_resolved(roles, AuthPath::HostBearer)
                .with_expires_at(Some(claims.expires_at)),
            binding.trusted_model_id,
        )?;
        self.role_shape.apply(ctx, &token)
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
            match binding.validator.validate_with::<OidcClaimMap>(token).await {
                Ok(validated) => matches.push((binding, validated)),
                Err(reason) => tracing::debug!(
                    iss = %binding.route.issuer,
                    aud = %binding.route.audience,
                    %reason,
                    "oidc binding set: binding refused the token"
                ),
            }
        }

        match matches.len() {
            0 => Err(AuthError::InvalidCredentials),
            1 => {
                let (binding, validated) = matches.pop().expect("one match");
                binding.authz_for_token(validated).await
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
        signed(
            keys,
            &TestClaims {
                sub: subject.to_owned(),
                iss: ISSUER.to_owned(),
                aud: audience.to_owned(),
                exp: jsonwebtoken::get_current_timestamp() + 3_600,
            },
        )
    }

    fn signed(keys: &TestKeys, claims: &impl Serialize) -> String {
        let header = serde_json::json!({"alg": "RS256", "kid": KID, "typ": "JWT"});
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

    /// Trusted model provenance is per-binding, resolved from that
    /// binding's own subject map — the agent audience certifies a runner,
    /// the owner audience certifies a person.
    #[tokio::test]
    async fn a_binding_attaches_its_own_trusted_model_id() {
        let keys = test_keys();
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
        let mut agent_map = OidcSubjectMap::new();
        agent_map
            .insert_binding(
                ISSUER,
                "agent-sub",
                crate::SubjectBinding::new(agent).with_trusted_model_id("acme/runner-v3"),
            )
            .expect("subject map");
        let bindings = OidcBindingSet::new([
            OidcBinding::new(
                config(AGENT_AUD),
                resolver(keys.decoding.clone()),
                agent_map,
                owner_access.clone(),
            )
            .expect("agent binding"),
            OidcBinding::new(
                config(OWNER_AUD),
                resolver(keys.decoding.clone()),
                subject_map("owner-sub", owner),
                owner_access,
            )
            .expect("owner binding"),
        ])
        .expect("binding set");

        let agent_ctx = bindings
            .authenticate(&Credentials::Bearer(token(&keys, AGENT_AUD, "agent-sub")))
            .await
            .expect("agent binding authenticates");
        assert_eq!(agent_ctx.trusted_model_id(), Some("acme/runner-v3"));

        let owner_ctx = bindings
            .authenticate(&Credentials::Bearer(token(&keys, OWNER_AUD, "owner-sub")))
            .await
            .expect("owner binding authenticates");
        assert_eq!(
            owner_ctx.trusted_model_id(),
            None,
            "a binding that declares no runner certifies none"
        );
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
                .allows_action("core_transfer", "transfer_to_owner")
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

    #[derive(Debug, serde::Deserialize, PartialEq, Eq)]
    struct TenantClaims {
        sub: String,
        tenant: String,
        #[serde(default)]
        groups: Vec<String>,
    }

    fn claims_with(audience: &str, exp: u64, extra: serde_json::Value) -> serde_json::Value {
        let mut claims = serde_json::json!({
            "sub": "owner-sub",
            "iss": ISSUER,
            "aud": audience,
            "exp": exp,
        });
        let object = claims.as_object_mut().expect("claims object");
        let serde_json::Value::Object(extra) = extra else {
            panic!("extra claims must be an object");
        };
        object.extend(extra);
        claims
    }

    /// A host reads its own claims from the verified payload, and a refusal
    /// keeps its reason instead of collapsing to `InvalidCredentials`.
    #[tokio::test]
    async fn validate_with_reads_custom_claims_and_names_the_refusal() {
        let keys = test_keys();
        let validator = OidcTokenValidator::new(config(OWNER_AUD), resolver(keys.decoding.clone()))
            .expect("validator");
        let later = jsonwebtoken::get_current_timestamp() + 3_600;
        let good = signed(
            &keys,
            &claims_with(
                OWNER_AUD,
                later,
                serde_json::json!({"tenant": "acme", "groups": ["ops"]}),
            ),
        );
        let validated = validator
            .validate_with::<TenantClaims>(&good)
            .await
            .expect("valid token");
        assert_eq!(validated.claims.subject, "owner-sub");
        assert_eq!(
            validated.custom,
            TenantClaims {
                sub: "owner-sub".into(),
                tenant: "acme".into(),
                groups: vec!["ops".into()],
            }
        );

        let expired = signed(
            &keys,
            &claims_with(OWNER_AUD, 1_000, serde_json::json!({"tenant": "acme"})),
        );
        assert_eq!(
            validator.validate_with::<TenantClaims>(&expired).await,
            Err(crate::OidcRejection::Expired)
        );
        let other_audience = signed(
            &keys,
            &claims_with("other", later, serde_json::json!({"tenant": "acme"})),
        );
        assert_eq!(
            validator
                .validate_with::<TenantClaims>(&other_audience)
                .await,
            Err(crate::OidcRejection::WrongAudience {
                expected: OWNER_AUD.into()
            })
        );
        let no_tenant = signed(&keys, &claims_with(OWNER_AUD, later, serde_json::json!({})));
        assert!(matches!(
            validator.validate_with::<TenantClaims>(&no_tenant).await,
            Err(crate::OidcRejection::CustomClaims(_))
        ));
        // The plain path accepts what it accepted before: custom claims are
        // the host's business, not a validity condition.
        assert!(validator.validate(&no_tenant).await.is_ok());
        assert_eq!(
            validator.validate(&expired).await,
            Err(AuthError::InvalidCredentials)
        );
    }

    /// Narrows the palette to the token's `tenant`; refuses a token without.
    #[derive(Debug)]
    struct TenantShaper;

    impl OidcRoleShaper for TenantShaper {
        fn shape(
            &self,
            context: AuthzContext,
            token: &ValidatedOidcToken<OidcClaimMap>,
        ) -> Result<AuthzContext, AuthError> {
            let tenant = token
                .custom
                .get("tenant")
                .and_then(serde_json::Value::as_str)
                .ok_or(AuthError::InvalidCredentials)?;
            Ok(context.with_tool_scope(ToolScope::Palette(vec![format!("{tenant}_tool")])))
        }
    }

    #[tokio::test]
    async fn a_host_role_shape_sees_every_verified_claim() {
        let keys = test_keys();
        let owner = UserId::new(Uuid::from_u128(0x0E1E));
        let owner_access: Arc<dyn OwnerAccessPort> = Arc::new(StaticOwnerAccess {
            agent: UserId::new(Uuid::from_u128(0xA9E1)),
            owner,
            agent_group: proxima_core::GroupId::new(Uuid::now_v7()),
            owner_group: proxima_core::GroupId::new(Uuid::now_v7()),
        });
        let shaper: Arc<dyn OidcRoleShaper> = Arc::new(TenantShaper);
        let bindings = OidcBindingSet::new([OidcBinding::with_role_shape(
            config(OWNER_AUD),
            resolver(keys.decoding.clone()),
            subject_map("owner-sub", owner),
            owner_access,
            OidcRoleShape::Host(shaper.clone()),
        )
        .expect("binding")])
        .expect("binding set");
        assert_eq!(
            OidcRoleShape::Host(shaper.clone()),
            OidcRoleShape::Host(shaper)
        );

        let later = jsonwebtoken::get_current_timestamp() + 3_600;
        let ctx = bindings
            .authenticate(&Credentials::Bearer(signed(
                &keys,
                &claims_with(OWNER_AUD, later, serde_json::json!({"tenant": "acme"})),
            )))
            .await
            .expect("the shaper admits a tenant token");
        assert_eq!(ctx.subject(), Some(owner));
        assert!(ctx.tool_scope().allows("acme_tool"));
        assert!(!ctx.tool_scope().allows("core_membership"));

        let refused = bindings
            .authenticate(&Credentials::Bearer(signed(
                &keys,
                &claims_with(OWNER_AUD, later, serde_json::json!({})),
            )))
            .await;
        assert_eq!(refused.err(), Some(AuthError::InvalidCredentials));
    }

    /// What a shaper may do to the context it is handed.
    #[derive(Debug)]
    enum Reshape {
        /// Grant admin on a Group the resolved roles do not carry.
        Widen,
        /// Swap in another subject.
        OtherSubject,
        /// Drop the expiry; the binding clamps it back to the token's.
        DropExpiry,
    }

    impl OidcRoleShaper for Reshape {
        fn shape(
            &self,
            context: AuthzContext,
            _token: &ValidatedOidcToken<OidcClaimMap>,
        ) -> Result<AuthzContext, AuthError> {
            let subject = context.subject().expect("resolved subject");
            Ok(match self {
                Self::Widen => AuthzContext::for_subject_with_role(
                    subject,
                    [(
                        OwnerRef::Group(proxima_core::GroupId::new(Uuid::now_v7())),
                        Role::admin(),
                    )],
                    AuthPath::HostBearer,
                ),
                Self::OtherSubject => {
                    AuthzContext::for_subject(UserId::new(Uuid::now_v7()), AuthPath::HostBearer)
                }
                Self::DropExpiry => context.with_expires_at(None),
            })
        }
    }

    #[tokio::test]
    async fn a_host_role_shape_narrows_or_refuses() {
        let keys = test_keys();
        let owner = UserId::new(Uuid::from_u128(0x0E1E));
        let later = jsonwebtoken::get_current_timestamp() + 3_600;
        let authenticate = |reshape: Reshape| {
            let owner_access: Arc<dyn OwnerAccessPort> = Arc::new(StaticOwnerAccess {
                agent: UserId::new(Uuid::from_u128(0xA9E1)),
                owner,
                agent_group: proxima_core::GroupId::new(Uuid::now_v7()),
                owner_group: proxima_core::GroupId::new(Uuid::now_v7()),
            });
            let bindings = OidcBindingSet::new([OidcBinding::with_role_shape(
                config(OWNER_AUD),
                resolver(keys.decoding.clone()),
                subject_map("owner-sub", owner),
                owner_access,
                OidcRoleShape::Host(Arc::new(reshape)),
            )
            .expect("binding")])
            .expect("binding set");
            let token = signed(&keys, &claims_with(OWNER_AUD, later, serde_json::json!({})));
            async move { bindings.authenticate(&Credentials::Bearer(token)).await }
        };

        assert_eq!(
            authenticate(Reshape::Widen).await.err(),
            Some(AuthError::InvalidCredentials),
            "a Group the resolved roles lack"
        );
        assert_eq!(
            authenticate(Reshape::OtherSubject).await.err(),
            Some(AuthError::InvalidCredentials),
            "another subject"
        );
        let clamped = authenticate(Reshape::DropExpiry)
            .await
            .expect("dropping the expiry narrows nothing");
        assert_eq!(
            clamped.expires_at(),
            Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(later)),
            "the stream cannot outlive the token"
        );
    }
}

//! Custom-host compile/example coverage for the validation-only API.
//!
//! Proves a Centauri-style host can validate two Zitadel audiences, branch
//! on which one accepted the token, resolve roles itself, and construct
//! `AuthzContext::server_resolved(...).with_tool_scope(...)` — all built on
//! `OidcTokenValidator`/`ValidatedOidcClaims`, with no
//! `OidcAuthConfig { owner, .. }` anywhere in this file (that field no
//! longer exists on `OidcAuthConfig`; this file compiling is part of the
//! proof).

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use aws_lc_rs::rand::SystemRandom;
use aws_lc_rs::rsa::KeySize;
use aws_lc_rs::signature::{KeyPair as _, RSA_PKCS1_SHA256, RsaKeyPair};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::DecodingKey;
use proxima_auth_oidc::{OidcAuthConfig, OidcTokenValidator, StaticJwksResolver};
use proxima_core::{
    AccessError, AuthError, AuthPath, AuthzContext, GroupId, Owner, OwnerAccessPort, OwnerRef,
    OwnerRoles, Role, ToolScope, UserId,
};
use serde::Serialize;
use uuid::Uuid;

const ISSUER: &str = "https://issuer.example";
const AGENT_AUD: &str = "centauri-agent";
const OWNER_AUD: &str = "centauri-owner";
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

fn token(keys: &TestKeys, issuer: &str, audience: &str, sub: &str, exp: u64) -> String {
    let header = serde_json::json!({"alg": "RS256", "kid": KID, "typ": "JWT"});
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

/// Static in-memory `OwnerAccessPort`: the custom host's own role
/// resolution, unrelated to the Postgres adapter under
/// `proxima_storage_pg`.
struct StaticOwnerAccess {
    group: GroupId,
}

#[async_trait]
impl OwnerAccessPort for StaticOwnerAccess {
    async fn resolve_roles_for_subject(&self, subject: UserId) -> Result<OwnerRoles, AccessError> {
        OwnerRoles::for_subject(subject, [(OwnerRef::Group(self.group), Role::editor())])
    }
}

/// A minimal Centauri-style host authenticator: two `OidcTokenValidator`s
/// sharing one JWKS resolver, branching on which one accepts the token,
/// resolving roles through its own `OwnerAccessPort`, and narrowing the
/// agent's tool scope. Built entirely on the validation-only API — no
/// `OidcAuthConfig { owner, .. }` anywhere.
struct TwoAudienceHost {
    agent_validator: OidcTokenValidator,
    owner_validator: OidcTokenValidator,
    agent_principal: Owner,
    owner_principal: Owner,
    owner_access: Arc<dyn OwnerAccessPort>,
}

impl TwoAudienceHost {
    fn new(
        keys: Arc<dyn proxima_auth_oidc::KeyResolver>,
        owner_access: Arc<dyn OwnerAccessPort>,
    ) -> Self {
        let agent_config = OidcAuthConfig {
            issuer: ISSUER.to_owned(),
            jwks_uri: None,
            audience: AGENT_AUD.to_owned(),
            allowed_subjects: None,
            leeway_secs: 0,
        };
        let owner_config = OidcAuthConfig {
            issuer: ISSUER.to_owned(),
            jwks_uri: None,
            audience: OWNER_AUD.to_owned(),
            allowed_subjects: None,
            leeway_secs: 0,
        };
        Self {
            agent_validator: OidcTokenValidator::new(agent_config, keys.clone())
                .expect("valid agent oidc config"),
            owner_validator: OidcTokenValidator::new(owner_config, keys)
                .expect("valid owner oidc config"),
            agent_principal: OwnerRef::Personal(UserId::new(Uuid::from_u128(0xA9E1))),
            owner_principal: OwnerRef::Personal(UserId::new(Uuid::from_u128(0x0E1E))),
            owner_access,
        }
    }

    async fn authenticate(&self, token: &str) -> Result<AuthzContext, AuthError> {
        if let Ok(claims) = self.agent_validator.validate(token).await {
            let OwnerRef::Personal(subject) = self.agent_principal else {
                unreachable!("agent principal is always Personal");
            };
            let roles = self
                .owner_access
                .resolve_roles_for_subject(subject)
                .await
                .map_err(|_| AuthError::InvalidCredentials)?;
            return Ok(AuthzContext::server_resolved(roles, AuthPath::HostBearer)
                .with_expires_at(Some(claims.expires_at))
                .with_tool_scope(ToolScope::Palette(vec!["core_goal:set".to_string()])));
        }
        if let Ok(claims) = self.owner_validator.validate(token).await {
            let OwnerRef::Personal(subject) = self.owner_principal else {
                unreachable!("owner principal is always Personal");
            };
            return Ok(AuthzContext::for_subject(subject, AuthPath::HostBearer)
                .with_expires_at(Some(claims.expires_at))
                .with_tool_scope(ToolScope::All));
        }
        Err(AuthError::InvalidCredentials)
    }
}

fn resolver(decoding: DecodingKey) -> Arc<dyn proxima_auth_oidc::KeyResolver> {
    Arc::new(StaticJwksResolver::new(HashMap::from([(
        KID.to_owned(),
        Arc::new(decoding),
    )])))
}

#[tokio::test]
async fn agent_audience_resolves_narrowed_tool_scope() {
    let keys = test_keys();
    let group = GroupId::new(Uuid::now_v7());
    let host = TwoAudienceHost::new(
        resolver(keys.decoding.clone()),
        Arc::new(StaticOwnerAccess { group }),
    );
    let token = token(&keys, ISSUER, AGENT_AUD, "agent-service-user", future_exp());

    let ctx = host
        .authenticate(&token)
        .await
        .expect("agent authenticates");

    assert_eq!(ctx.auth_path(), AuthPath::HostBearer);
    assert!(ctx.may_write(
        &OwnerRef::Group(group),
        proxima_core::AccessKind::Perspective
    ));
    assert!(ctx.tool_scope().allows_action("core_goal", "set"));
    assert!(!ctx.tool_scope().allows("core_membership"));
    assert!(
        !ctx.tool_scope()
            .allows_action("core_transfer", "transfer_to_owner")
    );
}

#[tokio::test]
async fn owner_audience_resolves_full_tool_scope() {
    let keys = test_keys();
    let group = GroupId::new(Uuid::now_v7());
    let host = TwoAudienceHost::new(
        resolver(keys.decoding.clone()),
        Arc::new(StaticOwnerAccess { group }),
    );
    let token = token(&keys, ISSUER, OWNER_AUD, "heinrich", future_exp());

    let ctx = host
        .authenticate(&token)
        .await
        .expect("owner authenticates");

    assert_eq!(ctx.tool_scope(), &ToolScope::All);
}

#[tokio::test]
async fn unknown_audience_is_rejected_by_both_validators() {
    let keys = test_keys();
    let group = GroupId::new(Uuid::now_v7());
    let host = TwoAudienceHost::new(
        resolver(keys.decoding.clone()),
        Arc::new(StaticOwnerAccess { group }),
    );
    let token = token(&keys, ISSUER, "some-other-api", "anyone", future_exp());

    let err = host
        .authenticate(&token)
        .await
        .expect_err("neither validator accepts an unknown audience");
    assert_eq!(err, AuthError::InvalidCredentials);
}

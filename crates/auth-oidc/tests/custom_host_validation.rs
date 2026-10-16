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
use proxima_auth_oidc::{
    OidcAuthConfig, OidcAuthenticator, OidcBinding, OidcBindingSet, OidcClaimMap, OidcRoleShape,
    OidcRoleShaper, OidcSubjectMap, OidcTokenValidator, StaticJwksResolver, SubjectBinding,
    ValidatedOidcToken,
};
use proxima_core::{
    AccessError, AccessKind, ActionName, AuthError, AuthPath, Authenticator, AuthzContext,
    Credentials, GroupId, OwnerAccessPort, OwnerId, OwnerRoles, Role, ScopeKey, ToolName,
    ToolScope, UserId,
};
use serde::Serialize;
use uuid::Uuid;

const ISSUER: &str = "https://issuer.example";
const AGENT_AUD: &str = "centauri-agent";
const OWNER_AUD: &str = "centauri-owner";
const KID: &str = "k1";

fn key(text: &str) -> ScopeKey {
    ScopeKey::parse(text).expect("a scope key")
}

/// The call gate's question, asked in the spelling the tests read best.
fn allows_leaf(scope: &ToolScope, tool: &str, action: &str) -> bool {
    scope.allows_action(
        &ToolName::parse(tool).expect("a tool name"),
        &ActionName::parse(action).expect("an action name"),
    )
}

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
    let claims = TestClaims {
        sub: sub.to_owned(),
        iss: issuer.to_owned(),
        aud: audience.to_owned(),
        exp,
    };
    signed_claims(keys, &claims, "JWT")
}

fn signed_claims(keys: &TestKeys, claims: &impl Serialize, typ: &str) -> String {
    let header = serde_json::json!({"alg": "RS256", "kid": KID, "typ": typ});
    let signing_input = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).expect("serialize header")),
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).expect("serialize claims"))
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
        OwnerRoles::for_subject(subject, [(OwnerId::from(self.group), Role::editor())])
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
    agent_principal: OwnerId,
    owner_principal: OwnerId,
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
            agent_principal: OwnerId::from(UserId::new(Uuid::from_u128(0xA9E1))),
            owner_principal: OwnerId::from(UserId::new(Uuid::from_u128(0x0E1E))),
            owner_access,
        }
    }

    async fn authenticate(&self, token: &str) -> Result<AuthzContext, AuthError> {
        if let Ok(claims) = self.agent_validator.validate(token).await {
            let subject = self.agent_principal.as_user();
            let roles = self
                .owner_access
                .resolve_roles_for_subject(subject)
                .await
                .map_err(|_| AuthError::InvalidCredentials)?;
            return Ok(AuthzContext::server_resolved(roles, AuthPath::HostBearer)
                .with_expires_at(Some(claims.expires_at))
                .with_tool_scope(ToolScope::Palette(vec![key("core_goal:set")])));
        }
        if let Ok(claims) = self.owner_validator.validate(token).await {
            let subject = self.owner_principal.as_user();
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
    assert!(ctx.may_write(&OwnerId::from(group), proxima_core::AccessKind::Perspective));
    assert!(allows_leaf(ctx.tool_scope(), "core_goal", "set"));
    assert!(!ctx.tool_scope().allows(&key("core_membership")));
    assert!(!allows_leaf(
        ctx.tool_scope(),
        "core_transfer",
        "transfer_to_owner"
    ));
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

// ------------------------------------- trusted model provenance at the edge

/// The default host-resolved path with a subject map, as a deployment
/// configures it. Only the map decides whether a principal is a configured
/// runner; nothing on the wire does.
fn mapped_authenticator(keys: &TestKeys, map: OidcSubjectMap, group: GroupId) -> OidcAuthenticator {
    let config = OidcAuthConfig {
        issuer: ISSUER.to_owned(),
        jwks_uri: None,
        audience: AGENT_AUD.to_owned(),
        allowed_subjects: None,
        leeway_secs: 0,
    };
    OidcAuthenticator::new(
        config,
        resolver(keys.decoding.clone()),
        map,
        Arc::new(StaticOwnerAccess { group }),
    )
    .expect("valid oidc config")
}

const RUNNER_SUB: &str = "agent-service-user";
const RUNNER_MODEL: &str = "acme/runner-v3";

fn runner_map(user_id: UserId, trusted: bool) -> OidcSubjectMap {
    let mut map = OidcSubjectMap::new();
    let binding = if trusted {
        SubjectBinding::new(user_id).with_trusted_model_id(RUNNER_MODEL)
    } else {
        SubjectBinding::new(user_id)
    };
    map.insert_binding(ISSUER, RUNNER_SUB, binding)
        .expect("subject map");
    map
}

/// A principal the deployment declared as a configured runner reaches the
/// engine with that runner identity bound to its `AuthzContext` — the value
/// flavors read through `ctx.caller()`.
#[tokio::test]
async fn a_mapped_runner_principal_reaches_the_edge_with_its_trusted_model_id() {
    let keys = test_keys();
    let group = GroupId::new(Uuid::now_v7());
    let user_id = UserId::new(Uuid::now_v7());
    let auth = mapped_authenticator(&keys, runner_map(user_id, true), group);
    let token = token(&keys, ISSUER, AGENT_AUD, RUNNER_SUB, future_exp());

    let ctx = auth
        .authenticate(&Credentials::Bearer(token))
        .await
        .expect("mapped principal authenticates");

    assert_eq!(ctx.trusted_model_id(), Some(RUNNER_MODEL));
    assert_eq!(ctx.subject(), Some(user_id));

    // Owner narrowing is the last thing the edge does before dispatch; the
    // bound identity must still be there afterwards.
    let narrowed = ctx
        .narrowed_to_owner(OwnerId::from(group))
        .expect("editor narrows to the group owner");
    assert_eq!(narrowed.trusted_model_id(), Some(RUNNER_MODEL));
}

/// The same principal, with no `trusted_model_id` in the map: authenticated
/// exactly as before, certifying no model. Unmapped deployments are
/// unchanged by this feature.
#[tokio::test]
async fn a_principal_the_map_declares_no_runner_for_carries_none() {
    let keys = test_keys();
    let group = GroupId::new(Uuid::now_v7());
    let user_id = UserId::new(Uuid::now_v7());
    let auth = mapped_authenticator(&keys, runner_map(user_id, false), group);
    let token = token(&keys, ISSUER, AGENT_AUD, RUNNER_SUB, future_exp());

    let ctx = auth
        .authenticate(&Credentials::Bearer(token))
        .await
        .expect("mapped principal authenticates");

    assert_eq!(ctx.trusted_model_id(), None);
    assert_eq!(ctx.subject(), Some(user_id));
}

/// A caller that merely *presents* the runner's `sub` gains nothing: an
/// entry in the map is what binds a runner, and a subject that is not in
/// the map is not authenticated at all.
#[tokio::test]
async fn an_unmapped_subject_cannot_claim_the_runner_identity() {
    let keys = test_keys();
    let group = GroupId::new(Uuid::now_v7());
    let auth = mapped_authenticator(&keys, runner_map(UserId::new(Uuid::now_v7()), true), group);
    let token = token(&keys, ISSUER, AGENT_AUD, "some-other-sub", future_exp());

    assert_eq!(
        auth.authenticate(&Credentials::Bearer(token)).await,
        Err(AuthError::InvalidCredentials)
    );
}

/// A custom host that shapes its own context binds no model identity unless
/// it says so — `TwoAudienceHost` above never calls
/// `with_trusted_model_id`, so neither of its audiences certifies a model.
#[tokio::test]
async fn a_custom_host_that_binds_nothing_certifies_nothing() {
    let keys = test_keys();
    let group = GroupId::new(Uuid::now_v7());
    let host = TwoAudienceHost::new(
        resolver(keys.decoding.clone()),
        Arc::new(StaticOwnerAccess { group }),
    );

    for audience in [AGENT_AUD, OWNER_AUD] {
        let token = token(&keys, ISSUER, audience, "anyone", future_exp());
        let ctx = host.authenticate(&token).await.expect("authenticates");
        assert_eq!(ctx.trusted_model_id(), None, "audience {audience}");
    }
}

const FORWARDING_AUDIENCE: &str = "proxima-forwarded";
const FORWARDING_SUBJECT: &str = "forwarder-subject";
const ORDINARY_SUBJECT: &str = "ordinary-subject";
const UNAVAILABLE_POLICY_SUBJECT: &str = "unavailable-policy-subject";
const FORWARDING_MODEL: &str = "example/forwarder-model";

fn forwarding_subject() -> UserId {
    UserId::new(Uuid::from_u128(0xf100))
}

fn ordinary_subject() -> UserId {
    UserId::new(Uuid::from_u128(0xf101))
}

fn unavailable_policy_subject() -> UserId {
    UserId::new(Uuid::from_u128(0xf102))
}

#[derive(Debug)]
struct ForwardingOwnerAccess;

#[async_trait]
impl OwnerAccessPort for ForwardingOwnerAccess {
    fn is_forwarder(&self, subject: UserId) -> Result<bool, AccessError> {
        if subject == unavailable_policy_subject() {
            return Err(AccessError::Resolution("policy unavailable".to_owned()));
        }
        Ok(subject == forwarding_subject())
    }

    async fn resolve_roles_for_subject(&self, subject: UserId) -> Result<OwnerRoles, AccessError> {
        assert_ne!(
            subject,
            forwarding_subject(),
            "forwarding does not enumerate roles"
        );
        assert_ne!(
            subject,
            unavailable_policy_subject(),
            "a failed policy resolves no roles"
        );
        OwnerRoles::for_subject(subject, [])
    }

    async fn resolve_forwarded_owner_role(
        &self,
        _subject: UserId,
        _owner: OwnerId,
    ) -> Result<Option<proxima_core::access::ForwarderRole>, AccessError> {
        panic!("the forwarded owner is resolved only after request owner agreement")
    }
}

fn forwarding_config(audience: &str) -> OidcAuthConfig {
    OidcAuthConfig {
        issuer: ISSUER.to_owned(),
        jwks_uri: None,
        audience: audience.to_owned(),
        allowed_subjects: None,
        leeway_secs: 0,
    }
}

fn forwarding_subject_map() -> OidcSubjectMap {
    let mut map = OidcSubjectMap::new();
    map.insert_binding(
        ISSUER,
        FORWARDING_SUBJECT,
        SubjectBinding::new(forwarding_subject()).with_trusted_model_id(FORWARDING_MODEL),
    )
    .expect("forwarding subject binding");
    map.insert(ISSUER, ORDINARY_SUBJECT, ordinary_subject())
        .expect("ordinary subject binding");
    map.insert(
        ISSUER,
        UNAVAILABLE_POLICY_SUBJECT,
        unavailable_policy_subject(),
    )
    .expect("unavailable policy subject binding");
    map
}

fn forwarding_authenticator(keys: &TestKeys) -> OidcAuthenticator {
    OidcAuthenticator::new(
        forwarding_config(FORWARDING_AUDIENCE),
        resolver(keys.decoding.clone()),
        forwarding_subject_map(),
        Arc::new(ForwardingOwnerAccess),
    )
    .expect("forwarding authenticator")
}

fn forwarding_bindings(keys: &TestKeys, shape: OidcRoleShape) -> OidcBindingSet {
    let access: Arc<dyn OwnerAccessPort> = Arc::new(ForwardingOwnerAccess);
    OidcBindingSet::new([
        OidcBinding::with_role_shape(
            forwarding_config(FORWARDING_AUDIENCE),
            resolver(keys.decoding.clone()),
            forwarding_subject_map(),
            access.clone(),
            shape,
        )
        .expect("forwarding binding"),
        OidcBinding::new(
            forwarding_config("proxima-other-audience"),
            resolver(keys.decoding.clone()),
            forwarding_subject_map(),
            access,
        )
        .expect("other audience binding"),
    ])
    .expect("forwarding binding set")
}

fn forwarding_authenticators(keys: &TestKeys) -> [(&'static str, Arc<dyn Authenticator>); 2] {
    [
        ("single", Arc::new(forwarding_authenticator(keys))),
        (
            "bindings",
            Arc::new(forwarding_bindings(keys, OidcRoleShape::ServerResolved)),
        ),
    ]
}

fn forwarding_claims(owner: Option<serde_json::Value>) -> serde_json::Value {
    let mut claims = serde_json::json!({
        "iss": ISSUER,
        "aud": FORWARDING_AUDIENCE,
        "sub": FORWARDING_SUBJECT,
        "exp": future_exp(),
    });
    if let Some(owner) = owner {
        claims["owner"] = owner;
    }
    claims
}

#[tokio::test]
async fn forwarded_owner_remains_pending_in_single_and_multiple_bindings() {
    let keys = test_keys();
    let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
    let claims = forwarding_claims(Some(serde_json::json!(owner)));
    let bearer = Credentials::Bearer(signed_claims(&keys, &claims, "JWT"));
    for (path, authenticator) in forwarding_authenticators(&keys) {
        let context = proxima_core::authenticate(authenticator.as_ref(), &bearer)
            .await
            .expect("a verified owner claim authenticates");
        assert_eq!(context.subject(), Some(forwarding_subject()), "{path}");
        assert_eq!(
            context.principal(),
            OwnerId::from(forwarding_subject()),
            "{path}"
        );
        assert_eq!(context.verified_forwarded_owner(), Some(owner), "{path}");
        assert_eq!(context.trusted_model_id(), Some(FORWARDING_MODEL), "{path}");
        assert!(context.expires_at().is_some(), "{path}");
        assert!(
            context.readable_owners(AccessKind::Fact).is_empty(),
            "{path}"
        );
        assert!(
            context.writable_owners(AccessKind::Fact).is_empty(),
            "{path}"
        );
        assert!(context.role_for_owner(&owner).is_none(), "{path}");
        assert!(
            context
                .owner_scope()
                .expect("core seals authentication")
                .readable_owners(AccessKind::Fact)
                .is_empty(),
            "{path}"
        );
    }
}

#[tokio::test]
async fn forwarded_owner_requires_a_plain_uuid_claim() {
    let keys = test_keys();
    let owner = Uuid::now_v7();
    let invalid = [
        None,
        Some(serde_json::Value::Null),
        Some(serde_json::json!(42)),
        Some(serde_json::json!(true)),
        Some(serde_json::json!({"id": owner})),
        Some(serde_json::json!([owner])),
        Some(serde_json::json!("not-an-owner")),
        Some(serde_json::json!(owner.simple().to_string())),
        Some(serde_json::json!(format!("personal:{owner}"))),
        Some(serde_json::json!(owner.urn().to_string())),
    ];
    for (path, authenticator) in forwarding_authenticators(&keys) {
        for owner in &invalid {
            let claims = forwarding_claims(owner.clone());
            let bearer = Credentials::Bearer(signed_claims(&keys, &claims, "JWT"));
            assert_eq!(
                authenticator.authenticate(&bearer).await,
                Err(AuthError::InvalidCredentials),
                "{path}: {owner:?}"
            );
        }
    }
}

#[tokio::test]
async fn forwarded_owner_claim_is_ignored_for_an_ordinary_subject() {
    let keys = test_keys();
    for (path, authenticator) in forwarding_authenticators(&keys) {
        for owner in [
            serde_json::json!(Uuid::now_v7()),
            serde_json::Value::Null,
            serde_json::json!(42),
        ] {
            let mut claims = forwarding_claims(Some(owner));
            claims["sub"] = serde_json::json!(ORDINARY_SUBJECT);
            let context = authenticator
                .authenticate(&Credentials::Bearer(signed_claims(&keys, &claims, "JWT")))
                .await
                .expect("an ordinary subject ignores the owner claim");
            assert_eq!(context.subject(), Some(ordinary_subject()), "{path}");
            assert_eq!(context.verified_forwarded_owner(), None, "{path}");
            assert_eq!(context.default_owner(), None, "{path}");
            assert_eq!(
                context.readable_owners(AccessKind::Fact),
                vec![OwnerId::from(ordinary_subject())],
                "{path}"
            );
        }
    }
}

#[tokio::test]
async fn forwarded_owner_policy_failure_refuses_without_resolving_roles() {
    let keys = test_keys();
    let mut claims = forwarding_claims(Some(serde_json::json!(Uuid::now_v7())));
    claims["sub"] = serde_json::json!(UNAVAILABLE_POLICY_SUBJECT);
    let bearer = Credentials::Bearer(signed_claims(&keys, &claims, "JWT"));
    for (path, authenticator) in forwarding_authenticators(&keys) {
        assert_eq!(
            authenticator.authenticate(&bearer).await,
            Err(AuthError::InvalidCredentials),
            "{path}"
        );
    }
}

#[tokio::test]
async fn forwarded_owner_still_requires_valid_token_credentials() {
    let keys = test_keys();
    let other_keys = test_keys();
    let valid = forwarding_claims(Some(serde_json::json!(Uuid::now_v7())));
    for (path, authenticator) in forwarding_authenticators(&keys) {
        for (field, value) in [
            ("iss", serde_json::json!("https://other-issuer.example")),
            ("aud", serde_json::json!("unconfigured-audience")),
            ("sub", serde_json::json!("unmapped-subject")),
            ("exp", serde_json::json!(1)),
            ("nbf", serde_json::json!(future_exp())),
        ] {
            let mut claims = valid.clone();
            claims[field] = value;
            assert_eq!(
                authenticator
                    .authenticate(&Credentials::Bearer(signed_claims(&keys, &claims, "JWT")))
                    .await,
                Err(AuthError::InvalidCredentials),
                "{path}: {field}"
            );
        }
        assert_eq!(
            authenticator
                .authenticate(&Credentials::Bearer(signed_claims(
                    &other_keys,
                    &valid,
                    "JWT"
                )))
                .await,
            Err(AuthError::InvalidCredentials),
            "{path}: wrong signing key"
        );
    }
    let authenticator = forwarding_authenticator(&keys).require_access_token_typ();
    assert_eq!(
        authenticator
            .authenticate(&Credentials::Bearer(signed_claims(&keys, &valid, "JWT")))
            .await,
        Err(AuthError::InvalidCredentials)
    );
    assert!(
        authenticator
            .authenticate(&Credentials::Bearer(signed_claims(&keys, &valid, "at+jwt")))
            .await
            .is_ok()
    );
}

#[derive(Debug, Clone, Copy)]
enum ForwardingShape {
    Preserve,
    Clear,
    Change,
    Grant,
}

impl OidcRoleShaper for ForwardingShape {
    fn shape(
        &self,
        context: AuthzContext,
        _token: &ValidatedOidcToken<OidcClaimMap>,
    ) -> Result<AuthzContext, AuthError> {
        let subject = context.subject().expect("authenticated subject");
        let owner = context.verified_forwarded_owner().expect("verified owner");
        let context = match self {
            Self::Preserve => {
                context.with_tool_scope(ToolScope::Palette(vec![key("host_forwarded")]))
            }
            Self::Clear => AuthzContext::for_subject(subject, AuthPath::HostBearer),
            Self::Change => AuthzContext::from_verified_forwarded_owner(
                subject,
                OwnerId::from(UserId::new(Uuid::now_v7())),
            ),
            Self::Grant => AuthzContext::for_subject_with_role(
                subject,
                [(owner, Role::editor())],
                AuthPath::HostBearer,
            ),
        };
        let context = context
            .with_trusted_model_id(FORWARDING_MODEL)
            .map_err(|_| AuthError::InvalidCredentials)?;
        context
            .with_publication_extensions(
                proxima_core::publication::PublicationExtensions::new()
                    .with("forwarding", "verified")
                    .expect("valid publication extension"),
            )
            .map_err(|_| AuthError::InvalidCredentials)
    }
}

#[tokio::test]
async fn forwarded_owner_host_shaping_preserves_the_pending_binding() {
    let keys = test_keys();
    let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
    let bearer = Credentials::Bearer(signed_claims(
        &keys,
        &forwarding_claims(Some(serde_json::json!(owner))),
        "JWT",
    ));
    let bindings = forwarding_bindings(
        &keys,
        OidcRoleShape::Host(Arc::new(ForwardingShape::Preserve)),
    );
    let context = bindings
        .authenticate(&bearer)
        .await
        .expect("narrowing shape");
    assert_eq!(context.verified_forwarded_owner(), Some(owner));
    assert_eq!(context.subject(), Some(forwarding_subject()));
    assert_eq!(context.trusted_model_id(), Some(FORWARDING_MODEL));
    assert_eq!(
        context.publication_extensions().get("forwarding"),
        Some(&proxima_core::publication::ExtensionValue::from("verified"))
    );
    assert!(
        context.expires_at().is_some(),
        "expiry is clamped to the token"
    );
    assert!(context.tool_scope().allows(&key("host_forwarded")));
    assert!(!context.tool_scope().allows(&key("other_tool")));
    assert!(context.readable_owners(AccessKind::Fact).is_empty());
    for shape in [
        ForwardingShape::Clear,
        ForwardingShape::Change,
        ForwardingShape::Grant,
    ] {
        let bindings = forwarding_bindings(&keys, OidcRoleShape::Host(Arc::new(shape)));
        assert_eq!(
            bindings.authenticate(&bearer).await,
            Err(AuthError::InvalidCredentials),
            "{shape:?}"
        );
    }
    let bindings = forwarding_bindings(
        &keys,
        OidcRoleShape::ServerResolvedWithToolScope(ToolScope::Palette(vec![key("host_forwarded")])),
    );
    let context = bindings
        .authenticate(&bearer)
        .await
        .expect("static scope shape");
    assert_eq!(context.verified_forwarded_owner(), Some(owner));
    assert!(context.tool_scope().allows(&key("host_forwarded")));
    assert!(!context.tool_scope().allows(&key("other_tool")));
}

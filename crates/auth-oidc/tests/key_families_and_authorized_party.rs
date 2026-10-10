//! Signature algorithms by key family and the authorized-party policy,
//! through the public API only.
//!
//! Every key here is generated in the test: nothing in this file is a
//! credential.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use aws_lc_rs::hmac;
use aws_lc_rs::rand::SystemRandom;
use aws_lc_rs::rsa::KeySize;
use aws_lc_rs::signature::{
    ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, Ed25519KeyPair, KeyPair as _, RSA_PKCS1_SHA256,
    RSA_PKCS1_SHA384, RSA_PKCS1_SHA512, RsaKeyPair, RsaPublicKeyComponents,
};
use axum::{Router, routing::get};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::DecodingKey;
use proxima_auth_oidc::{
    AuthorizedPartyPolicy, HttpJwksResolver, KeyResolver, OidcAuthConfig, OidcAuthenticator,
    OidcBinding, OidcBindingSet, OidcRejection, OidcSubjectMap, OidcTokenValidator,
    StaticJwksResolver,
};
use proxima_core::{
    AccessError, AuthError, Authenticator, Credentials, OwnerAccessPort, OwnerRoles, UserId,
};
use serde_json::{Value, json};
use uuid::Uuid;

const ISSUER: &str = "https://issuer.example";
const AUDIENCE: &str = "proxima-api";
const OTHER_AUDIENCE: &str = "proxima-admin";

/// One key pair per family, generated once for the whole file. The kid of
/// each is its family name.
struct Keys {
    rsa: RsaKeyPair,
    ec: EcdsaKeyPair,
    ed: Ed25519KeyPair,
}

static KEYS: LazyLock<Keys> = LazyLock::new(|| {
    let rng = SystemRandom::new();
    let ec_pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng)
        .expect("generate test P-256 key");
    Keys {
        rsa: RsaKeyPair::generate(KeySize::Rsa2048).expect("generate test RSA key"),
        ec: EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, ec_pkcs8.as_ref())
            .expect("load test P-256 key"),
        ed: Ed25519KeyPair::generate().expect("generate test Ed25519 key"),
    }
});

fn b64(bytes: impl AsRef<[u8]>) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

impl Keys {
    /// The public halves as a JWKS, each entry named for its family.
    fn jwks(&self) -> Value {
        let rsa = RsaPublicKeyComponents::<Vec<u8>>::from(self.rsa.public_key());
        let ec = self.ec.public_key().as_ref();
        assert_eq!(ec.len(), 65, "uncompressed P-256 point");
        json!({ "keys": [
            { "kid": "rsa", "kty": "RSA", "alg": "RS256", "use": "sig",
              "n": b64(&rsa.n), "e": b64(&rsa.e) },
            { "kid": "ec", "kty": "EC", "crv": "P-256", "alg": "ES256", "use": "sig",
              "x": b64(&ec[1..33]), "y": b64(&ec[33..]) },
            { "kid": "ed", "kty": "OKP", "crv": "Ed25519", "alg": "EdDSA", "use": "sig",
              "x": b64(self.ed.public_key()) },
        ]})
    }

    /// The public key bytes an attacker would use as an HMAC secret.
    fn public_bytes(&self, kid: &str) -> Vec<u8> {
        match kid {
            "rsa" => self.rsa.public_key().as_ref().to_vec(),
            "ec" => self.ec.public_key().as_ref().to_vec(),
            "ed" => self.ed.public_key().as_ref().to_vec(),
            other => panic!("no test key {other}"),
        }
    }
}

/// A signature algorithm the validator accepts, with the key family it signs
/// for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Alg {
    Rs256,
    Rs384,
    Rs512,
    Es256,
    EdDsa,
}

impl Alg {
    const ALL: [Self; 5] = [
        Self::Rs256,
        Self::Rs384,
        Self::Rs512,
        Self::Es256,
        Self::EdDsa,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Rs256 => "RS256",
            Self::Rs384 => "RS384",
            Self::Rs512 => "RS512",
            Self::Es256 => "ES256",
            Self::EdDsa => "EdDSA",
        }
    }

    /// The kid of the key that signs for this algorithm.
    fn kid(self) -> &'static str {
        match self {
            Self::Rs256 | Self::Rs384 | Self::Rs512 => "rsa",
            Self::Es256 => "ec",
            Self::EdDsa => "ed",
        }
    }

    fn sign(self, input: &[u8]) -> Vec<u8> {
        let rng = SystemRandom::new();
        let rsa = |padding: &'static dyn aws_lc_rs::signature::RsaEncoding| {
            let mut signature = vec![0; KEYS.rsa.public_modulus_len()];
            KEYS.rsa
                .sign(padding, &rng, input, &mut signature)
                .expect("sign with RSA");
            signature
        };
        match self {
            Self::Rs256 => rsa(&RSA_PKCS1_SHA256),
            Self::Rs384 => rsa(&RSA_PKCS1_SHA384),
            Self::Rs512 => rsa(&RSA_PKCS1_SHA512),
            Self::Es256 => KEYS
                .ec
                .sign(&rng, input)
                .expect("sign with P-256")
                .as_ref()
                .to_vec(),
            Self::EdDsa => KEYS.ed.sign(input).as_ref().to_vec(),
        }
    }
}

fn compact(header: &Value, claims: &Value, signature: impl FnOnce(&[u8]) -> Vec<u8>) -> String {
    let signing_input = format!(
        "{}.{}",
        b64(serde_json::to_vec(header).expect("header")),
        b64(serde_json::to_vec(claims).expect("claims"))
    );
    let signature = signature(signing_input.as_bytes());
    format!("{signing_input}.{}", b64(signature))
}

/// A token signed with `alg`'s key but naming `kid`, so a mismatch between
/// the header and the key the resolver hands back can be built.
fn signed_as(alg: Alg, kid: &str, claims: &Value) -> String {
    let header = json!({ "alg": alg.name(), "kid": kid, "typ": "JWT" });
    compact(&header, claims, |input| alg.sign(input))
}

fn signed(alg: Alg, claims: &Value) -> String {
    signed_as(alg, alg.kid(), claims)
}

/// A header naming `alg` over a signature that was never made: for algorithms
/// the validator must refuse before it looks at the signature.
fn forged(alg: &str, kid: &str, claims: &Value) -> String {
    let header = json!({ "alg": alg, "kid": kid, "typ": "JWT" });
    compact(&header, claims, |_| vec![0; 64])
}

/// HS256 with the target key's public bytes as the shared secret: the
/// classic algorithm-confusion token.
fn hs256_with_public_key_as_secret(kid: &str, claims: &Value) -> String {
    let secret = KEYS.public_bytes(kid);
    let header = json!({ "alg": "HS256", "kid": kid, "typ": "JWT" });
    compact(&header, claims, |input| {
        hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, &secret), input)
            .as_ref()
            .to_vec()
    })
}

fn future_exp() -> u64 {
    jsonwebtoken::get_current_timestamp() + 3_600
}

fn claims_for(issuer: &str, audience: impl Into<Value>, extra: Value) -> Value {
    let mut claims = json!({
        "sub": "subject-1",
        "iss": issuer,
        "aud": audience.into(),
        "exp": future_exp(),
    });
    let (Some(object), Value::Object(extra)) = (claims.as_object_mut(), extra) else {
        panic!("claims and extras are objects");
    };
    object.extend(extra);
    claims
}

fn claims(extra: Value) -> Value {
    claims_for(ISSUER, json!(AUDIENCE), extra)
}

fn config(issuer: &str, audience: &str) -> OidcAuthConfig {
    OidcAuthConfig {
        issuer: issuer.to_owned(),
        jwks_uri: None,
        audience: audience.to_owned(),
        allowed_subjects: None,
        leeway_secs: 0,
    }
}

fn pinned() -> Arc<dyn KeyResolver> {
    Arc::new(
        StaticJwksResolver::from_jwks_json(&KEYS.jwks().to_string()).expect("pinned test JWKS"),
    )
}

fn validator(keys: Arc<dyn KeyResolver>) -> OidcTokenValidator {
    OidcTokenValidator::new(config(ISSUER, AUDIENCE), keys).expect("validator")
}

async fn rejection(validator: &OidcTokenValidator, token: &str) -> OidcRejection {
    validator
        .validate_with::<Value>(token)
        .await
        .expect_err("the token must be refused")
}

/// Serves `jwks` at `/keys` of a loopback issuer and returns the issuer URL.
async fn serve_jwks(jwks: Value) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test listener");
    let issuer = format!("http://{}", listener.local_addr().expect("local addr"));
    let body = jwks.to_string();
    let app = Router::new().route("/keys", get(move || async move { body }));
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("mock issuer");
    });
    (issuer, server)
}

// -- key families ----------------------------------------------------------

#[tokio::test]
async fn each_key_family_verifies_through_a_pinned_set() {
    let validator = validator(pinned());

    for alg in Alg::ALL {
        let validated = validator
            .validate(&signed(alg, &claims(json!({}))))
            .await
            .unwrap_or_else(|err| panic!("{} must verify: {err:?}", alg.name()));
        assert_eq!(validated.subject, "subject-1");
    }
}

#[tokio::test]
async fn each_key_family_verifies_over_http() {
    let (issuer, server) = serve_jwks(KEYS.jwks()).await;
    let resolver = HttpJwksResolver::new(issuer.clone(), Some(format!("{issuer}/keys")))
        .expect("loopback issuer");
    let validator =
        OidcTokenValidator::new(config(&issuer, AUDIENCE), Arc::new(resolver)).expect("validator");

    for alg in Alg::ALL {
        let token = signed(alg, &claims_for(&issuer, json!(AUDIENCE), json!({})));
        let validated = validator
            .validate(&token)
            .await
            .unwrap_or_else(|err| panic!("{} must verify over HTTP: {err:?}", alg.name()));
        assert_eq!(validated.issuer, issuer);
    }
    server.abort();
}

/// The accepted algorithm follows the key. Every signer against every key:
/// only the pair of one family verifies, and the rest are refused as a
/// disallowed algorithm before any signature is checked.
#[tokio::test]
async fn only_the_algorithms_of_the_keys_family_are_accepted() {
    let validator = validator(pinned());

    for alg in Alg::ALL {
        for kid in ["rsa", "ec", "ed"] {
            let result = validator
                .validate_with::<Value>(&signed_as(alg, kid, &claims(json!({}))))
                .await;
            if kid == alg.kid() {
                assert!(result.is_ok(), "{} with the {kid} key", alg.name());
            } else {
                assert_eq!(
                    result.err(),
                    Some(OidcRejection::DisallowedAlgorithm),
                    "{} against the {kid} key",
                    alg.name()
                );
            }
        }
    }
}

#[tokio::test]
async fn algorithms_the_validator_never_verifies_are_disallowed_whatever_the_key() {
    let validator = validator(pinned());

    for (alg, kid) in [
        ("PS256", "rsa"),
        ("PS512", "rsa"),
        ("ES384", "ec"),
        ("HS384", "rsa"),
        ("HS512", "ed"),
    ] {
        let token = forged(alg, kid, &claims(json!({})));
        assert_eq!(
            validator.validate_with::<Value>(&token).await.err(),
            Some(OidcRejection::DisallowedAlgorithm),
            "{alg} against the {kid} key"
        );
    }
}

#[tokio::test]
async fn hs256_is_refused_against_every_key_type() {
    let validator = validator(pinned());

    for kid in ["rsa", "ec", "ed"] {
        let token = hs256_with_public_key_as_secret(kid, &claims(json!({})));
        assert_eq!(
            rejection(&validator, &token).await,
            OidcRejection::DisallowedAlgorithm,
            "HS256 against the {kid} key"
        );
    }
}

/// A shared secret verifies no algorithm at all: an HMAC key in a resolver
/// does not turn HS256 on.
#[tokio::test]
async fn hs256_is_refused_against_a_shared_secret_key() {
    let secret = b"a-shared-secret-of-sufficient-length";
    let resolver = Arc::new(StaticJwksResolver::new(HashMap::from([(
        "hmac".to_owned(),
        Arc::new(DecodingKey::from_secret(secret)),
    )])));
    let validator = validator(resolver);
    let header = json!({ "alg": "HS256", "kid": "hmac", "typ": "JWT" });
    let token = compact(&header, &claims(json!({})), |input| {
        hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, secret), input)
            .as_ref()
            .to_vec()
    });

    assert_eq!(
        rejection(&validator, &token).await,
        OidcRejection::DisallowedAlgorithm
    );
}

#[tokio::test]
async fn alg_none_is_refused() {
    let validator = validator(pinned());
    let header = json!({ "alg": "none", "kid": "rsa", "typ": "JWT" });
    let unsigned = compact(&header, &claims(json!({})), |_| Vec::new());

    for token in [unsigned.clone(), unsigned.replace("none", "None")] {
        assert!(
            validator.validate_with::<Value>(&token).await.is_err(),
            "{token}"
        );
    }
    assert_eq!(
        validator.validate(&unsigned).await,
        Err(AuthError::InvalidCredentials)
    );
}

// -- authorized party ------------------------------------------------------

fn policy(allowed: &[&str]) -> AuthorizedPartyPolicy {
    AuthorizedPartyPolicy::new(allowed.iter().copied()).expect("policy")
}

fn with_policy(allowed: &[&str]) -> OidcTokenValidator {
    validator(pinned()).with_authorized_party_policy(policy(allowed))
}

fn rs256(extra: Value) -> String {
    signed(Alg::Rs256, &claims(extra))
}

#[tokio::test]
async fn an_azp_outside_the_allowed_set_is_refused() {
    let validator = with_policy(&["web", "cli"]);

    assert_eq!(
        rejection(&validator, &rs256(json!({ "azp": "mobile" }))).await,
        OidcRejection::UnauthorizedParty {
            azp: "mobile".into()
        }
    );
    // Exact and case-sensitive.
    assert_eq!(
        rejection(&validator, &rs256(json!({ "azp": "WEB" }))).await,
        OidcRejection::UnauthorizedParty { azp: "WEB".into() }
    );
    assert!(
        validator
            .validate(&rs256(json!({ "azp": "web" })))
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn several_audiences_need_an_allowed_azp() {
    let validator = with_policy(&["web"]);
    let audiences = json!([AUDIENCE, OTHER_AUDIENCE]);
    let token = |extra: Value| signed(Alg::Rs256, &claims_for(ISSUER, audiences.clone(), extra));

    assert_eq!(
        rejection(&validator, &token(json!({}))).await,
        OidcRejection::MissingAuthorizedParty
    );
    assert!(
        validator
            .validate(&token(json!({ "azp": "web" })))
            .await
            .is_ok()
    );
    assert_eq!(
        rejection(&validator, &token(json!({ "azp": "other" }))).await,
        OidcRejection::UnauthorizedParty {
            azp: "other".into()
        }
    );
    // One value said twice is one audience.
    let repeated = signed(
        Alg::Rs256,
        &claims_for(ISSUER, json!([AUDIENCE, AUDIENCE]), json!({})),
    );
    assert!(validator.validate(&repeated).await.is_ok());
}

#[tokio::test]
async fn a_single_audience_without_azp_passes_and_a_non_string_azp_does_not() {
    let validator = with_policy(&["web"]);

    assert!(validator.validate(&rs256(json!({}))).await.is_ok());
    for azp in [
        json!(null),
        json!(7),
        json!(["web"]),
        json!({ "id": "web" }),
    ] {
        assert_eq!(
            rejection(&validator, &rs256(json!({ "azp": azp.clone() }))).await,
            OidcRejection::InvalidClaim("azp".into()),
            "azp {azp}"
        );
    }
}

/// The policy runs on the verified payload only: whatever `azp` an unsigned
/// or tampered token claims, the refusal is the signature's.
#[tokio::test]
async fn the_policy_never_runs_on_an_unverified_token() {
    let validator = with_policy(&["web"]);
    let tamper_signature = |token: &str| {
        let (head, signature) = token.rsplit_once('.').expect("compact jws");
        let mut bytes = URL_SAFE_NO_PAD.decode(signature).expect("signature");
        bytes[0] ^= 0xff;
        format!("{head}.{}", b64(bytes))
    };

    for azp in ["web", "mobile"] {
        let token = rs256(json!({ "azp": azp }));
        assert_eq!(
            rejection(&validator, &tamper_signature(&token)).await,
            OidcRejection::BadSignature,
            "flipped signature, azp {azp}"
        );
    }

    // A payload swapped under a signature made for another one.
    let signed_for_web = rs256(json!({ "azp": "web" }));
    let signed_for_mobile = rs256(json!({ "azp": "mobile" }));
    let payload_of = |token: &str| token.split('.').nth(1).expect("payload").to_owned();
    let (head, _) = signed_for_web.split_once('.').expect("header");
    let signature = signed_for_web.rsplit('.').next().expect("signature");
    let swapped = format!("{head}.{}.{signature}", payload_of(&signed_for_mobile));
    assert_eq!(
        rejection(&validator, &swapped).await,
        OidcRejection::BadSignature
    );
    let swapped_back = format!(
        "{}.{}.{}",
        signed_for_mobile.split('.').next().expect("header"),
        payload_of(&signed_for_web),
        signed_for_mobile.rsplit('.').next().expect("signature")
    );
    assert_eq!(
        rejection(&validator, &swapped_back).await,
        OidcRejection::BadSignature
    );
}

/// The policy checks the verified claims before the host's own claim type is
/// read, so a token the policy refuses is never handed to `C`.
#[tokio::test]
async fn the_policy_runs_before_the_hosts_claims_are_read() {
    #[derive(Debug, serde::Deserialize)]
    struct NeedsTenant {
        #[allow(dead_code, reason = "only deserialization is under test")]
        tenant: String,
    }
    let validator = with_policy(&["web"]);
    let token = rs256(json!({ "azp": "mobile" }));

    assert_eq!(
        validator.validate_with::<NeedsTenant>(&token).await.err(),
        Some(OidcRejection::UnauthorizedParty {
            azp: "mobile".into()
        })
    );
    // Accepted tokens still hand `C` every claim, azp included.
    let accepted = validator
        .validate_with::<Value>(&rs256(json!({ "azp": "web" })))
        .await
        .expect("allowed azp");
    assert_eq!(accepted.custom["azp"], "web");
}

#[tokio::test]
async fn without_a_policy_azp_and_several_audiences_are_accepted_as_before() {
    let validator = validator(pinned());

    for extra in [json!({}), json!({ "azp": "anyone" }), json!({ "azp": 7 })] {
        assert!(
            validator.validate(&rs256(extra.clone())).await.is_ok(),
            "{extra}"
        );
    }
    let several = signed(
        Alg::Rs256,
        &claims_for(ISSUER, json!([AUDIENCE, OTHER_AUDIENCE]), json!({})),
    );
    assert!(validator.validate(&several).await.is_ok());
}

struct NoRoles;

#[async_trait]
impl OwnerAccessPort for NoRoles {
    async fn resolve_roles_for_subject(&self, subject: UserId) -> Result<OwnerRoles, AccessError> {
        OwnerRoles::for_subject(subject, [])
    }
}

fn subject_map() -> OidcSubjectMap {
    let mut map = OidcSubjectMap::new();
    map.insert(ISSUER, "subject-1", UserId::new(Uuid::from_u128(0xA9E1)))
        .expect("subject map");
    map
}

#[tokio::test]
async fn the_authenticator_enforces_its_policy_as_invalid_credentials() {
    let authenticator = OidcAuthenticator::new(
        config(ISSUER, AUDIENCE),
        pinned(),
        subject_map(),
        Arc::new(NoRoles),
    )
    .expect("authenticator")
    .with_authorized_party_policy(policy(&["web"]));
    let authenticate = |token: String| {
        let authenticator = &authenticator;
        async move {
            authenticator
                .authenticate(&Credentials::Bearer(token))
                .await
        }
    };

    assert!(
        authenticate(rs256(json!({ "azp": "web" }))).await.is_ok(),
        "allowed azp"
    );
    assert_eq!(
        authenticate(rs256(json!({ "azp": "mobile" }))).await.err(),
        Some(AuthError::InvalidCredentials)
    );
}

/// Two bindings on one issuer: each route enforces the set its own binding
/// carries, so a client allowed on one audience is refused on the other.
#[tokio::test]
async fn bindings_on_one_issuer_enforce_their_own_sets() {
    let binding = |audience: &str, allowed: &[&str]| {
        OidcBinding::new(
            config(ISSUER, audience),
            pinned(),
            subject_map(),
            Arc::new(NoRoles),
        )
        .expect("binding")
        .with_authorized_party_policy(policy(allowed))
    };
    let set = OidcBindingSet::new([
        binding(AUDIENCE, &["web"]),
        binding(OTHER_AUDIENCE, &["cli"]),
    ])
    .expect("binding set");
    let authenticate = |audience: &str, azp: &str| {
        let token = signed(
            Alg::Rs256,
            &claims_for(ISSUER, json!(audience), json!({ "azp": azp })),
        );
        let set = &set;
        async move { set.authenticate(&Credentials::Bearer(token)).await }
    };

    assert!(authenticate(AUDIENCE, "web").await.is_ok());
    assert!(authenticate(OTHER_AUDIENCE, "cli").await.is_ok());
    assert_eq!(
        authenticate(AUDIENCE, "cli").await.err(),
        Some(AuthError::InvalidCredentials)
    );
    assert_eq!(
        authenticate(OTHER_AUDIENCE, "web").await.err(),
        Some(AuthError::InvalidCredentials)
    );
}

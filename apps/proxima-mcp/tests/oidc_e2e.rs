//! End-to-end: boot the facade with an OIDC authenticator + resource metadata
//! against a real Postgres, then prove the security surface over HTTP:
//! discovery is public, `/mcp` is 401+WWW-Authenticate without a bearer, and a
//! valid Zitadel-shaped JWT lists + calls the Code-flavor tools.
//!
//! Requires `PROXIMA_TEST_DATABASE_URL`; skips cleanly otherwise. Built only
//! with `--features code` (asserts the Code flavor's tools are present).
#![cfg(feature = "code")]

mod common;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use aws_lc_rs::rand::SystemRandom;
use aws_lc_rs::rsa::KeySize;
use aws_lc_rs::signature::{KeyPair as _, RSA_PKCS1_SHA256, RsaKeyPair};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::DecodingKey;
use proxima::{Proxima, ResourceServerMetadata};
use proxima_auth_oidc::{OidcAuthConfig, OidcAuthenticator, OidcSubjectMap, StaticJwksResolver};
use proxima_core::storage_ports::OwnerMembershipAdminPort;
use proxima_core::{
    AccessKind, AuthPath, AuthzContext, Engine, FlavorRegistry, GroupId, Owner, OwnerAccessPort,
    OwnerRef, Relation, Role, UserId,
};
use proxima_mcp::ProximaMcpApp;
use proxima_storage_pg::{PgOwnerAccessResolver, PgStorage};
use serde_json::json;
use uuid::Uuid;

use common::require_env_or_skip;

const KID: &str = "e2e-key";
const ISSUER: &str = "https://idp.e2e.test";
const AUDIENCE: &str = "proxima-mcp";

fn keypair() -> (RsaKeyPair, StaticJwksResolver) {
    let signing = RsaKeyPair::generate(KeySize::Rsa2048).expect("generate test RSA key");
    let dec = DecodingKey::from_rsa_der(signing.public_key().as_ref());
    let mut keys = HashMap::new();
    keys.insert(KID.to_string(), Arc::new(dec));
    (signing, StaticJwksResolver::new(keys))
}

fn mint(signing: &RsaKeyPair, sub: &str) -> String {
    let exp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 3600;
    let header = json!({"alg": "RS256", "kid": KID, "typ": "JWT"});
    let claims = json!({"iss": ISSUER, "aud": AUDIENCE, "sub": sub, "exp": exp});
    let signing_input = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).expect("serialize header")),
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).expect("serialize claims"))
    );
    let mut signature = vec![0; signing.public_modulus_len()];
    signing
        .sign(
            &RSA_PKCS1_SHA256,
            &SystemRandom::new(),
            signing_input.as_bytes(),
            &mut signature,
        )
        .expect("sign jwt");
    format!("{}.{}", signing_input, URL_SAFE_NO_PAD.encode(signature))
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // linear e2e: boot + 4 assertion phases read best in one flow
async fn oidc_e2e_discovery_public_and_code_tools_behind_bearer()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(database_url) = require_env_or_skip("PROXIMA_TEST_DATABASE_URL") else {
        eprintln!("skipping oidc_e2e: PROXIMA_TEST_DATABASE_URL not set");
        return Ok(());
    };

    let subject = UserId::new(Uuid::now_v7());
    let owner: Owner = OwnerRef::Personal(subject);
    let owner_key = owner_header(owner);
    let (enc, resolver) = keypair();
    let mut subject_map = OidcSubjectMap::new();
    subject_map.insert(ISSUER, "operator-sub", subject)?;
    let owner_access: Arc<dyn OwnerAccessPort> =
        Arc::new(PgOwnerAccessResolver::connect_lazy(&database_url)?);
    let authn = OidcAuthenticator::new(
        OidcAuthConfig {
            issuer: ISSUER.to_string(),
            jwks_uri: None,
            audience: AUDIENCE.to_string(),
            allowed_subjects: None,
            leeway_secs: 60,
        },
        Arc::new(resolver),
        subject_map,
        owner_access.clone(),
    )?;

    let running = Proxima::<ProximaMcpApp>::app()
        .database_url(database_url)
        .owner_access(owner_access)
        .authenticator(Arc::new(authn))
        .resource_metadata(ResourceServerMetadata {
            public_url: "https://proxima.e2e.test".to_string(),
            authorization_servers: vec![ISSUER.to_string()],
        })
        .mcp_bind("127.0.0.1:0".parse().unwrap())
        .run()
        .await?;
    let addr = running.mcp_addr.ok_or("missing MCP listener address")?;
    let base = format!("http://{addr}");
    let url = format!("{base}/mcp");
    let client = reqwest::Client::new();

    // 1. Discovery is public — no Authorization header.
    let disc = client
        .get(format!("{base}/.well-known/oauth-protected-resource"))
        .send()
        .await?;
    assert_eq!(
        disc.status(),
        reqwest::StatusCode::OK,
        "discovery must be reachable unauthenticated"
    );
    let disc_json: serde_json::Value = disc.json().await?;
    assert_eq!(disc_json["resource"], "https://proxima.e2e.test/mcp");
    assert_eq!(disc_json["authorization_servers"][0], ISSUER);

    // 2. /mcp without a bearer → 401 carrying WWW-Authenticate.
    let no_auth = client
        .post(&url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "e2e", "version": "0"}}
        }))
        .send()
        .await?;
    assert_eq!(no_auth.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert!(
        no_auth.headers().contains_key("WWW-Authenticate"),
        "401 must advertise WWW-Authenticate"
    );

    // 3. A valid JWT initializes and lists the Code-flavor tools.
    let bearer = format!("Bearer {}", mint(&enc, "operator-sub"));
    let session = initialize(&client, &url, &bearer, &owner_key).await?;
    initialized(&client, &url, &session, &bearer).await?;
    let body = post_rpc(
        &client,
        &url,
        Some(&session),
        &bearer,
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
    )
    .await?;
    let names: Vec<String> = body["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .filter_map(|tool| tool["name"].as_str().map(String::from))
        .collect();
    let code_tools: Vec<&String> = names
        .iter()
        .filter(|n| n.starts_with("proxima-code"))
        .collect();
    assert_eq!(
        code_tools.len(),
        10,
        "expected the 10 Code-flavor tools, got {}: {code_tools:?}",
        code_tools.len()
    );

    // 4. A Code-flavor tool is callable with the bearer (list_repos: empty ok).
    let list_repos = names
        .iter()
        .find(|n| n.contains("list_repos"))
        .expect("a list_repos tool");
    let call = post_rpc(
        &client,
        &url,
        Some(&session),
        &bearer,
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": list_repos, "arguments": {}}}),
    )
    .await?;
    assert!(
        call.get("result").is_some(),
        "list_repos call should succeed, got {call:?}"
    );

    running.shutdown().await;
    Ok(())
}

/// Default host-resolved group-auth path, end to end against real Postgres:
/// the validated `(iss, sub)` maps through an `OidcSubjectMap` to a
/// `UserId`, `PgOwnerAccessResolver` resolves its current
/// `proxima_core.group_memberships` row into `OwnerRoles`, and
/// `McpEdgeAuth::with_host` narrows that to the deployment-configured Group
/// owner — proving a real Editor row (seeded after boot, matching a
/// production membership grant) permits a scoped tool call.
#[tokio::test]
async fn oidc_e2e_group_auth_host_resolved_editor_role_permits_tool_call()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(database_url) = require_env_or_skip("PROXIMA_TEST_DATABASE_URL") else {
        eprintln!("skipping oidc_e2e_group_auth: PROXIMA_TEST_DATABASE_URL not set");
        return Ok(());
    };

    let group_owner: Owner = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
    let subject = UserId::new(Uuid::now_v7());
    let (enc, resolver) = keypair();

    let mut subject_map = OidcSubjectMap::new();
    subject_map.insert(ISSUER, "group-member-sub", subject)?;
    let owner_access: Arc<dyn OwnerAccessPort> =
        Arc::new(PgOwnerAccessResolver::connect_lazy(&database_url)?);

    let authn = OidcAuthenticator::new(
        OidcAuthConfig {
            issuer: ISSUER.to_string(),
            jwks_uri: None,
            audience: AUDIENCE.to_string(),
            allowed_subjects: None,
            leeway_secs: 60,
        },
        Arc::new(resolver),
        subject_map,
        owner_access.clone(),
    )?;

    let running = Proxima::<ProximaMcpApp>::app()
        .database_url(database_url.clone())
        .owner(group_owner)
        .owner_access(owner_access)
        .authenticator(Arc::new(authn))
        .resource_metadata(ResourceServerMetadata {
            public_url: "https://proxima.e2e.test".to_string(),
            authorization_servers: vec![ISSUER.to_string()],
        })
        .mcp_bind("127.0.0.1:0".parse().unwrap())
        .run()
        .await?;

    // Seed the Editor membership row after boot (migrations have run by
    // now), matching how a production grant lands: a row appears, no
    // redeploy needed.
    let OwnerRef::Group(group_id) = group_owner else {
        unreachable!("group_owner is always Group")
    };
    let storage = PgStorage::connect(&database_url).await?;
    let permit_engine = Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests());
    let permit_authz = AuthzContext::for_subject_with_role(
        UserId::new(Uuid::now_v7()),
        [(group_owner, Role::admin())],
        AuthPath::HostBearer,
    );
    let permit = permit_engine
        .authorize_owner_write(&permit_authz, &group_owner, AccessKind::Goal)
        .await?;
    storage
        .add_group_member(&permit, group_id, subject, Relation::Editor, Uuid::now_v7())
        .await?;

    let addr = running.mcp_addr.ok_or("missing MCP listener address")?;
    let base = format!("http://{addr}");
    let url = format!("{base}/mcp");
    let client = reqwest::Client::new();

    let bearer = format!("Bearer {}", mint(&enc, "group-member-sub"));
    let session = initialize(&client, &url, &bearer, &owner_header(group_owner)).await?;
    initialized(&client, &url, &session, &bearer).await?;
    let body = post_rpc(
        &client,
        &url,
        Some(&session),
        &bearer,
        json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": {"name": "core_search_memories", "arguments": {"query": "anything"}}
        }),
    )
    .await?;
    assert!(
        body.get("result").is_some(),
        "host-resolved Editor role on the configured group owner should permit a scoped tool call, got {body:?}"
    );

    running.shutdown().await;
    Ok(())
}

async fn initialize(
    client: &reqwest::Client,
    url: &str,
    bearer: &str,
    owner_key: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let response = client
        .post(url)
        .header("Origin", "http://localhost")
        .header("Authorization", bearer)
        .header("X-Proxima-Owner", owner_key)
        .header("MCP-Protocol-Version", "2025-03-26")
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "e2e", "version": "0"}}
        }))
        .send()
        .await?;
    assert!(response.status().is_success(), "{}", response.status());
    let session_id = response
        .headers()
        .get("Mcp-Session-Id")
        .ok_or("missing session id")?
        .to_str()?
        .to_string();
    let _ = sse_json(response).await?;
    Ok(session_id)
}

fn owner_header(owner: Owner) -> String {
    owner.external_key()
}

async fn initialized(
    client: &reqwest::Client,
    url: &str,
    session_id: &str,
    bearer: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let response = client
        .post(url)
        .header("Origin", "http://localhost")
        .header("Authorization", bearer)
        .header("MCP-Protocol-Version", "2025-03-26")
        .header("Mcp-Session-Id", session_id)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .json(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
        .send()
        .await?;
    assert!(response.status().is_success(), "{}", response.status());
    Ok(())
}

async fn post_rpc(
    client: &reqwest::Client,
    url: &str,
    session_id: Option<&str>,
    bearer: &str,
    body: serde_json::Value,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let mut request = client
        .post(url)
        .header("Origin", "http://localhost")
        .header("Authorization", bearer)
        .header("MCP-Protocol-Version", "2025-03-26")
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .json(&body);
    if let Some(session_id) = session_id {
        request = request.header("Mcp-Session-Id", session_id);
    }
    let response = request.send().await?;
    assert!(response.status().is_success(), "{}", response.status());
    sse_json(response).await
}

async fn sse_json(
    response: reqwest::Response,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let text = response.text().await?;
    for data in text.lines().filter_map(|line| line.strip_prefix("data:")) {
        let data = data.trim();
        if data.is_empty() {
            continue;
        }
        if let Ok(value) = serde_json::from_str(data) {
            return Ok(value);
        }
    }
    Err(format!("missing JSON SSE data in {text:?}").into())
}

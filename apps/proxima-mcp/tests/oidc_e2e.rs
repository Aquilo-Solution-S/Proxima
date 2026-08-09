//! End-to-end: boot the facade with an OIDC authenticator + resource metadata
//! against a real Postgres, then prove the security surface over HTTP:
//! discovery is public, `/mcp` is 401+WWW-Authenticate without a bearer, and a
//! valid Zitadel-shaped JWT lists + calls the Code-flavor tools.
//!
//! Requires `PROXIMA_TEST_DATABASE_URL`; skips cleanly otherwise. Built with
//! `--features code` (asserts the Code flavor's tools are present); the mounted
//! REST smoke additionally requires `rest` and `PROXIMA_REST_ENABLED=true`.
#![cfg(feature = "code")]

mod common;

#[cfg(feature = "rest")]
use std::collections::BTreeSet;
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
    OwnerRef, Relation, Role, ToolScope, UserId,
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
        owner_access,
    )?;

    let running = Proxima::<ProximaMcpApp>::app()
        .tool_scope(ToolScope::All)
        .database_url(database_url)
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
    // The public origin, not a per-surface path. One identifier is one
    // audience, so a single token reaches both `/mcp` and `/v1`; a
    // path-suffixed identifier would mint non-interchangeable tokens per
    // surface (17 §Protected-resource identifier).
    assert_eq!(disc_json["resource"], "https://proxima.e2e.test");
    assert!(
        !disc_json["resource"]
            .as_str()
            .expect("resource is a string")
            .ends_with("/mcp"),
        "the identifier must not be scoped to one surface: {disc_json}",
    );
    assert_eq!(disc_json["authorization_servers"][0], ISSUER);

    // Browser CORS is listener-wide, including anonymous discovery and an
    // unauthenticated preflight for the protected MCP route.
    let discovery_cors = client
        .get(format!("{base}/.well-known/oauth-protected-resource"))
        .header("Origin", "http://localhost:5173")
        .send()
        .await?;
    assert_eq!(discovery_cors.status(), reqwest::StatusCode::OK);
    assert_eq!(
        discovery_cors
            .headers()
            .get("Access-Control-Allow-Origin")
            .and_then(|value| value.to_str().ok()),
        Some("http://localhost:5173")
    );
    let preflight = client
        .request(reqwest::Method::OPTIONS, &url)
        .header("Origin", "http://localhost:5173")
        .header("Access-Control-Request-Method", "POST")
        .header(
            "Access-Control-Request-Headers",
            "authorization,content-type,x-proxima-owner",
        )
        .send()
        .await?;
    assert_eq!(preflight.status(), reqwest::StatusCode::NO_CONTENT);
    assert_eq!(
        preflight
            .headers()
            .get("Access-Control-Allow-Origin")
            .and_then(|value| value.to_str().ok()),
        Some("http://localhost:5173")
    );

    // Public metadata bypasses bearer auth, not listener-wide Host validation.
    let foreign_discovery = client
        .get(format!("{base}/.well-known/oauth-protected-resource"))
        .header("Host", "foreign.e2e.test")
        .send()
        .await?;
    assert_eq!(foreign_discovery.status(), reqwest::StatusCode::FORBIDDEN);
    assert!(
        !foreign_discovery.headers().contains_key("WWW-Authenticate"),
        "Host rejection must not be rendered as an authentication challenge"
    );
    assert_eq!(
        foreign_discovery.text().await?,
        "Forbidden: Host header is not allowed"
    );

    // The listener guard also covers the router fallback, not only mounted
    // protocol routes.
    let foreign_fallback = client
        .get(format!("{base}/not-a-real-route"))
        .header("Host", "foreign.e2e.test")
        .send()
        .await?;
    assert_eq!(foreign_fallback.status(), reqwest::StatusCode::FORBIDDEN);
    assert!(
        !foreign_fallback.headers().contains_key("WWW-Authenticate"),
        "Host rejection must run before the authenticated fallback"
    );
    assert_eq!(
        foreign_fallback.text().await?,
        "Forbidden: Host header is not allowed"
    );

    // Protected production routes reject a foreign Host before bearer auth.
    // If the layer order regresses, this no-bearer request becomes a 401.
    let foreign_mcp = client
        .post(&url)
        .header("Host", "foreign.e2e.test")
        .header("Origin", "http://localhost")
        .send()
        .await?;
    assert_eq!(foreign_mcp.status(), reqwest::StatusCode::FORBIDDEN);
    assert!(
        !foreign_mcp.headers().contains_key("WWW-Authenticate"),
        "Host rejection must run before bearer authentication"
    );
    assert_eq!(
        foreign_mcp.text().await?,
        "Forbidden: Host header is not allowed"
    );

    // 2. /mcp without a bearer → 401 carrying WWW-Authenticate.
    let no_auth = client
        .post(&url)
        .header("Origin", "http://localhost:5173")
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
    assert_eq!(
        no_auth
            .headers()
            .get("Access-Control-Allow-Origin")
            .and_then(|value| value.to_str().ok()),
        Some("http://localhost:5173")
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
        11,
        "expected the 11 Code-flavor tools, got {}: {code_tools:?}",
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

/// Boots the production facade with both transport projections enabled, then
/// proves the deployment palette is identical at the authenticated MCP and
/// `OpenAPI` surfaces. `from_env()` is deliberate: CI must exercise the
/// `PROXIMA_REST_ENABLED` deployment gate, not a test-only router.
#[cfg(feature = "rest")]
#[tokio::test]
#[allow(clippy::too_many_lines)] // linear e2e: auth + two surfaces read best in one flow
async fn oidc_e2e_rest_openapi_matches_the_mcp_scope_on_the_mounted_runtime()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(database_url) = require_env_or_skip("PROXIMA_TEST_DATABASE_URL") else {
        eprintln!("skipping REST OIDC e2e: PROXIMA_TEST_DATABASE_URL not set");
        return Ok(());
    };
    let Some(rest_enabled) = require_env_or_skip("PROXIMA_REST_ENABLED") else {
        eprintln!("skipping REST OIDC e2e: PROXIMA_REST_ENABLED not set");
        return Ok(());
    };
    assert_eq!(
        rest_enabled, "true",
        "REST OIDC e2e requires PROXIMA_REST_ENABLED=true"
    );

    let subject = UserId::new(Uuid::now_v7());
    let owner: Owner = OwnerRef::Personal(subject);
    let owner_key = owner_header(owner);
    let (enc, resolver) = keypair();
    let mut subject_map = OidcSubjectMap::new();
    subject_map.insert(ISSUER, "rest-operator-sub", subject)?;
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
        owner_access,
    )?;

    let allowed_tools = BTreeSet::from([
        "core_search_memories".to_string(),
        "proxima-code_list_repos".to_string(),
    ]);
    let running = Proxima::<ProximaMcpApp>::app()
        .from_env()
        .tool_scope(ToolScope::Palette(allowed_tools.iter().cloned().collect()))
        .database_url(database_url)
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
    let mcp_url = format!("{base}/mcp");
    let openapi_url = format!("{base}/v1/openapi.json");
    let client = reqwest::Client::new();
    let bearer = format!("Bearer {}", mint(&enc, "rest-operator-sub"));

    // The same mounted listener still serves MCP, narrowed to the deployment
    // palette rather than falling open when REST is enabled.
    let session = initialize(&client, &mcp_url, &bearer, &owner_key).await?;
    initialized(&client, &mcp_url, &session, &bearer).await?;
    let catalog = post_rpc(
        &client,
        &mcp_url,
        Some(&session),
        &bearer,
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
    )
    .await?;
    let mcp_tools: BTreeSet<String> = catalog["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .map(|tool| tool["name"].as_str().expect("tool name").to_string())
        .collect();
    assert_eq!(mcp_tools, allowed_tools);

    let no_auth = client
        .get(&openapi_url)
        .header("Origin", "http://localhost")
        .send()
        .await?;
    assert_eq!(no_auth.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert!(no_auth.headers().contains_key("WWW-Authenticate"));

    let response = client
        .get(&openapi_url)
        .header("Origin", "http://localhost")
        .header("Authorization", bearer.as_str())
        .header("X-Proxima-Owner", owner_key.as_str())
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("Cache-Control")
            .and_then(|value| value.to_str().ok()),
        Some("private, no-store")
    );
    let document: serde_json::Value = response.json().await?;
    assert_eq!(document["openapi"], "3.2.0");
    let rest_tool_paths: BTreeSet<String> = document["paths"]
        .as_object()
        .expect("OpenAPI paths")
        .keys()
        .filter(|path| path.starts_with("/v1/tools/"))
        .cloned()
        .collect();
    let expected_paths: BTreeSet<String> = allowed_tools
        .iter()
        .map(|tool| format!("/v1/tools/{tool}"))
        .collect();
    assert_eq!(rest_tool_paths, expected_paths);

    let list_repos_url = format!("{base}/v1/tools/proxima-code_list_repos");
    let foreign_rest = client
        .post(&list_repos_url)
        .header("Host", "foreign.e2e.test")
        .header("Origin", "http://localhost")
        .header("Authorization", bearer.as_str())
        .header("X-Proxima-Owner", owner_key.as_str())
        .json(&json!({}))
        .send()
        .await?;
    assert_eq!(foreign_rest.status(), reqwest::StatusCode::FORBIDDEN);
    assert_eq!(
        foreign_rest.text().await?,
        "Forbidden: Host header is not allowed"
    );

    // Invoke a real mounted REST tool, not only its generated OpenAPI route.
    let list_repos = client
        .post(&list_repos_url)
        .header("Origin", "http://localhost")
        .header("Authorization", bearer.as_str())
        .header("X-Proxima-Owner", owner_key.as_str())
        .json(&json!({}))
        .send()
        .await?;
    assert_eq!(list_repos.status(), reqwest::StatusCode::OK);
    assert_eq!(
        list_repos
            .headers()
            .get("Cache-Control")
            .and_then(|value| value.to_str().ok()),
        Some("private, no-store")
    );
    let list_repos_body: serde_json::Value = list_repos.json().await?;
    assert_eq!(list_repos_body["repos"], json!([]));
    assert_eq!(list_repos_body["has_more"], false);
    assert_eq!(list_repos_body["next_cursor"], serde_json::Value::Null);

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
        owner_access,
    )?;

    let running = Proxima::<ProximaMcpApp>::app()
        .tool_scope(ToolScope::All)
        .database_url(database_url.clone())
        .owner(group_owner)
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

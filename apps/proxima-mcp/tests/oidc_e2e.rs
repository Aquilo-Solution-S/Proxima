//! End-to-end: boot the facade with an OIDC authenticator + resource metadata
//! against a real Postgres, then prove the security surface over HTTP:
//! discovery is public, `/mcp` is 401+WWW-Authenticate without a bearer, and a
//! valid Zitadel-shaped JWT lists + calls the core tools.
//!
//! Each test boots on a pg-testkit database of its own (`create_db`). Built in
//! every feature set: it calls only core tools. The mounted REST smoke
//! additionally requires `rest` (mounted through `rest_enabled(true)`, not
//! process env).

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
use proxima::{Proxima, ResourceServerMetadata, S3RuntimeConfig};
use proxima_auth_oidc::{OidcAuthConfig, OidcAuthenticator, OidcSubjectMap, StaticJwksResolver};
#[cfg(feature = "rest")]
use proxima_core::ToolScope;
use proxima_core::storage_ports::{MembershipWritePermit, OwnerMembershipAdminPort};
use proxima_core::{
    AccessKind, AuthPath, AuthzContext, ColdObjectStore, Engine, FlavorRegistry, GroupId,
    OwnerAccessPort, OwnerId, Relation, Role, UserId,
};
use proxima_mcp::ProximaMcpApp;
use serde_json::json;
use uuid::Uuid;

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
async fn oidc_e2e_discovery_public_and_core_tools_behind_bearer()
-> Result<(), Box<dyn std::error::Error>> {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset");
        return Ok(());
    }
    let s3 = S3RuntimeConfig {
        force_path_style: true,
        ..S3RuntimeConfig::from_env()?
    };
    let (database_url, created_db) = live_database_url().await?;
    let (runtime_url, platform_url) = common::split_roles(&database_url).await?;

    let subject = UserId::new(Uuid::now_v7());
    let owner: OwnerId = OwnerId::from(subject);
    let owner_key = owner_header(owner);
    let (enc, resolver) = keypair();
    let mut subject_map = OidcSubjectMap::new();
    subject_map.insert(ISSUER, "operator-sub", subject)?;
    let owner_access: Arc<dyn OwnerAccessPort> =
        Arc::new(common::owner_access(&runtime_url, &platform_url).await?);
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
        .served_surface(proxima_core::ServedSurface::All)
        .database_url(runtime_url.clone())
        .store_number(0)
        .platform_database_url(platform_url.clone())
        .s3(s3)
        .authenticator(Arc::new(authn))
        .resource_metadata(ResourceServerMetadata {
            public_url: "https://proxima.e2e.test".to_string(),
            authorization_servers: vec![ISSUER.to_string()],
        })
        .mcp_bind("127.0.0.1:0".parse().unwrap())
        .run()
        .await?;
    let addr = running.mcp_addr().ok_or("missing MCP listener address")?;
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
    // The origin document names the origin (`/v1`, host routes); `/mcp` has
    // its own, path-exact, because an MCP client requires `resource` to equal
    // the URL it connected to (15 §Security guarantee).
    assert_eq!(disc_json["resource"], "https://proxima.e2e.test");
    assert_eq!(disc_json["authorization_servers"][0], ISSUER);
    let mcp_disc: serde_json::Value = client
        .get(format!("{base}/.well-known/oauth-protected-resource/mcp"))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(mcp_disc["resource"], "https://proxima.e2e.test/mcp");
    assert_eq!(mcp_disc["authorization_servers"][0], ISSUER);
    assert_eq!(
        mcp_disc["scopes_supported"],
        json!(["openid", "offline_access"])
    );

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
    assert_eq!(
        no_auth
            .headers()
            .get("WWW-Authenticate")
            .and_then(|value| value.to_str().ok()),
        Some(
            "Bearer resource_metadata=\"https://proxima.e2e.test/.well-known/oauth-protected-resource/mcp\""
        ),
        "a 401 on /mcp points at the /mcp document"
    );
    assert_eq!(
        no_auth
            .headers()
            .get("Access-Control-Allow-Origin")
            .and_then(|value| value.to_str().ok()),
        Some("http://localhost:5173")
    );

    // 3. A valid JWT initializes and lists the core tools.
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
    for core_tool in [
        "core_remember",
        "core_search_memories",
        "core_memory_spaces",
        "core_forget",
    ] {
        assert!(
            names.iter().any(|name| name == core_tool),
            "expected {core_tool} in the served catalog: {names:?}"
        );
    }

    // 4. A read-only core tool is callable with the bearer.
    let tool_call = |id: u64, name: &str, arguments: serde_json::Value| {
        json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
               "params": {"name": name, "arguments": arguments}})
    };
    let call = post_rpc(
        &client,
        &url,
        Some(&session),
        &bearer,
        tool_call(3, "core_memory_spaces", json!({})),
    )
    .await?;
    assert!(
        call.get("result").is_some(),
        "core_memory_spaces call should succeed, got {call:?}"
    );
    assert_ne!(call["result"]["isError"], json!(true), "{call:?}");

    // 5. A write under owner RLS is read back through the same bearer: the
    // remembered memory is found by a search scoped to the caller.
    let remembered = post_rpc(
        &client,
        &url,
        Some(&session),
        &bearer,
        tool_call(
            10,
            "core_remember",
            json!({"title": "e2e run marker", "body": "oidc e2e run marker", "tags": []}),
        ),
    )
    .await?;
    let remembered: serde_json::Value = serde_json::from_str(&mcp_text(&remembered))?;
    let marker = remembered["handle"]
        .as_str()
        .ok_or_else(|| format!("core_remember: {remembered}"))?
        .to_owned();
    let search = post_rpc(
        &client,
        &url,
        Some(&session),
        &bearer,
        tool_call(
            11,
            "core_search_memories",
            json!({"query": "marker", "mode": "lexical", "limit": 5}),
        ),
    )
    .await?;
    assert_ne!(search["result"]["isError"], json!(true), "{search}");
    let found: serde_json::Value = serde_json::from_str(&mcp_text(&search))?;
    assert!(
        found["memories"]
            .as_array()
            .is_some_and(|hits| hits.iter().any(|hit| hit["memory"] == marker.as_str())),
        "the remembered memory is read back: {found}"
    );

    assert!(
        names.iter().any(|name| name == "core_forget"),
        "served catalog must include core_forget: {names:?}"
    );
    assert!(
        names.iter().all(|name| !name.contains("open_batch")),
        "open_batch must stay deleted: {names:?}"
    );

    let remembered = post_rpc(
        &client,
        &url,
        Some(&session),
        &bearer,
        json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {
                "name": "core_remember",
                "arguments": {
                    "title": "e2e forget",
                    "body": "cool this row",
                    "tags": []
                }
            }
        }),
    )
    .await?;
    let remember_text = mcp_text(&remembered);
    let remember_json: serde_json::Value = serde_json::from_str(&remember_text)?;
    let handle = remember_json["handle"]
        .as_str()
        .or_else(|| remember_json["memory"].as_str())
        .ok_or_else(|| format!("remember missing handle: {remembered}"))?
        .to_string();
    let forgotten = post_rpc(
        &client,
        &url,
        Some(&session),
        &bearer,
        json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "tools/call",
            "params": {
                "name": "core_forget",
                "arguments": { "memory": handle }
            }
        }),
    )
    .await?;
    assert!(
        forgotten.get("result").is_some(),
        "core_forget must run on the live listener: {forgotten:?}"
    );
    assert_ne!(
        forgotten["result"]["isError"],
        json!(true),
        "a JSON-RPC result must not conceal a tool failure: {forgotten:?}"
    );
    let forgotten_output: serde_json::Value = serde_json::from_str(&mcp_text(&forgotten))?;
    assert_eq!(forgotten_output["ok"], json!(true));
    let memory_t = handle.parse::<Uuid>()?;
    let cold = running
        .host()
        .blobs()
        .as_ref()
        .expect("configured S3 fixture")
        .cold_store();
    let cold_key = proxima_core::cold_object_key(memory_t);
    assert!(!cold.get(&cold_key).await?.is_empty());
    cold.delete(&cold_key).await?;

    running.shutdown().await;
    if let Some(name) = created_db {
        let _ = proxima_pg_testkit::drop_db(&name).await;
    }
    Ok(())
}

/// Boots the production facade with both transport projections enabled, then
/// proves the deployment palette is identical at the authenticated MCP and
/// `OpenAPI` surfaces. `from_env()` still reads deployment config;
/// `rest_enabled(true)` mounts `/v1` without mutating process env.
#[cfg(feature = "rest")]
#[tokio::test]
#[allow(clippy::too_many_lines)] // linear e2e: auth + two surfaces read best in one flow
async fn oidc_e2e_rest_openapi_matches_the_mcp_scope_on_the_mounted_runtime()
-> Result<(), Box<dyn std::error::Error>> {
    #[derive(Debug)]
    struct CallerScopeBySubject;
    impl proxima_auth_oidc::OidcRoleShaper for CallerScopeBySubject {
        fn shape(
            &self,
            context: AuthzContext,
            token: &proxima_auth_oidc::ValidatedOidcToken<proxima_auth_oidc::OidcClaimMap>,
        ) -> Result<AuthzContext, proxima_core::AuthError> {
            let names: &[&str] = match token.claims.subject.as_str() {
                "rest-operator-sub" => return Ok(context),
                "exact-command-sub" => &["core_memory_spaces"],
                "exact-reads-sub" => &[
                    "core_memory_spaces",
                    "core_search_memories",
                    "resource:memory",
                ],
                _ => return Err(proxima_core::AuthError::InvalidCredentials),
            };
            Ok(context.with_tool_scope(ToolScope::Palette(
                names
                    .iter()
                    .map(|name| proxima_core::ScopeKey::parse(name).unwrap())
                    .collect(),
            )))
        }
    }

    let (database_url, created_db) = live_database_url().await?;
    let (runtime_url, platform_url) = common::split_roles(&database_url).await?;

    let subject = UserId::new(Uuid::now_v7());
    let owner: OwnerId = OwnerId::from(subject);
    let owner_key = owner_header(owner);
    let (enc, resolver) = keypair();
    let mut subject_map = OidcSubjectMap::new();
    for name in ["rest-operator-sub", "exact-command-sub", "exact-reads-sub"] {
        subject_map.insert(ISSUER, name, subject)?;
    }
    let owner_access: Arc<dyn OwnerAccessPort> =
        Arc::new(common::owner_access(&runtime_url, &platform_url).await?);
    let binding = proxima_auth_oidc::OidcBinding::with_role_shape(
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
        proxima_auth_oidc::OidcRoleShape::Host(Arc::new(CallerScopeBySubject)),
    )?;
    let authn = proxima_auth_oidc::OidcBindingSet::new([binding])?;

    let allowed_tools = BTreeSet::from([
        "core_remember".to_string(),
        "core_memory_spaces".to_string(),
    ]);
    let running = Proxima::<ProximaMcpApp>::app()
        .from_env()
        .rest_enabled(true)
        .served_surface(proxima_core::ServedSurface::Palette(
            allowed_tools
                .iter()
                .map(|tool| {
                    proxima_core::ScopeKey::parse(tool).expect("an allowed tool is a scope key")
                })
                .collect(),
        ))
        .database_url(runtime_url.clone())
        .store_number(0)
        .platform_database_url(platform_url.clone())
        .authenticator(Arc::new(authn))
        .resource_metadata(ResourceServerMetadata {
            public_url: "https://proxima.e2e.test".to_string(),
            authorization_servers: vec![ISSUER.to_string()],
        })
        .mcp_bind("127.0.0.1:0".parse().unwrap())
        .run()
        .await?;
    let addr = running.mcp_addr().ok_or("missing MCP listener address")?;
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
    // The palette's own entries, plus every read (admitted under every
    // palette, #462), and no write the palette does not hold.
    assert!(mcp_tools.is_superset(&allowed_tools), "{mcp_tools:?}");
    assert!(mcp_tools.contains("core_search_memories"), "{mcp_tools:?}");
    assert!(!mcp_tools.contains("core_forget"), "{mcp_tools:?}");

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
    assert_eq!(document["info"]["version"], proxima_core::RELEASE_VERSION);
    let paths = document["paths"].as_object().expect("OpenAPI paths");
    let rest_tool_paths: BTreeSet<String> = paths
        .keys()
        .filter(|path| {
            path.strip_prefix("/v1/tools/")
                .is_some_and(|rest| !rest.contains('/'))
        })
        .cloned()
        .collect();
    let expected_paths: BTreeSet<String> = mcp_tools
        .iter()
        .map(|tool| format!("/v1/tools/{tool}"))
        .collect();
    assert_eq!(rest_tool_paths, expected_paths);
    // A dispatcher's per-action routes: the palette holds no action leaf, so
    // each is a read every palette admits (#462), of a tool MCP lists, whose
    // MCP schema offers that action.
    let mcp_schemas: std::collections::BTreeMap<&str, String> = catalog["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .map(|tool| {
            (
                tool["name"].as_str().expect("tool name"),
                tool["inputSchema"].to_string(),
            )
        })
        .collect();
    let mut action_routes = 0;
    for (path, item) in paths {
        let Some((tool, action)) = path
            .strip_prefix("/v1/tools/")
            .and_then(|rest| rest.split_once('/'))
        else {
            continue;
        };
        action_routes += 1;
        let schema = mcp_schemas
            .get(tool)
            .unwrap_or_else(|| panic!("{path}: {tool} is not listed over MCP"));
        assert!(
            schema.contains(&format!("\"{action}\"")),
            "{path}: MCP does not offer {action}: {schema}"
        );
        assert_eq!(
            item.pointer("/post/x-proxima-read-only"),
            Some(&json!(true)),
            "{path} is a read"
        );
    }
    assert!(
        action_routes > 0,
        "a mixed dispatcher is listed for its reads"
    );

    let memory_spaces_url = format!("{base}/v1/tools/core_memory_spaces");
    let foreign_rest = client
        .post(&memory_spaces_url)
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
    let memory_spaces = client
        .post(&memory_spaces_url)
        .header("Origin", "http://localhost")
        .header("Authorization", bearer.as_str())
        .header("X-Proxima-Owner", owner_key.as_str())
        .json(&json!({}))
        .send()
        .await?;
    assert_eq!(memory_spaces.status(), reqwest::StatusCode::OK);
    assert_eq!(
        memory_spaces
            .headers()
            .get("Cache-Control")
            .and_then(|value| value.to_str().ok()),
        Some("private, no-store")
    );
    let memory_spaces_body: serde_json::Value = memory_spaces.json().await?;
    assert!(
        memory_spaces_body["spaces"]
            .as_array()
            .is_some_and(|spaces| !spaces.is_empty()),
        "the caller sees at least its own space: {memory_spaces_body}"
    );

    // #489: the same mounted deployment admits only each caller's exact keys.
    let remembered = post_rpc(
        &client,
        &mcp_url,
        Some(&session),
        &bearer,
        json!({
            "jsonrpc":"2.0","id":20,"method":"tools/call","params":{
                "name":"core_remember","arguments":{
                    "title":"Scoped memory", "body":"Exact caller scope observation",
                    "idempotency_key":format!("exact-scope-{}", Uuid::now_v7())
                }
            }
        }),
    )
    .await?;
    let remembered: serde_json::Value = serde_json::from_str(&mcp_text(&remembered))?;
    let memory = remembered["handle"]
        .as_str()
        .ok_or("missing memory handle")?;
    for (name, reads) in [("exact-command-sub", false), ("exact-reads-sub", true)] {
        let scoped_bearer = format!("Bearer {}", mint(&enc, name));
        let scoped_session = initialize(&client, &mcp_url, &scoped_bearer, &owner_key).await?;
        initialized(&client, &mcp_url, &scoped_session, &scoped_bearer).await?;
        let listed = post_rpc(
            &client,
            &mcp_url,
            Some(&scoped_session),
            &scoped_bearer,
            json!({"jsonrpc":"2.0","id":21,"method":"tools/list","params":{}}),
        )
        .await?;
        let names: BTreeSet<&str> = listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        let mut expected = BTreeSet::from(["core_memory_spaces"]);
        if reads {
            expected.insert("core_search_memories");
        }
        assert_eq!(names, expected);
        let resources = post_rpc(
            &client,
            &mcp_url,
            Some(&scoped_session),
            &scoped_bearer,
            json!({"jsonrpc":"2.0","id":22,"method":"resources/list","params":{}}),
        )
        .await?;
        assert_eq!(resources["result"]["resources"], json!([]));
        let templates = post_rpc(
            &client,
            &mcp_url,
            Some(&scoped_session),
            &scoped_bearer,
            json!({"jsonrpc":"2.0","id":23,"method":"resources/templates/list","params":{}}),
        )
        .await?;
        let templates = templates["result"]["resourceTemplates"].as_array().unwrap();
        assert_eq!(templates.len(), usize::from(reads));
        if reads {
            assert!(
                templates[0]["uriTemplate"]
                    .as_str()
                    .unwrap()
                    .starts_with("proxima://memory/")
            );
        }
        let response = client
            .get(&openapi_url)
            .header("Origin", "http://localhost")
            .header("Authorization", &scoped_bearer)
            .header("X-Proxima-Owner", &owner_key)
            .send()
            .await?;
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let spec: serde_json::Value = response.json().await?;
        let paths = spec["paths"].as_object().unwrap();
        let tool_paths: BTreeSet<String> = paths
            .keys()
            .filter(|path| {
                path.strip_prefix("/v1/tools/")
                    .is_some_and(|rest| !rest.contains('/'))
            })
            .cloned()
            .collect();
        assert_eq!(
            tool_paths,
            expected
                .iter()
                .map(|tool| format!("/v1/tools/{tool}"))
                .collect()
        );
        assert!(!paths.contains_key("/v1/how-to"));
        let resource_paths: Vec<_> = paths
            .keys()
            .filter(|path| path.starts_with("/v1/resources/"))
            .collect();
        assert_eq!(resource_paths.len(), usize::from(reads));
        let search = post_rpc(&client, &mcp_url, Some(&scoped_session), &scoped_bearer,
            json!({"jsonrpc":"2.0","id":24,"method":"tools/call","params":{
                "name":"core_search_memories","arguments":{"query":"Scoped","mode":"lexical","limit":5}
            }})).await?;
        let resource = post_rpc(
            &client,
            &mcp_url,
            Some(&scoped_session),
            &scoped_bearer,
            json!({"jsonrpc":"2.0","id":25,"method":"resources/read","params":{
                "uri":format!("proxima://memory/{memory}")
            }}),
        )
        .await?;
        if reads {
            assert!(search.get("error").is_none(), "{search}");
            assert_ne!(search["result"]["isError"], true, "{search}");
            let found: serde_json::Value = serde_json::from_str(&mcp_text(&search))?;
            assert!(
                found["memories"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|hit| hit["memory"] == memory)
            );
            let read: serde_json::Value = serde_json::from_str(
                resource["result"]["contents"][0]["text"]
                    .as_str()
                    .ok_or("missing memory content")?,
            )?;
            assert_eq!(read["memory"], memory);
        } else {
            assert_eq!(search["error"]["code"], -32600, "{search}");
            assert_eq!(resource["error"]["code"], -32600, "{resource}");
        }
        let spaces = client
            .post(format!("{base}/v1/tools/core_memory_spaces"))
            .header("Origin", "http://localhost")
            .header("Authorization", &scoped_bearer)
            .header("X-Proxima-Owner", &owner_key)
            .json(&json!({}))
            .send()
            .await?;
        assert_eq!(spaces.status(), reqwest::StatusCode::OK);
        let search = client
            .post(format!("{base}/v1/tools/core_search_memories"))
            .header("Origin", "http://localhost")
            .header("Authorization", &scoped_bearer)
            .header("X-Proxima-Owner", &owner_key)
            .json(&json!({"query":"Scoped","mode":"lexical","limit":5}))
            .send()
            .await?;
        let resource = client
            .get(format!("{base}/v1/resources/memory/{memory}"))
            .header("Origin", "http://localhost")
            .header("Authorization", &scoped_bearer)
            .header("X-Proxima-Owner", &owner_key)
            .send()
            .await?;
        let expected_status = if reads {
            reqwest::StatusCode::OK
        } else {
            reqwest::StatusCode::FORBIDDEN
        };
        assert_eq!(search.status(), expected_status);
        assert_eq!(resource.status(), expected_status);
        if reads {
            let resource: serde_json::Value = resource.json().await?;
            assert_eq!(resource["memory"], memory);
        }
        let guide = client
            .get(format!("{base}/v1/how-to"))
            .header("Origin", "http://localhost")
            .header("Authorization", &scoped_bearer)
            .header("X-Proxima-Owner", &owner_key)
            .send()
            .await?;
        assert_eq!(guide.status(), reqwest::StatusCode::FORBIDDEN);
    }

    running.shutdown().await;
    if let Some(name) = created_db {
        let _ = proxima_pg_testkit::drop_db(&name).await;
    }
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
    let (database_url, created_db) = live_database_url().await?;
    let (runtime_url, platform_url) = common::split_roles(&database_url).await?;

    let group_owner: OwnerId = OwnerId::from(GroupId::new(Uuid::now_v7()));
    let subject = UserId::new(Uuid::now_v7());
    let (enc, resolver) = keypair();

    let mut subject_map = OidcSubjectMap::new();
    subject_map.insert(ISSUER, "group-member-sub", subject)?;
    let owner_access: Arc<dyn OwnerAccessPort> =
        Arc::new(common::owner_access(&runtime_url, &platform_url).await?);

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
        .served_surface(proxima_core::ServedSurface::All)
        .database_url(runtime_url.clone())
        .store_number(0)
        .platform_database_url(platform_url.clone())
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
    let group_id = group_owner.as_group();
    let storage = common::runtime_storage(&runtime_url, &platform_url).await?;
    let permit_engine = Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests());
    let permit_authz =
        proxima_core::test_fixtures::authenticated_context(AuthzContext::for_subject_with_role(
            UserId::new(Uuid::now_v7()),
            [(group_owner, Role::admin())],
            AuthPath::HostBearer,
        ));
    let permit = permit_engine
        .authorize_owner_write(&permit_authz, &group_owner, AccessKind::Goal)
        .await?;
    storage
        .add_group_member(
            &MembershipWritePermit::new_for_tests(&permit, None, None, None),
            group_id,
            subject.into(),
            Relation::Editor,
            Uuid::now_v7(),
        )
        .await?;

    let addr = running.mcp_addr().ok_or("missing MCP listener address")?;
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
    if let Some(name) = created_db {
        let _ = proxima_pg_testkit::drop_db(&name).await;
    }
    Ok(())
}

/// A pg-testkit database of the test's own, never a shared one: a build
/// that linked a flavor migrates its schema into whatever database it boots
/// on, and a host that links none then refuses that database at boot.
async fn live_database_url() -> Result<(String, Option<String>), Box<dyn std::error::Error>> {
    let name = format!("proxima_oidc_e2e_{}", Uuid::now_v7().simple());
    proxima_pg_testkit::create_db(&name)
        .await
        .map_err(|err| format!("PG required for honest OIDC e2e: {err}"))?;
    Ok((proxima_pg_testkit::db_url(&name), Some(name)))
}

fn mcp_text(body: &serde_json::Value) -> String {
    if let Some(text) = body["result"]["structuredContent"].as_object() {
        return serde_json::to_string(text).unwrap_or_default();
    }
    body["result"]["content"]
        .as_array()
        .and_then(|items| items.first())
        .and_then(|item| item["text"].as_str())
        .unwrap_or_default()
        .to_string()
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
    let result = sse_json(response).await?;
    assert_eq!(
        result["result"]["serverInfo"]["version"],
        proxima_core::RELEASE_VERSION
    );
    Ok(session_id)
}

fn owner_header(owner: OwnerId) -> String {
    owner.to_string()
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

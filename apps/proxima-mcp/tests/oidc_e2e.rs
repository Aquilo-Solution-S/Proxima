//! End-to-end: boot the facade with an OIDC authenticator + resource metadata
//! against a real Postgres, then prove the security surface over HTTP:
//! discovery is public, `/mcp` is 401+WWW-Authenticate without a bearer, and a
//! valid Zitadel-shaped JWT lists + calls the Code-flavor tools.
//!
//! Requires `PROXIMA_TEST_DATABASE_URL`; skips cleanly otherwise. Built only
//! with `--features code` (asserts the Code flavor's tools are present).
#![cfg(feature = "code")]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use jsonwebtoken::{DecodingKey, EncodingKey, Header, encode};
use proxima::{Proxima, ResourceServerMetadata};
use proxima_auth_oidc::{OidcAuthConfig, OidcAuthenticator, StaticJwksResolver};
use proxima_core::{Owner, OwnerRef, UserId};
use proxima_mcp::ProximaMcpApp;
use serde_json::json;
use uuid::Uuid;

const KID: &str = "e2e-key";
const ISSUER: &str = "https://idp.e2e.test";
const AUDIENCE: &str = "proxima-mcp";

// Static 2048-bit RSA test keypair. Baked so the e2e test signs RS256 tokens
// via jsonwebtoken (ring/aws-lc) WITHOUT the `rsa`/`rand` crates
// (RUSTSEC-2023-0071: the `rsa` crate ships an unfixed Marvin timing
// sidechannel). The public half (JWK n/e) feeds the StaticJwksResolver; the
// private PEM signs. Test-only material, never a real credential.
const TEST_RSA_PRIVATE_KEY_PEM: &str = "-----BEGIN RSA PRIVATE KEY-----
MIIEpAIBAAKCAQEAvcvNMtDvpJExXOyytyqUOWhX2sxa+Xtxd4KmfJ05+iPgT/Ri
yZzx3UoTuJYtvDCCRcXKU13Rn8cIc0ushWlKpLDW08U4r9bBVctcajpnOumCcuIv
nM1/HEiM+WuYPRFk0I5h++ueLA0KhIfPs0ORLpqsvF0XIuL6/uZtObrH9wxPMmG4
r5Hh7h3Gm5PchY0R8H7VrEOm79fnra7OGg5nh7XkmStnZnwozODW0FFnpW+kMeCK
2+2fzmSWg1A/clFdicji1+xIvk7Wog9CVsZZK9iRHgAIxmsU+Iawb/Wwlwuu+/gI
ZWFkund24iA2qLktFx/39CORZqfFRNiIsHSvIQIDAQABAoIBAA//Yahq3AgvBM4k
VVwDBsNf/CfBGdn1gbblGEtgpUZkR7/1hW4hAHH6kHb6kZhPLmvbJBaqzcR97kRp
mH0WRuhiz3jCIukPXPRyU7PQgGsCy7ALSKAa4h/sLZXIb+iV0r2RgsjNL2PfJYfO
Or+NbmtTNkQaRJz4LNfXbFV1XO2Bwqw+swuIKFokhTJk/rF+PGn4yk5n38uQTwtO
sZiq+C2aI8Hw8LZKCoGmioGrstT29ts4yfX2rQKPqowl2kJ2hLCKoBwl+h5WJwkA
ECiOnNgflonp046T1bZQ9hIxnuw1y6Iq6SYJ5W74rsVLEzvvHPQAAWH1BQIG51gG
MFW+CcECgYEA+N4a37q8G4c+QOWexsqnleYErsM1RRhnpRO8reVJMJa8mEGMr0eN
1SAjEDL4/jQ2Pb2GlrsDi8x9MnyWo3VQDSyLLINrWyE+AiQuJjludPBL1B3RSw9t
yTAaSQjcCgP6IIvz8W7gbKPadzICqpzr+iKsbZUYkEKMW6Sk47kmEIUCgYEAwzxM
z/SAUs7TsGWW3XJfXxz1aX8bcRF0lpfTf1T/wNUPet37p0mf4JKOILIwuAJ5LUIh
uxduS+7HdIh3sLxGIGVt4QXPDO1R1RZMM6DUZa51/9nLIK9q1ocMXOApufQrs4OM
L2NcqRRKbVZPM9rBG9MKL0MgL1fUiV/OagVJFO0CgYA/A23mjE+o4LugjwN+7j00
tUMmRQMt9Zn4sGCr30yC4wfpvV8z2nhNKI/4QA/PvcSmKWD0tXGWajahG+7AgKm+
TDMJGFWMg4RB4otU3mHbdiSdFtexm7x+npFpQLcGSi+BIi6oSRzGJU7hs2X9cTJG
6ZSjQocvr8n+QlgF2RGMSQKBgQCe2xiw+IPVXR7X38FCjEZXsMtqvJbKiGZyBjV7
3OCAuZvv4GFcO8bPxs/IgNStVK3einnBro37UN2Pz158OqVgxMcEGmLfZNZ56Lu2
In3QAoVW2ZKzFKh8x8Piai7pdGh+l2HgSRvjI3RvxJOLYMpR5oTZ8edlPjTcVk0w
7P4K/QKBgQCPUzg3NonDO1t6R/MFsIJDIPR24VPorF8471hL1lANlrx1RrNn9/WY
2i6DbqVf3ZP43iXLWEIMcn6FzrhjJPfkcNVXvU+TWm594cbPGYrzBP1ONaA6okmf
hWfFqOK7kd53G/fwyOu4usJWEGojv1ey6Sn9X1myw/jG5XNE9yLrbA==
-----END RSA PRIVATE KEY-----
";
const TEST_JWK_N: &str = "vcvNMtDvpJExXOyytyqUOWhX2sxa-Xtxd4KmfJ05-iPgT_RiyZzx3UoTuJYtvDCCRcXKU13Rn8cIc0ushWlKpLDW08U4r9bBVctcajpnOumCcuIvnM1_HEiM-WuYPRFk0I5h--ueLA0KhIfPs0ORLpqsvF0XIuL6_uZtObrH9wxPMmG4r5Hh7h3Gm5PchY0R8H7VrEOm79fnra7OGg5nh7XkmStnZnwozODW0FFnpW-kMeCK2-2fzmSWg1A_clFdicji1-xIvk7Wog9CVsZZK9iRHgAIxmsU-Iawb_Wwlwuu-_gIZWFkund24iA2qLktFx_39CORZqfFRNiIsHSvIQ";
const TEST_JWK_E: &str = "AQAB";

fn keypair() -> (EncodingKey, StaticJwksResolver) {
    let enc = EncodingKey::from_rsa_pem(TEST_RSA_PRIVATE_KEY_PEM.as_bytes()).expect("encoding key");
    let dec = DecodingKey::from_rsa_components(TEST_JWK_N, TEST_JWK_E).expect("decoding key");
    let mut keys = HashMap::new();
    keys.insert(KID.to_string(), Arc::new(dec));
    (enc, StaticJwksResolver::new(keys))
}

fn mint(enc: &EncodingKey, sub: &str) -> String {
    let exp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 3600;
    let mut header = Header::new(jsonwebtoken::Algorithm::RS256);
    header.kid = Some(KID.to_string());
    encode(
        &header,
        &json!({"iss": ISSUER, "aud": AUDIENCE, "sub": sub, "exp": exp}),
        enc,
    )
    .expect("mint jwt")
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // linear e2e: boot + 4 assertion phases read best in one flow
async fn oidc_e2e_discovery_public_and_code_tools_behind_bearer()
-> Result<(), Box<dyn std::error::Error>> {
    let Ok(database_url) = std::env::var("PROXIMA_TEST_DATABASE_URL") else {
        eprintln!("skipping oidc_e2e: PROXIMA_TEST_DATABASE_URL not set");
        return Ok(());
    };

    let owner: Owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    let (enc, resolver) = keypair();
    let authn = OidcAuthenticator::new(
        OidcAuthConfig {
            issuer: ISSUER.to_string(),
            jwks_uri: None,
            audience: AUDIENCE.to_string(),
            owner,
            allowed_subjects: None,
            leeway_secs: 60,
        },
        Arc::new(resolver),
    )?;

    let running = Proxima::<ProximaMcpApp>::app()
        .database_url(database_url)
        .owner(owner)
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
    let session = initialize(&client, &url, &bearer).await?;
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

async fn initialize(
    client: &reqwest::Client,
    url: &str,
    bearer: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let response = client
        .post(url)
        .header("Origin", "http://localhost")
        .header("Authorization", bearer)
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

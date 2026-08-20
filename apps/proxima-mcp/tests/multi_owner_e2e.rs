//! End-to-end multi-owner MCP serving against real Postgres.
//!
//! Requires `PROXIMA_TEST_DATABASE_URL`; skips cleanly otherwise. Built when
//! the `code` feature is on (the host default) so the production
//! `ProximaMcpApp` bundle is exercised.
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
use proxima_core::storage_ports::{OwnerMembershipAdminPort, OwnerWritePermit};
use proxima_core::{
    AccessKind, AuthPath, AuthzContext, Engine, FlavorRegistry, GroupId, Owner, OwnerAccessPort,
    OwnerRef, Relation, Role, ToolScope, UserId,
};
use proxima_mcp::ProximaMcpApp;
use proxima_storage_pg::{PgOwnerAccessResolver, PgStorage};
use serde_json::json;
use uuid::Uuid;

use common::require_env_or_skip;

const KID: &str = "multi-owner-key";
const ISSUER: &str = "https://idp.multi-owner.test";
const AUDIENCE: &str = "proxima-mcp";

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn multi_owner_sessions_bind_owner_palette_and_revocation()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(database_url) = require_env_or_skip("PROXIMA_TEST_DATABASE_URL") else {
        eprintln!("skipping multi_owner_e2e: PROXIMA_TEST_DATABASE_URL not set");
        return Ok(());
    };

    let subject_a = UserId::new(Uuid::now_v7());
    let subject_b = UserId::new(Uuid::now_v7());
    let group_a = GroupId::new(Uuid::now_v7());
    let group_b = GroupId::new(Uuid::now_v7());
    let owner_a = OwnerRef::Group(group_a);
    let owner_b = OwnerRef::Group(group_b);
    let owner_access: Arc<dyn OwnerAccessPort> =
        Arc::new(PgOwnerAccessResolver::connect_lazy(&database_url)?);

    let (signing, resolver) = keypair();
    let mut subject_map = OidcSubjectMap::new();
    subject_map.insert(ISSUER, "subject-a", subject_a)?;
    subject_map.insert(ISSUER, "subject-b", subject_b)?;
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
        .authenticator(Arc::new(authn))
        .resource_metadata(ResourceServerMetadata {
            public_url: "https://proxima.multi-owner.test".to_string(),
            authorization_servers: vec![ISSUER.to_string()],
        })
        .mcp_bind("127.0.0.1:0".parse().unwrap())
        .run()
        .await?;

    let storage = PgStorage::connect(&database_url).await?;
    grant_member(&storage, owner_a, group_a, subject_a, Relation::Admin).await?;
    grant_member(&storage, owner_b, group_b, subject_a, Relation::Viewer).await?;
    grant_member(&storage, owner_b, group_b, subject_b, Relation::Admin).await?;

    let addr = running.mcp_addr.ok_or("missing MCP listener address")?;
    let url = format!("http://{addr}/mcp");
    let client = reqwest::Client::new();
    let bearer_a = format!("Bearer {}", mint(&signing, "subject-a"));
    let bearer_b = format!("Bearer {}", mint(&signing, "subject-b"));
    let unique_query = format!("multi-owner isolated note {}", Uuid::now_v7());

    let session_a_owner_a = initialize(&client, &url, &bearer_a, &owner_header(owner_a)).await?;
    initialized(&client, &url, &session_a_owner_a, &bearer_a).await?;
    let tools_a_body = post_rpc(
        &client,
        &url,
        Some(&session_a_owner_a),
        &bearer_a,
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
    )
    .await?;
    let tools_a = tool_names(&tools_a_body);
    assert!(
        tools_a.contains(&"core_remember".to_string()),
        "{tools_a:?}"
    );

    let remembered = call_tool(
        &client,
        &url,
        &session_a_owner_a,
        &bearer_a,
        "core_remember",
        json!({
            "title": "multi-owner isolation",
            "body": unique_query,
            "idempotency_key": format!("multi-owner-{}", Uuid::now_v7())
        }),
    )
    .await?;
    assert!(
        remembered["handle"]
            .as_str()
            .is_some_and(|h| h.starts_with("F:")),
        "remember output: {remembered:?}"
    );

    let group_b_session = initialize(&client, &url, &bearer_b, &owner_header(owner_b)).await?;
    initialized(&client, &url, &group_b_session, &bearer_b).await?;
    let search_b = call_tool(
        &client,
        &url,
        &group_b_session,
        &bearer_b,
        "core_search_memories",
        json!({"query": unique_query, "mode": "lexical", "include_body": true}),
    )
    .await?;
    assert_eq!(
        search_b["memories"].as_array().map(Vec::len),
        Some(0),
        "group B must not read group A note: {search_b:?}"
    );

    let viewer_on_group_b = initialize(&client, &url, &bearer_a, &owner_header(owner_b)).await?;
    initialized(&client, &url, &viewer_on_group_b, &bearer_a).await?;
    let viewer_body = post_rpc(
        &client,
        &url,
        Some(&viewer_on_group_b),
        &bearer_a,
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/list", "params": {}}),
    )
    .await?;
    let viewer_tools = tool_names(&viewer_body);
    assert!(
        viewer_tools.contains(&"core_search_memories".to_string()),
        "{viewer_tools:?}"
    );
    assert!(
        !viewer_tools.contains(&"core_remember".to_string()),
        "viewer palette must hide write tools: {viewer_tools:?}"
    );

    let denied = initialize_raw(&client, &url, &bearer_b, &owner_header(owner_a)).await?;
    assert_eq!(
        denied.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "subject B must not bind group A"
    );

    revoke_member(&storage, owner_a, group_a, subject_a).await?;
    let oidc_after_revoke = post_rpc_raw(
        &client,
        &url,
        Some(&session_a_owner_a),
        &bearer_a,
        json!({"jsonrpc": "2.0", "id": 5, "method": "tools/list", "params": {}}),
    )
    .await?;
    // Membership is re-checked on the next request. A session whose bound
    // owner the principal can no longer narrow to answers the same 404 as an
    // unknown session (no cross-owner session-existence oracle); the client
    // re-initializes and owner selection then fails with 401.
    assert_eq!(
        oidc_after_revoke.status(),
        reqwest::StatusCode::NOT_FOUND,
        "revoked membership must invalidate the bound session on next request"
    );
    let reinit_after_revoke =
        initialize_raw(&client, &url, &bearer_a, &owner_header(owner_a)).await?;
    assert_eq!(
        reinit_after_revoke.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "re-initialization after revocation must fail owner selection"
    );

    running.shutdown().await;
    Ok(())
}

/// `core_transfer` over the wire, through the narrowing edge — the surface
/// every real caller reaches it on.
///
/// `mcp_auth` narrows each authenticated request with
/// `AuthzContext::narrowed_to_owner(selected_owner)`, whose role map holds
/// exactly the one owner the caller selected. A transfer needs authority on
/// TWO owners, so the request context can never carry both: before the
/// destination was resolved out of band, whichever side the caller
/// selected, the other answered `Forbidden` and the verb 403'd on every
/// possible invocation. Every transfer test that passed anyway called
/// `Engine::transfer_to_owner` directly with an un-narrowed context, which
/// no listener can produce. This test only speaks HTTP.
#[tokio::test]
#[allow(clippy::too_many_lines)] // linear e2e: boot + three authorization phases read best in one flow
async fn multi_owner_core_transfer_needs_admin_on_both_owners_through_the_narrowing_edge()
-> Result<(), Box<dyn std::error::Error>> {
    let (database_url, created_db) = live_database_url().await?;

    let source_group = GroupId::new(Uuid::now_v7());
    let destination_group = GroupId::new(Uuid::now_v7());
    let source: Owner = OwnerRef::Group(source_group);
    let destination: Owner = OwnerRef::Group(destination_group);

    // Three callers, one per authorization shape under test.
    let both_sides = UserId::new(Uuid::now_v7());
    let source_side = UserId::new(Uuid::now_v7());
    let destination_side = UserId::new(Uuid::now_v7());

    let owner_access: Arc<dyn OwnerAccessPort> =
        Arc::new(PgOwnerAccessResolver::connect_lazy(&database_url)?);
    let (signing, resolver) = keypair();
    let mut subject_map = OidcSubjectMap::new();
    subject_map.insert(ISSUER, "transfer-both-sides", both_sides)?;
    subject_map.insert(ISSUER, "transfer-source-side", source_side)?;
    subject_map.insert(ISSUER, "transfer-destination-side", destination_side)?;
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

    // `core_transfer` is outside the default `memory` profile, so the
    // deployment palette has to be the full surface for it to be served at
    // all — the first thing that must hold for the verb to be reachable.
    let running = Proxima::<ProximaMcpApp>::app()
        .tool_scope(ToolScope::All)
        .database_url(database_url.clone())
        .authenticator(Arc::new(authn))
        .resource_metadata(ResourceServerMetadata {
            public_url: "https://proxima.multi-owner.test".to_string(),
            authorization_servers: vec![ISSUER.to_string()],
        })
        .mcp_bind("127.0.0.1:0".parse().unwrap())
        .run()
        .await?;

    let result: Result<(), Box<dyn std::error::Error>> = async {
        // Real membership rows, seeded after boot the way a production grant
        // lands. Nothing here fabricates an in-memory role.
        let storage = PgStorage::connect(&database_url).await?;
        grant_member(&storage, source, source_group, both_sides, Relation::Admin).await?;
        grant_member(
            &storage,
            destination,
            destination_group,
            both_sides,
            Relation::Admin,
        )
        .await?;
        grant_member(&storage, source, source_group, source_side, Relation::Admin).await?;
        grant_member(
            &storage,
            destination,
            destination_group,
            destination_side,
            Relation::Admin,
        )
        .await?;

        let addr = running.mcp_addr.ok_or("missing MCP listener address")?;
        let url = format!("http://{addr}/mcp");
        let client = reqwest::Client::new();
        let source_key = owner_header(source);
        let destination_key = owner_header(destination);

        // (a) Admin + manage on BOTH the source group and the destination
        // group. The caller selects the SOURCE — the side whose write permit
        // carries the transfer — and the destination is re-resolved out of
        // band from its membership row.
        let bearer = format!("Bearer {}", mint(&signing, "transfer-both-sides"));
        let session = initialize(&client, &url, &bearer, &source_key).await?;
        initialized(&client, &url, &session, &bearer).await?;
        let remembered = call_tool(
            &client,
            &url,
            &session,
            &bearer,
            "core_remember",
            json!({
                "title": "transfer over the wire",
                "body": format!("wire transfer needle {}", Uuid::now_v7()),
                "idempotency_key": format!("wire-transfer-{}", Uuid::now_v7())
            }),
        )
        .await?;
        let handle = remembered["handle"]
            .as_str()
            .ok_or_else(|| format!("remember output has no handle: {remembered}"))?
            .to_string();

        let transferred = call_tool(
            &client,
            &url,
            &session,
            &bearer,
            "core_transfer",
            json!({
                "action": "transfer_to_owner",
                "entity": handle,
                "to_owner": destination_key
            }),
        )
        .await?;
        assert_eq!(
            transferred["ok"],
            json!(true),
            "admin+manage on both owners must transfer over the served surface: {transferred}"
        );

        // The move is real, not just a happy answer.
        let memory_t = handle
            .strip_prefix("F:")
            .ok_or_else(|| format!("unexpected handle spelling: {handle}"))?
            .parse::<Uuid>()?;
        let pool = sqlx::PgPool::connect(&database_url).await?;
        let landed: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.memory WHERE t = $1")
                .bind(memory_t)
                .fetch_one(&pool)
                .await?;
        assert_eq!(
            landed,
            destination.stored_owner_id(),
            "the series must live at the destination owner after the transfer"
        );

        // (b) Admin on the SOURCE only. The source gate opens; the
        // destination has no membership row for this caller, so the
        // receiving side refuses.
        let bearer = format!("Bearer {}", mint(&signing, "transfer-source-side"));
        let session = initialize(&client, &url, &bearer, &source_key).await?;
        initialized(&client, &url, &session, &bearer).await?;
        let stranded = call_tool(
            &client,
            &url,
            &session,
            &bearer,
            "core_remember",
            json!({
                "title": "source-side only",
                "body": format!("source side needle {}", Uuid::now_v7()),
                "idempotency_key": format!("wire-transfer-source-{}", Uuid::now_v7())
            }),
        )
        .await?;
        let stranded_handle = stranded["handle"]
            .as_str()
            .ok_or_else(|| format!("remember output has no handle: {stranded}"))?
            .to_string();
        let refused = call_tool_raw(
            &client,
            &url,
            &session,
            &bearer,
            "core_transfer",
            json!({
                "action": "transfer_to_owner",
                "entity": stranded_handle,
                "to_owner": destination_key
            }),
        )
        .await?;
        let message = rpc_error_message(&refused)?;
        assert!(
            message.contains("requires manage on this owner"),
            "admin on the source alone must be refused by the receiving side, got: {refused}"
        );

        // (c) Admin + manage on the DESTINATION only. This caller selects
        // the destination — the one owner it can select — so the served
        // palette does admit `core_transfer` and the refusal comes from the
        // engine's SOURCE gate, not from the edge hiding the tool. Manage on
        // the receiving side is consent to receive, never authority to pull
        // another owner's memory.
        let bearer = format!("Bearer {}", mint(&signing, "transfer-destination-side"));
        let session = initialize(&client, &url, &bearer, &destination_key).await?;
        initialized(&client, &url, &session, &bearer).await?;
        let refused = call_tool_raw(
            &client,
            &url,
            &session,
            &bearer,
            "core_transfer",
            json!({
                "action": "transfer_to_owner",
                "entity": stranded_handle,
                "to_owner": destination_key
            }),
        )
        .await?;
        let message = rpc_error_message(&refused)?;
        assert!(
            message.contains("requires admin on this owner"),
            "manage on the destination alone must not move another owner's memory, got: {refused}"
        );

        // The refusals left the series where it was.
        let stranded_t = stranded_handle
            .strip_prefix("F:")
            .ok_or_else(|| format!("unexpected handle spelling: {stranded_handle}"))?
            .parse::<Uuid>()?;
        let unmoved: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.memory WHERE t = $1")
                .bind(stranded_t)
                .fetch_one(&pool)
                .await?;
        assert_eq!(
            unmoved,
            source.stored_owner_id(),
            "a refused transfer must not move the series"
        );

        Ok(())
    }
    .await;

    running.shutdown().await;
    if let Some(name) = created_db {
        let _ = proxima_pg_testkit::drop_db(&name).await;
    }
    result
}

/// `PROXIMA_TEST_DATABASE_URL` when the operator pinned one, otherwise a
/// throwaway database. Unlike the palette/revocation test above, the
/// transfer reachability regression must not silently skip.
async fn live_database_url() -> Result<(String, Option<String>), Box<dyn std::error::Error>> {
    if let Some(url) = require_env_or_skip("PROXIMA_TEST_DATABASE_URL") {
        return Ok((url, None));
    }
    let name = format!("proxima_multi_owner_e2e_{}", Uuid::now_v7().simple());
    proxima_pg_testkit::create_db(&name)
        .await
        .map_err(|err| format!("PG required for the wire-level transfer e2e: {err}"))?;
    Ok((proxima_pg_testkit::db_url(&name), Some(name)))
}

/// The JSON-RPC error message for a refused tool call. A `ProtocolError`
/// with `Forbidden` reaches the wire as `invalid_request` carrying the
/// engine's own refusal text, so the assertion can name the gate that
/// fired instead of merely "something failed".
fn rpc_error_message(body: &serde_json::Value) -> Result<String, Box<dyn std::error::Error>> {
    assert!(
        body.get("result").is_none(),
        "expected a refusal, got a result: {body}"
    );
    Ok(body["error"]["message"]
        .as_str()
        .ok_or_else(|| format!("expected a JSON-RPC error, got: {body}"))?
        .to_string())
}

async fn grant_member(
    storage: &PgStorage,
    owner: Owner,
    group: GroupId,
    member: UserId,
    relation: Relation,
) -> Result<(), Box<dyn std::error::Error>> {
    let permit = membership_permit(owner).await?;
    storage
        .add_group_member(&permit, group, member, relation, Uuid::now_v7())
        .await?;
    Ok(())
}

async fn revoke_member(
    storage: &PgStorage,
    owner: Owner,
    group: GroupId,
    member: UserId,
) -> Result<(), Box<dyn std::error::Error>> {
    let permit = membership_permit(owner).await?;
    storage.remove_group_member(&permit, group, member).await?;
    Ok(())
}

async fn membership_permit(owner: Owner) -> Result<OwnerWritePermit, Box<dyn std::error::Error>> {
    let engine = Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests());
    let authz = AuthzContext::for_subject_with_role(
        UserId::new(Uuid::now_v7()),
        [(owner, Role::admin())],
        AuthPath::HostBearer,
    );
    Ok(engine
        .authorize_owner_write(&authz, &owner, AccessKind::Goal)
        .await?)
}

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

async fn initialize(
    client: &reqwest::Client,
    url: &str,
    bearer: &str,
    owner_key: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let response = initialize_raw(client, url, bearer, owner_key).await?;
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

async fn initialize_raw(
    client: &reqwest::Client,
    url: &str,
    bearer: &str,
    owner_key: &str,
) -> Result<reqwest::Response, Box<dyn std::error::Error>> {
    Ok(client
        .post(url)
        .header("Origin", "http://localhost")
        .header("Authorization", bearer)
        .header("X-Proxima-Owner", owner_key)
        .header("MCP-Protocol-Version", "2025-03-26")
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": {"name": "multi-owner-e2e", "version": "0"}
            }
        }))
        .send()
        .await?)
}

async fn initialized(
    client: &reqwest::Client,
    url: &str,
    session_id: &str,
    bearer: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let response = post_rpc_raw(
        client,
        url,
        Some(session_id),
        bearer,
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    )
    .await?;
    assert!(response.status().is_success(), "{}", response.status());
    Ok(())
}

async fn call_tool(
    client: &reqwest::Client,
    url: &str,
    session_id: &str,
    bearer: &str,
    name: &str,
    arguments: serde_json::Value,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let body = call_tool_raw(client, url, session_id, bearer, name, arguments).await?;
    let text = body["result"]["content"][0]["text"]
        .as_str()
        .ok_or_else(|| format!("missing text content in {body}"))?;
    Ok(serde_json::from_str(text)?)
}

/// The whole JSON-RPC envelope for a `tools/call`, so a refusal can be
/// inspected instead of unwrapped.
async fn call_tool_raw(
    client: &reqwest::Client,
    url: &str,
    session_id: &str,
    bearer: &str,
    name: &str,
    arguments: serde_json::Value,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    post_rpc(
        client,
        url,
        Some(session_id),
        bearer,
        json!({
            "jsonrpc": "2.0",
            "id": 9,
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments}
        }),
    )
    .await
}

async fn post_rpc(
    client: &reqwest::Client,
    url: &str,
    session_id: Option<&str>,
    bearer: &str,
    body: serde_json::Value,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let response = post_rpc_raw(client, url, session_id, bearer, body).await?;
    assert!(response.status().is_success(), "{}", response.status());
    sse_json(response).await
}

async fn post_rpc_raw(
    client: &reqwest::Client,
    url: &str,
    session_id: Option<&str>,
    bearer: &str,
    body: serde_json::Value,
) -> Result<reqwest::Response, Box<dyn std::error::Error>> {
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
    Ok(request.send().await?)
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

fn tool_names(body: &serde_json::Value) -> Vec<String> {
    body["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .filter_map(|tool| tool["name"].as_str().map(String::from))
        .collect()
}

fn owner_header(owner: Owner) -> String {
    owner.external_key()
}

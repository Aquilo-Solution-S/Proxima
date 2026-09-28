mod common;

use std::collections::{BTreeSet, HashMap};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use aws_lc_rs::rand::SystemRandom;
use aws_lc_rs::rsa::KeySize;
use aws_lc_rs::signature::{KeyPair as _, RSA_PKCS1_SHA256, RsaKeyPair};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use boon::{Compiler, Schemas};
use jsonwebtoken::DecodingKey;
use proxima::{Proxima, ResourceServerMetadata};
use proxima_auth_oidc::{OidcAuthConfig, OidcAuthenticator, OidcSubjectMap, StaticJwksResolver};
use proxima_core::{
    FlavorRegistry, OwnerAccessPort, OwnerRef, ToolScope, UserId, provider_safe_tool_name,
};
use proxima_mcp::ProximaMcpApp;
use proxima_storage_pg::PgOwnerAccessResolver;
use serde_json::json;
use uuid::Uuid;

use common::require_env_or_skip;

const KID: &str = "end-to-end-key";
const ISSUER: &str = "https://idp.end-to-end.test";
const AUDIENCE: &str = "proxima-mcp";
const OUTPUT_SCHEMA_URL: &str = "https://proxima.test/core-fact-output.json";

/// Ceiling on the serialized `tools/list` result for the authorized full
/// catalog. Clients that put every definition into context pay for each
/// char, so growth is a decision: trim prose or schema before raising it.
/// Measured in bytes when set (#375): with `code` 124,488 before, 80,110
/// after; without it 71,509 before, 42,601 after.
#[cfg(feature = "code")]
const TOOLS_LIST_BUDGET: usize = 84_000;
#[cfg(not(feature = "code"))]
const TOOLS_LIST_BUDGET: usize = 45_000;

#[test]
fn an_explicit_object_root_constrains_every_union_branch() {
    for keyword in ["anyOf", "oneOf"] {
        let mut schema = json!({
            "type": "object",
            keyword: [{"type": "object"}, {"type": "string"}],
        });
        let original = schema.clone();
        proxima_core::mcp::normalize_mcp_output_schema(&mut schema)
            .expect("an explicit object root already constrains the union");
        assert_eq!(schema, original);
        let mut compiler = Compiler::new();
        compiler.add_resource(OUTPUT_SCHEMA_URL, schema).unwrap();
        let mut schemas = Schemas::new();
        let index = compiler.compile(OUTPUT_SCHEMA_URL, &mut schemas).unwrap();
        schemas.validate(&json!({}), index).unwrap();
        for value in [
            json!("scalar"),
            json!(null),
            json!([]),
            json!(42),
            json!(true),
        ] {
            assert!(
                schemas.validate(&value, index).is_err(),
                "{keyword} cannot override the root object constraint: {value}"
            );
        }
    }
}

#[test]
fn composed_tools_publish_object_output_schemas() {
    let mut registry = FlavorRegistry::new();
    <ProximaMcpApp as proxima::flavor::FlavorBundle>::register(&mut registry)
        .expect("compiled flavors register");
    let registry = registry.try_freeze().expect("compiled registry freezes");
    for tool in registry.list_mcp_tools() {
        assert_eq!(
            tool.output_schema["type"], "object",
            "{} must publish an object output root: {}",
            tool.name, tool.output_schema
        );
    }
    for name in ["core_goal", "core_fact", "core_membership", "core_upload"] {
        let tool = registry
            .list_mcp_tools()
            .iter()
            .find(|tool| tool.name == name)
            .expect("core union tool registered");
        let branches = tool.output_schema["anyOf"]
            .as_array()
            .expect("output union must remain intact");
        assert!(!branches.is_empty(), "{name} must retain its variants");
        assert!(branches.iter().all(|branch| branch["type"] == "object"));
    }
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

#[tokio::test]
async fn oidc_host_auth_serves_tools_list() -> Result<(), Box<dyn std::error::Error>> {
    let Some(database_url) = require_env_or_skip("DATABASE_URL") else {
        eprintln!("skipping oidc_host_auth_serves_tools_list: DATABASE_URL not set");
        return Ok(());
    };
    let (runtime_url, platform_url) = common::split_roles(&database_url).await?;
    let subject = UserId::new(Uuid::now_v7());
    let owner_key = OwnerRef::Personal(subject).external_key();
    let (signing, resolver) = keypair();
    let mut subject_map = OidcSubjectMap::new();
    subject_map.insert(ISSUER, "operator-sub", subject)?;
    let owner_access: Arc<dyn OwnerAccessPort> =
        Arc::new(PgOwnerAccessResolver::connect_lazy_platform(
            &runtime_url,
            &platform_url,
            common::schemas(),
        )?);
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
        .database_url(runtime_url.clone())
        .platform_database_url(platform_url.clone())
        .owner(OwnerRef::Personal(subject))
        .authenticator(Arc::new(authn))
        .resource_metadata(ResourceServerMetadata {
            public_url: "https://proxima.end-to-end.test".to_string(),
            authorization_servers: vec![ISSUER.to_string()],
        })
        .mcp_bind(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0))
        .run()
        .await?;
    let addr = running.mcp_addr().ok_or("missing MCP listener address")?;
    let client = reqwest::Client::new();
    let url = format!("http://{addr}/mcp");
    let bearer = format!("Bearer {}", mint(&signing, "operator-sub"));
    let session_id = initialize(&client, &url, &bearer, &owner_key).await?;
    initialized(&client, &url, &session_id, &bearer).await?;
    let body = post_rpc(
        &client,
        &url,
        Some(&session_id),
        &bearer,
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
    )
    .await?;
    let listed = body["result"]["tools"].as_array().expect("tools");
    let names: Vec<_> = listed
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert!(names.contains(&"core_remember"), "got {names:?}");
    assert!(names.contains(&"core_goal"), "got {names:?}");

    let expected: BTreeSet<_> = running
        .host()
        .registry()
        .list_mcp_tools()
        .iter()
        .map(|tool| provider_safe_tool_name(tool.name))
        .collect();
    let actual: BTreeSet<_> = names.iter().map(|name| (*name).to_owned()).collect();
    assert_eq!(
        actual, expected,
        "the full authorized catalog must be listed"
    );
    for tool in listed {
        assert_eq!(
            tool["outputSchema"]["type"], "object",
            "strict MCP clients require object output roots: {tool}"
        );
        assert_plain_wire_schemas(tool);
    }
    let size = serde_json::to_string(&body["result"])?.len();
    assert!(
        size <= TOOLS_LIST_BUDGET,
        "tools/list is {size} bytes, over its {TOOLS_LIST_BUDGET}-byte budget"
    );

    assert_union_reply_matches_schema(&client, &url, &session_id, &bearer, listed).await?;

    running.shutdown().await;
    Ok(())
}

/// One reply against the output schema `tools/list` published for its tool,
/// as a strict client checks it.
fn assert_reply_matches_listed_schema(
    listed: &[serde_json::Value],
    name: &str,
    output: &serde_json::Value,
) {
    assert!(output.is_object(), "{name} replied {output}");
    let schema = &listed
        .iter()
        .find(|tool| tool["name"] == name)
        .unwrap_or_else(|| panic!("{name} listed"))["outputSchema"];
    let url = format!("https://proxima.test/{name}-output.json");
    let mut compiler = Compiler::new();
    compiler
        .add_resource(&url, schema.clone())
        .expect("published output schema loads");
    let mut schemas = Schemas::new();
    let index = compiler
        .compile(&url, &mut schemas)
        .expect("published output schema compiles");
    if let Err(error) = schemas.validate(output, index) {
        panic!("{name} structuredContent must validate against outputSchema: {error:#}");
    }
}

/// `tools/list` carries plain JSON Schema: no `x-` extension anywhere, no
/// `$schema`, and output schemas without prose (`description`, `title`,
/// `examples`) — models never see an output schema; clients only validate
/// against it.
fn assert_plain_wire_schemas(tool: &serde_json::Value) {
    fn visit(schema: &serde_json::Value, name: &str, output: bool, path: &str) {
        let Some(map) = schema.as_object() else {
            return;
        };
        for key in map.keys() {
            assert!(
                !key.starts_with("x-") && key != "$schema",
                "{name}: `{key}` at {path}"
            );
            assert!(
                !output || !["description", "title", "examples"].contains(&key.as_str()),
                "{name}: output schema keeps `{key}` at {path}"
            );
        }
        for keyword in ["properties", "patternProperties", "$defs"] {
            for (child, subschema) in map
                .get(keyword)
                .and_then(serde_json::Value::as_object)
                .into_iter()
                .flatten()
            {
                visit(
                    subschema,
                    name,
                    output,
                    &format!("{path}/{keyword}/{child}"),
                );
            }
        }
        for keyword in ["anyOf", "oneOf", "allOf", "prefixItems"] {
            for (index, subschema) in map
                .get(keyword)
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .enumerate()
            {
                visit(
                    subschema,
                    name,
                    output,
                    &format!("{path}/{keyword}/{index}"),
                );
            }
        }
        for keyword in [
            "items",
            "additionalProperties",
            "not",
            "if",
            "then",
            "else",
            "contains",
        ] {
            if let Some(subschema) = map.get(keyword) {
                visit(subschema, name, output, &format!("{path}/{keyword}"));
            }
        }
    }
    let name = tool["name"].as_str().unwrap_or_default();
    visit(&tool["inputSchema"], name, false, "inputSchema");
    visit(&tool["outputSchema"], name, true, "outputSchema");
}

async fn assert_union_reply_matches_schema(
    client: &reqwest::Client,
    url: &str,
    session_id: &str,
    bearer: &str,
    listed: &[serde_json::Value],
) -> Result<(), Box<dyn std::error::Error>> {
    let cited_object_id = Uuid::now_v7().to_string();
    let called = post_rpc(
        client,
        url,
        Some(session_id),
        bearer,
        json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {
                "name": "core_fact",
                "arguments": {
                    "action": "facts_citing_object",
                    "cited_object_id": cited_object_id,
                },
            },
        }),
    )
    .await?;
    assert!(called.get("error").is_none(), "{called}");
    let output = &called["result"]["structuredContent"];
    assert!(output.is_object(), "{called}");
    assert_eq!(output["cited_object_id"], cited_object_id);
    assert_eq!(output["facts"], json!([]));
    assert_reply_matches_listed_schema(listed, "core_fact", output);

    // A write reply whose type skips a field when unset: the published
    // schema must not require what serde leaves out.
    let call = |id: u64, name: &str, arguments: serde_json::Value| {
        let request = json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": { "name": name, "arguments": arguments },
        });
        async move { post_rpc(client, url, Some(session_id), bearer, request).await }
    };
    let mut facts = Vec::new();
    for (id, title) in [(4, "first observation"), (5, "second observation")] {
        let remembered = call(
            id,
            "core_remember",
            json!({ "title": title, "body": format!("{title} for the schema check") }),
        )
        .await?;
        let output = &remembered["result"]["structuredContent"];
        assert_reply_matches_listed_schema(listed, "core_remember", output);
        facts.push(output["handle"].clone());
    }
    let derived = call(
        6,
        "core_derive",
        json!({
            "kind": "Abstraction",
            "title": "a pattern",
            "body": "a pattern over both observations",
            "source_handles": facts,
        }),
    )
    .await?;
    assert_reply_matches_listed_schema(
        listed,
        "core_derive",
        &derived["result"]["structuredContent"],
    );
    let found = call(
        7,
        "core_search_memories",
        json!({ "query": "observation", "mode": "lexical" }),
    )
    .await?;
    assert_reply_matches_listed_schema(
        listed,
        "core_search_memories",
        &found["result"]["structuredContent"],
    );
    let spaces = call(8, "core_memory_spaces", json!({})).await?;
    assert_reply_matches_listed_schema(
        listed,
        "core_memory_spaces",
        &spaces["result"]["structuredContent"],
    );

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
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": {"name": "e2e", "version": "0"}
            }
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

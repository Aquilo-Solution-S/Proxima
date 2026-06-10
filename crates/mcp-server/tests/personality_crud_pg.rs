//! End-to-end MCP-CRUD over real Postgres. Mirrors `streamable_http_pg.rs`
//! shape. Asserts the discovery surface returns the expected catalog
//! contents and that mutation tools work end-to-end.

mod common;

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use common::{create_db, drop_db, initialize, initialized, post_rpc};
use proxima_core::auth::NoAuth;
use proxima_core::wake::token_store::WakeTokenStore;
use proxima_core::{Engine, FlavorRegistry, OrgId, Owner, Principal, UserId};
use proxima_mcp_server::{McpAuthStore, McpToolHost, default_allowlist, serve_streamable_http};
use proxima_storage_pg::PgStorage;
use serde_json::{Value, json};

#[tokio::test(flavor = "multi_thread")]
#[allow(clippy::too_many_lines)]
async fn discovery_to_mutation_smoke() -> Result<(), Box<dyn std::error::Error>> {
    // Helper to call a tool by name and extract the typed JSON output.
    async fn call_tool(
        client: &reqwest::Client,
        url: &str,
        session: &str,
        bearer: &str,
        name: &str,
        args: Value,
    ) -> Result<Value, Box<dyn std::error::Error>> {
        let resp = post_rpc(
            client,
            url,
            Some(session),
            bearer,
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": { "name": name, "arguments": args }
            }),
        )
        .await?;
        // tools/call returns { "result": { "content": [{ "type":"text", "text": "<json>" }] } }
        let text = resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("content[0].text exists; full response: {resp}"));
        Ok(serde_json::from_str(text)?)
    }

    let Some(db_name) = create_db().await? else {
        return Ok(());
    };
    let database_url = format!("postgres://proxima:proxima@localhost/{db_name}");
    let owner = Owner {
        principal: Principal::User(UserId::new(uuid::Uuid::now_v7())),
        org_id: OrgId::new(uuid::Uuid::now_v7()),
    };
    // The personality CRUD core tools dispatch through an attached engine,
    // so wire one over the same PG storage (Engine::compose embedding shape).
    let pg = PgStorage::connect(&database_url).await?;
    pg.run_migrations().await?;
    let resolver = NoAuth::new(owner.principal.clone(), owner.clone());
    let engine = Arc::new(Engine::compose(
        Box::new(resolver),
        Arc::new(pg.clone()),
        |_| {},
    ));
    let server = McpToolHost::from_pool(
        pg.pool().clone(),
        owner.clone(),
        Arc::new(FlavorRegistry::new().freeze()),
    )
    .with_engine(engine);
    let store = Arc::new(WakeTokenStore::new(Duration::from_mins(5)));
    let auth_store = Arc::new(McpAuthStore::new(store));
    let master_token = uuid::Uuid::now_v7();
    auth_store
        .replace_local_master_token(master_token, owner.clone())
        .await;

    let (handle, addr) = serve_streamable_http(
        SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0),
        server,
        default_allowlist(),
        auth_store,
    )
    .await?;

    let client = reqwest::Client::new();
    let url = format!("http://{addr}/mcp");
    let bearer = format!("Bearer {master_token}");
    let session = initialize(&client, &url, &bearer).await?;
    initialized(&client, &url, &session, &bearer).await?;

    // 1. Discovery: list_substrate_tools includes substrate-pack + MCP CRUD.
    let tools = call_tool(
        &client,
        &url,
        &session,
        &bearer,
        "core/list_substrate_tools",
        json!({}),
    )
    .await?;
    let arr = tools["tools"].as_array().expect("tools array");
    let names: std::collections::HashSet<_> = arr
        .iter()
        .map(|t| t["tool_id"].as_str().unwrap().to_string())
        .collect();
    assert!(
        names.contains("core/list_personalities"),
        "MCP CRUD tool present in discovery output"
    );
    assert!(
        names.contains("core/instantiate_personality"),
        "MCP CRUD tool present in discovery output"
    );

    // 3. Mutation: instantiate_personality.
    let inst = call_tool(
        &client,
        &url,
        &session,
        &bearer,
        "core/instantiate_personality",
        json!({ "display_name": "TestSubject", "purpose": "smoke test" }),
    )
    .await?;
    // Master-token calls run in OutputMode::RawIds (handles are minted only
    // for wake-dispatched model contexts), so the id is a raw UUID.
    let p_handle = inst["personality"].as_str().expect("P handle").to_string();
    assert!(
        uuid::Uuid::parse_str(&p_handle).is_ok(),
        "raw personality id under master token, got {p_handle}"
    );

    // 4. Read-after-write: list_personalities returns it.
    let list = call_tool(
        &client,
        &url,
        &session,
        &bearer,
        "core/list_personalities",
        json!({}),
    )
    .await?;
    let items = list["personalities"].as_array().expect("array");
    assert!(
        items
            .iter()
            .any(|p| p["display_name"].as_str() == Some("TestSubject")),
        "TestSubject visible in list"
    );

    // 5. Tombstone — first call returns idempotent_replay=false, second=true.
    let t1 = call_tool(
        &client,
        &url,
        &session,
        &bearer,
        "core/tombstone_personality",
        json!({ "personality": p_handle }),
    )
    .await?;
    assert_eq!(
        t1["idempotent_replay"],
        json!(false),
        "first tombstone is the canonical one"
    );
    let t2 = call_tool(
        &client,
        &url,
        &session,
        &bearer,
        "core/tombstone_personality",
        json!({ "personality": p_handle }),
    )
    .await?;
    assert_eq!(
        t2["idempotent_replay"],
        json!(true),
        "second tombstone is idempotent"
    );

    handle.abort();
    let _ = handle.await;
    drop_db(&db_name).await?;
    Ok(())
}

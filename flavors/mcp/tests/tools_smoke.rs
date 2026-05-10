use std::sync::Arc;

use proxima_core::McpToolError;
use proxima_core::mcp::{HandleTable, McpAuthorContext, McpToolCtx};
use proxima_core::{FlavorRegistry, OrgId, Owner, Principal, UserId};
use serde_json::json;
use sqlx::{Connection, Executor, PgConnection};

const ADMIN_URL: &str = "postgres://postgres@localhost/postgres";

#[tokio::test]
async fn remember_then_search_round_trip() -> Result<(), Box<dyn std::error::Error>> {
    let Some(db_name) = create_db().await? else {
        return Ok(());
    };
    let pg =
        proxima_storage_pg::PgStorage::connect(&format!("postgres://postgres@localhost/{db_name}"))
            .await?;
    pg.run_migrations().await?;
    proxima_mcp_substrate::migrator().run(pg.pool()).await?;

    let mut registry = FlavorRegistry::new();
    proxima_mcp_substrate::register(&mut registry);
    let frozen = Arc::new(registry.freeze());
    let owner = nil_owner();
    let handles = Arc::new(HandleTable::new());
    let author = author_ctx();

    let remembered = call_tool(
        pg.pool(),
        &owner,
        &handles,
        &frozen,
        author.clone(),
        "proxima-mcp/proxima_remember",
        json!({
            "title": "Atlas edges",
            "body": "Edges must be loaded from the visible node set.",
            "tags": ["atlas"],
            "idempotency_key": "tools-smoke-atlas-edge-loading"
        }),
    )
    .await?;
    assert!(
        remembered["handle"]
            .as_str()
            .expect("handle")
            .starts_with('N')
    );

    let searched = call_tool(
        pg.pool(),
        &owner,
        &handles,
        &frozen,
        author,
        "proxima-mcp/proxima_search_graph",
        json!({"query": "atlas edges", "limit": 5}),
    )
    .await?;
    assert_eq!(
        searched["matches"][0]["handle"], remembered["handle"],
        "search should reuse the session handle"
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn derive_scopes_idempotency_by_owner_and_kind() -> Result<(), Box<dyn std::error::Error>> {
    let Some(db_name) = create_db().await? else {
        return Ok(());
    };
    let pg =
        proxima_storage_pg::PgStorage::connect(&format!("postgres://postgres@localhost/{db_name}"))
            .await?;
    pg.run_migrations().await?;
    proxima_mcp_substrate::migrator().run(pg.pool()).await?;

    let mut registry = FlavorRegistry::new();
    proxima_mcp_substrate::register(&mut registry);
    let frozen = Arc::new(registry.freeze());
    let frozen_b = frozen.clone();
    let owner_a = nil_owner();
    let owner_b = Owner {
        principal: Principal::User(UserId::new(uuid::Uuid::from_u128(1))),
        org_id: OrgId::new(uuid::Uuid::nil()),
    };

    let shared_args = || {
        json!({
            "kind": "Abstraction",
            "title": "Shared idempotency",
            "body": "Same body, same key, different owner.",
            "model_id": "codex-test",
            "idempotency_key": "shared-key-collision",
        })
    };

    let a = call_tool(
        pg.pool(),
        &owner_a,
        &Arc::new(HandleTable::new()),
        &frozen,
        author_ctx(),
        "proxima-mcp/proxima_derive",
        shared_args(),
    )
    .await?;
    let b = call_tool(
        pg.pool(),
        &owner_b,
        &Arc::new(HandleTable::new()),
        &frozen,
        author_ctx(),
        "proxima-mcp/proxima_derive",
        shared_args(),
    )
    .await?;

    assert_ne!(a["uuid"], b["uuid"], "owner-a and owner-b must not collide");
    assert_eq!(b["idempotent_replay"], json!(false));

    let abstraction = call_tool(
        pg.pool(),
        &owner_a,
        &Arc::new(HandleTable::new()),
        &frozen,
        author_ctx(),
        "proxima-mcp/proxima_derive",
        json!({
            "kind": "Abstraction",
            "title": "Same key, A vs P",
            "body": "kind dimension test.",
            "model_id": "codex-test",
            "idempotency_key": "kind-key-collision",
        }),
    )
    .await?;
    let perspective = call_tool(
        pg.pool(),
        &owner_a,
        &Arc::new(HandleTable::new()),
        &frozen_b,
        author_ctx(),
        "proxima-mcp/proxima_derive",
        json!({
            "kind": "Perspective",
            "title": "Same key, A vs P",
            "body": "kind dimension test.",
            "model_id": "codex-test",
            "idempotency_key": "kind-key-collision",
        }),
    )
    .await?;
    assert_ne!(
        abstraction["uuid"], perspective["uuid"],
        "kind dimension must split memory_id"
    );
    assert_eq!(perspective["idempotent_replay"], json!(false));

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn derive_rejects_upward_provenance() -> Result<(), Box<dyn std::error::Error>> {
    let Some(db_name) = create_db().await? else {
        return Ok(());
    };
    let pg =
        proxima_storage_pg::PgStorage::connect(&format!("postgres://postgres@localhost/{db_name}"))
            .await?;
    pg.run_migrations().await?;
    proxima_mcp_substrate::migrator().run(pg.pool()).await?;

    let mut registry = FlavorRegistry::new();
    proxima_mcp_substrate::register(&mut registry);
    let frozen = Arc::new(registry.freeze());
    let owner = nil_owner();
    let handles = Arc::new(HandleTable::new());
    let author = author_ctx();

    let perspective = call_tool(
        pg.pool(),
        &owner,
        &handles,
        &frozen,
        author.clone(),
        "proxima-mcp/proxima_derive",
        json!({
            "kind": "Perspective",
            "title": "Top-layer perspective",
            "body": "A perspective with no sources, used as a layering pivot.",
            "model_id": "codex-test",
            "idempotency_key": "derive-layer-test-perspective"
        }),
    )
    .await?;
    let perspective_handle = perspective["handle"].as_str().expect("handle").to_string();

    let upward = call_tool(
        pg.pool(),
        &owner,
        &handles,
        &frozen,
        author,
        "proxima-mcp/proxima_derive",
        json!({
            "kind": "Abstraction",
            "title": "Should fail",
            "body": "Trying to derive an Abstraction from a Perspective is upward.",
            "model_id": "codex-test",
            "source_handles": [perspective_handle],
            "idempotency_key": "derive-layer-test-upward"
        }),
    )
    .await;

    match upward {
        Err(McpToolError::LayeringViolation(_)) => {}
        other => panic!("expected LayeringViolation, got {other:?}"),
    }

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

async fn call_tool(
    pool: &sqlx::PgPool,
    owner: &Owner,
    handles: &Arc<HandleTable>,
    registry: &Arc<proxima_core::FlavorRegistryFrozen>,
    author: McpAuthorContext,
    name: &str,
    args: serde_json::Value,
) -> Result<serde_json::Value, proxima_core::McpToolError> {
    let descriptor = registry
        .list_mcp_tools()
        .iter()
        .find(|tool| tool.name == name)
        .expect("registered tool");
    (descriptor.call)(
        McpToolCtx {
            pool: pool.clone(),
            owner: owner.clone(),
            handles: handles.clone(),
            registry: registry.clone(),
            author,
            caller_self_perspective: None,
            master_token_id: None,
            engine: None,
        },
        args,
    )
    .await
}

fn nil_owner() -> Owner {
    Owner {
        principal: Principal::User(UserId::new(uuid::Uuid::nil())),
        org_id: OrgId::new(uuid::Uuid::nil()),
    }
}

fn author_ctx() -> McpAuthorContext {
    McpAuthorContext {
        model_id: "codex-test".into(),
        client_name: "codex".into(),
        client_version: "1".into(),
        caller_self_perspective: None,
    }
}

async fn create_db() -> Result<Option<String>, Box<dyn std::error::Error>> {
    let db_name = format!("proxima_test_{}", uuid::Uuid::now_v7().simple());
    let Ok(mut conn) = PgConnection::connect(ADMIN_URL).await else {
        eprintln!("skipping (no admin PG)");
        return Ok(None);
    };
    conn.execute(format!("CREATE DATABASE \"{db_name}\"").as_str())
        .await?;
    conn.close().await?;
    Ok(Some(db_name))
}

async fn drop_db(name: &str) -> Result<(), sqlx::Error> {
    let mut conn = PgConnection::connect(ADMIN_URL).await?;
    conn.execute(format!("DROP DATABASE IF EXISTS \"{name}\" WITH (FORCE)").as_str())
        .await?;
    conn.close().await?;
    Ok(())
}

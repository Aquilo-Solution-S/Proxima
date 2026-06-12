//! End-to-end: a master-token MCP call resolves to a per-token
//! shell-author personality via the M0 ensure-on-call wiring; reconnect
//! resolves to the same identity; distinct tokens resolve to distinct
//! identities.

mod common;

use std::sync::Arc;

use common::{create_db, drop_db};
use proxima_core::auth::NoAuth;
use proxima_core::mcp::McpAuthorContext;
use proxima_core::storage::Storage;
use proxima_core::verbs::query::MemoryStore;
use proxima_core::{Engine, FlavorRegistry, OrgId, Owner, Principal, UserId};
use proxima_mcp_server::McpToolHost;
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread")]
async fn master_token_call_mints_per_token_self_perspective()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(db_name) = create_db().await? else {
        return Ok(());
    };
    let database_url = format!("postgres://proxima:proxima@localhost/{db_name}");
    let pg = PgStorage::connect(&database_url).await?;
    pg.run_migrations().await?;

    let owner = Owner {
        principal: Principal::User(UserId::new(Uuid::now_v7())),
        org_id: OrgId::new(Uuid::now_v7()),
    };

    // Build an Engine wired with the live PG storage so the call_tool
    // ensure step can reach the master-token verb.
    let resolver = NoAuth::new(owner.principal.clone(), owner.clone());
    let engine = Arc::new(
        Engine::new(
            FlavorRegistry::new().freeze(),
            MemoryStore::new(),
            Box::new(resolver),
        )
        .with_storage(Arc::new(pg.clone())),
    );

    // Build McpToolHost from pool (mirrors personality_crud_e2e_pg.rs pattern).
    let server = McpToolHost::from_pool(
        pg.pool().clone(),
        owner.clone(),
        Arc::new(FlavorRegistry::new().freeze()),
    )
    .with_engine(engine.clone());

    // Set up MCP edge auth and register the master token.
    let auth_store = proxima_mcp_server::McpEdgeAuth::headless();
    let token = Uuid::now_v7();
    auth_store
        .replace_local_master_token(token, owner.clone())
        .await;

    // Resolve the auth context for this token.
    let auth = auth_store
        .resolve(&format!("pxm_{token}"))
        .await
        .expect("token resolves");
    assert_eq!(auth.master_token_id, Some(token));
    assert!(auth.authz.capabilities.tool_scope.allows("anything"));

    // Build the author context with caller_self_perspective = None so the
    // ensure step (not the test) populates it.
    let author = McpAuthorContext {
        model_id: "test".into(),
        client_name: "test".into(),
        client_version: "0".into(),
        caller_self_perspective: None,
    };

    // Trigger the ensure via call_tool. core/list_personalities is read-only
    // and routes through the same call_tool surface that performs the M0
    // ensure step.
    server
        .call_tool(
            "core/list_personalities",
            serde_json::json!({}),
            author,
            Some(auth),
        )
        .await?;

    // Verify call_tool was the minter — the mapping table starts empty
    // (create_db produces a fresh DB), so finding the row here proves the
    // ensure-on-call step in McpToolHost::call_tool inserted it, not a
    // later probe.
    let pool = pg.pool().clone();
    let mapping_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM proxima_core.master_token_personality
         WHERE master_token_id = $1",
    )
    .bind(token)
    .fetch_one(&pool)
    .await?;
    assert_eq!(
        mapping_count, 1,
        "call_tool ensure step should have minted exactly one master_token_personality row"
    );

    // Verify: storage now has a per-token shell-author personality for this
    // owner. ensure_master_token_personality is idempotent so calling it here
    // is a read-back probe, not a duplicate write.
    let identity = pg.ensure_master_token_personality(&owner, token).await?;
    assert_ne!(identity.instance_id.into_inner(), Uuid::nil());
    assert_ne!(
        identity.self_perspective_memory_id.into_inner(),
        Uuid::nil()
    );

    // Reconnect: a second ensure under the same token returns the same
    // identity (idempotency contract).
    let identity_again = pg.ensure_master_token_personality(&owner, token).await?;
    assert_eq!(identity, identity_again);

    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn distinct_master_tokens_resolve_to_distinct_identities()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(db_name) = create_db().await? else {
        return Ok(());
    };
    let database_url = format!("postgres://proxima:proxima@localhost/{db_name}");
    let pg = PgStorage::connect(&database_url).await?;
    pg.run_migrations().await?;

    let owner = Owner {
        principal: Principal::User(UserId::new(Uuid::now_v7())),
        org_id: OrgId::new(Uuid::now_v7()),
    };

    let t_a = Uuid::now_v7();
    let t_b = Uuid::now_v7();

    let a = pg.ensure_master_token_personality(&owner, t_a).await?;
    let b = pg.ensure_master_token_personality(&owner, t_b).await?;
    assert_ne!(a.instance_id, b.instance_id);
    assert_ne!(a.self_perspective_memory_id, b.self_perspective_memory_id);

    drop_db(&db_name).await?;
    Ok(())
}

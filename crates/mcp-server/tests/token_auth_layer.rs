//! MCP auth layer accepts/rejects typed wire tokens.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::routing::get;
use axum::{Extension, Router};
use proxima_core::wake::token_store::{WakeTokenContext, WakeTokenStore};
use proxima_core::{
    HandleTable, MemoryHandleClass, MemoryId, OrgId, Owner, Principal, ToolScope, UserId,
    WakeChainDepth,
};
use proxima_mcp_server::security::{default_allowlist, mcp_auth_layer};
use proxima_mcp_server::{MASTER_TOKEN_PREFIX, McpAuthContext, McpEdgeAuth, WAKE_TOKEN_PREFIX};
use tower::util::ServiceExt;
use uuid::Uuid;

fn make_owner() -> Owner {
    Owner {
        principal: Principal::User(UserId::new(Uuid::now_v7())),
        org_id: OrgId::new(Uuid::now_v7()),
    }
}

fn make_ctx() -> WakeTokenContext {
    WakeTokenContext {
        invocation_id: Uuid::now_v7(),
        personality_instance_id: Uuid::now_v7(),
        wake_entry_id: Uuid::now_v7(),
        change_event_seq: Uuid::now_v7(),
        owner: make_owner(),
        palette: vec!["core/emit_abstraction".into()],
        model_id: "anthropic/claude-3-5-sonnet".into(),
        max_rounds: 4,
        current_root_perspective_memory_id: MemoryId::new(Uuid::now_v7()),
        current_root_perspective_memory_class: MemoryHandleClass::Perspective,
        triggering_event_memory_id: MemoryId::new(Uuid::now_v7()),
        triggering_event_memory_class: MemoryHandleClass::Fact,
        triggering_event_depth: WakeChainDepth::new(0),
        read_log: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        handles: Arc::new(HandleTable::new()),
    }
}

async fn protected(extensions: Extension<McpAuthContext>) -> String {
    match &extensions.0.authz.capabilities.tool_scope {
        ToolScope::All => "all".to_string(),
        ToolScope::Palette(palette) => palette.join(","),
    }
}

fn app(auth: Arc<McpEdgeAuth>) -> Router {
    Router::new()
        .route("/protected", get(protected))
        .layer(mcp_auth_layer(auth, default_allowlist()))
}

async fn get_status(app: Router, bearer: Option<String>) -> (StatusCode, String) {
    let mut request = Request::builder().uri("/protected");
    if let Some(bearer) = bearer {
        request = request.header(header::AUTHORIZATION, bearer);
    }
    let resp = app
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
    let body = std::str::from_utf8(&body).unwrap().to_string();
    (status, body)
}

#[tokio::test]
async fn rejects_without_authorization_header() {
    let auth = Arc::new(McpEdgeAuth::engine_hosted(Arc::new(WakeTokenStore::new(
        Duration::from_mins(1),
    ))));
    let (status, _) = get_status(app(auth), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn rejects_invalid_bearer_token() {
    let auth = Arc::new(McpEdgeAuth::engine_hosted(Arc::new(WakeTokenStore::new(
        Duration::from_mins(1),
    ))));
    let (status, _) = get_status(app(auth), Some("Bearer not-registered".into())).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn passes_with_valid_wake_token_and_injects_extension() {
    let store = Arc::new(WakeTokenStore::new(Duration::from_mins(1)));
    let ctx = make_ctx();
    let token = store.mint(ctx.clone()).await;
    let auth = Arc::new(McpEdgeAuth::engine_hosted(store));
    let (status, body) = get_status(
        app(auth),
        Some(format!("Bearer {WAKE_TOKEN_PREFIX}{token}")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, ctx.palette.join(","));
}

#[tokio::test]
async fn repeated_valid_wake_token_auth_renews_idle_lease() {
    let store = Arc::new(WakeTokenStore::new(Duration::from_millis(80)));
    let token = store
        .mint_with_max_lifetime(make_ctx(), Duration::from_millis(500))
        .await;
    let auth = Arc::new(McpEdgeAuth::engine_hosted(store));
    let app = app(auth);

    for _ in 0..5 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let (status, _) = get_status(
            app.clone(),
            Some(format!("Bearer {WAKE_TOKEN_PREFIX}{token}")),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }
}

#[tokio::test]
async fn bare_uuid_of_valid_wake_token_is_unauthorized() {
    let store = Arc::new(WakeTokenStore::new(Duration::from_mins(1)));
    let token = store.mint(make_ctx()).await;
    let auth = Arc::new(McpEdgeAuth::engine_hosted(store));
    let (status, _) = get_status(app(auth), Some(format!("Bearer {token}"))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn master_token_requires_master_prefix() {
    let auth = Arc::new(McpEdgeAuth::headless());
    let owner = make_owner();
    let token = Uuid::now_v7();
    auth.replace_local_master_token(token, owner).await;

    let (bare_status, _) = get_status(app(auth.clone()), Some(format!("Bearer {token}"))).await;
    assert_eq!(bare_status, StatusCode::UNAUTHORIZED);

    let (wake_status, _) = get_status(
        app(auth.clone()),
        Some(format!("Bearer {WAKE_TOKEN_PREFIX}{token}")),
    )
    .await;
    assert_eq!(wake_status, StatusCode::UNAUTHORIZED);

    let (master_status, body) = get_status(
        app(auth),
        Some(format!("Bearer {MASTER_TOKEN_PREFIX}{token}")),
    )
    .await;
    assert_eq!(master_status, StatusCode::OK);
    assert_eq!(body, "all");
}

#[tokio::test]
async fn malformed_reserved_prefix_is_unauthorized() {
    let auth = Arc::new(McpEdgeAuth::headless());
    let (status, _) = get_status(app(auth), Some("Bearer pxw_zzz".into())).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

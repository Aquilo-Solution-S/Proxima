//! Phase 1d: `WakeTokenAuthLayer` accepts/rejects per `Authorization` header.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::routing::get;
use axum::{Extension, Router};
use proxima_core::wake::token_store::{WakeTokenContext, WakeTokenStore};
use proxima_core::{HandleTable, MemoryId, OrgId, Owner, Principal, UserId, WakeChainDepth};
use proxima_mcp_server::security::{default_allowlist, mcp_auth_layer};
use proxima_mcp_server::{McpAuthContext, McpAuthStore, McpToolScope};
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
        invocation_id: Uuid::new_v4(),
        personality_instance_id: Uuid::new_v4(),
        wake_entry_id: Uuid::new_v4(),
        change_event_seq: Uuid::new_v4(),
        owner: make_owner(),
        palette: vec!["core/emit_abstraction".into()],
        model_id: "anthropic/claude-3-5-sonnet".into(),
        max_rounds: 4,
        current_root_perspective_memory_id: MemoryId::new(Uuid::now_v7()),
        triggering_event_memory_id: MemoryId::new(Uuid::now_v7()),
        triggering_event_depth: WakeChainDepth::new(0),
        read_log: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        handles: Arc::new(HandleTable::new()),
    }
}

async fn protected(extensions: Extension<McpAuthContext>) -> String {
    match &extensions.0.scope {
        McpToolScope::All => "all".to_string(),
        McpToolScope::Palette(palette) => palette.join(","),
    }
}

#[tokio::test]
async fn rejects_without_authorization_header() {
    let store = Arc::new(WakeTokenStore::new(Duration::from_mins(1)));
    let auth_store = Arc::new(McpAuthStore::new(store));
    let app = Router::new()
        .route("/protected", get(protected))
        .layer(mcp_auth_layer(auth_store, default_allowlist()));
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/protected")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn rejects_invalid_bearer_token() {
    let store = Arc::new(WakeTokenStore::new(Duration::from_mins(1)));
    let auth_store = Arc::new(McpAuthStore::new(store));
    let app = Router::new()
        .route("/protected", get(protected))
        .layer(mcp_auth_layer(auth_store, default_allowlist()));
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/protected")
                .header(
                    header::AUTHORIZATION,
                    "Bearer 00000000-0000-0000-0000-000000000000",
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn passes_with_valid_token_and_injects_extension() {
    let store = Arc::new(WakeTokenStore::new(Duration::from_mins(1)));
    let ctx = make_ctx();
    let token = store.mint(ctx.clone()).await;
    let auth_store = Arc::new(McpAuthStore::new(store));
    let app = Router::new()
        .route("/protected", get(protected))
        .layer(mcp_auth_layer(auth_store, default_allowlist()));
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/protected")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
    let body_str = std::str::from_utf8(&body).unwrap();
    assert_eq!(body_str, ctx.palette.join(","));
}

#[tokio::test]
async fn repeated_valid_wake_token_auth_renews_idle_lease() {
    let store = Arc::new(WakeTokenStore::new(Duration::from_millis(80)));
    let token = store
        .mint_with_max_lifetime(make_ctx(), Duration::from_millis(500))
        .await;
    let auth_store = Arc::new(McpAuthStore::new(store));
    let app = Router::new()
        .route("/protected", get(protected))
        .layer(mcp_auth_layer(auth_store, default_allowlist()));

    for _ in 0..5 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}

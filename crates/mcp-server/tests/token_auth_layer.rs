//! Phase 1d: `WakeTokenAuthLayer` accepts/rejects per `Authorization` header.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::routing::get;
use axum::{Extension, Router};
use proxima_core::wake::token_store::{WakeTokenContext, WakeTokenStore};
use proxima_core::{OrgId, Owner, Principal, UserId};
use proxima_mcp_server::security::wake_token_auth_layer;
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
        owner: make_owner(),
        palette: vec!["core/emit_abstraction".into()],
        model_id: "anthropic/claude-3-5-sonnet".into(),
        max_rounds: 4,
    }
}

async fn protected(extensions: Extension<WakeTokenContext>) -> String {
    extensions.0.invocation_id.to_string()
}

#[tokio::test]
async fn rejects_without_authorization_header() {
    let store = Arc::new(WakeTokenStore::new(Duration::from_mins(1)));
    let app = Router::new()
        .route("/protected", get(protected))
        .layer(wake_token_auth_layer(store));
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
    let app = Router::new()
        .route("/protected", get(protected))
        .layer(wake_token_auth_layer(store));
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
    let app = Router::new()
        .route("/protected", get(protected))
        .layer(wake_token_auth_layer(store));
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
    assert_eq!(body_str, ctx.invocation_id.to_string());
}

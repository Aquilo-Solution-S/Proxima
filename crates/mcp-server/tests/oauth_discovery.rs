use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use proxima_core::RevalidationConfig;
use proxima_mcp_server::{
    McpEdgeAuth, ResourceServerMetadata, default_allowlist, mcp_auth_layer_with_metadata,
    protected_resource_router,
};
use tower::ServiceExt;

#[tokio::test]
async fn discovery_is_public_and_json() {
    let md = ResourceServerMetadata {
        public_url: "https://p.example.com".into(),
        authorization_servers: vec!["https://idp.example.com".into()],
    };
    let app = protected_resource_router(&md);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/.well-known/oauth-protected-resource")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn missing_bearer_401_carries_www_authenticate() {
    use axum::Router;
    use axum::routing::any;

    let md = ResourceServerMetadata {
        public_url: "https://p.example.com".into(),
        authorization_servers: vec!["https://idp.example.com".into()],
    };
    let value = header::HeaderValue::from_str(&md.www_authenticate_value()).unwrap();
    let edge = Arc::new(McpEdgeAuth::headless());
    let app = Router::new()
        .route("/mcp", any(|| async { StatusCode::OK }))
        .layer(mcp_auth_layer_with_metadata(
            edge,
            default_allowlist(),
            RevalidationConfig::default(),
            Some(value),
        ));
    let resp = app
        .oneshot(Request::builder().uri("/mcp").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert!(resp.headers().contains_key(header::WWW_AUTHENTICATE));
}

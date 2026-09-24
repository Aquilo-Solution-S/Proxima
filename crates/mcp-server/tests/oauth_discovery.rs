use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use axum::routing::any;
use proxima_core::RevalidationConfig;
use proxima_mcp_server::{
    MCP_PATH, McpEdgeAuth, ResourceServerMetadata, mcp_auth_layer_with_metadata,
    protected_resource_router,
};
use tower::ServiceExt;

fn metadata() -> ResourceServerMetadata {
    ResourceServerMetadata {
        public_url: "https://p.example.com".into(),
        authorization_servers: vec!["https://idp.example.com".into()],
    }
}

async fn document(path: &str) -> serde_json::Value {
    let resp = protected_resource_router(&metadata())
        .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "{path}");
    assert_eq!(resp.headers()[header::CONTENT_TYPE], "application/json");
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&body).unwrap()
}

// An MCP client requires `resource` to equal the URL it connected to, path
// included; the origin document keeps answering the origin for `/v1`.
#[tokio::test]
async fn each_discovery_document_is_public_and_names_its_resource() {
    let origin = document("/.well-known/oauth-protected-resource").await;
    assert_eq!(origin["resource"], "https://p.example.com");

    let mcp = document("/.well-known/oauth-protected-resource/mcp").await;
    assert_eq!(mcp["resource"], "https://p.example.com/mcp");
    assert_eq!(
        mcp["scopes_supported"],
        serde_json::json!(["openid", "offline_access"])
    );
}

async fn challenge(app: Router, path: &str) -> String {
    let resp = app
        .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "{path}");
    resp.headers()[header::WWW_AUTHENTICATE]
        .to_str()
        .unwrap()
        .to_owned()
}

fn auth_layer() -> proxima_mcp_server::McpAuthLayer {
    mcp_auth_layer_with_metadata(
        Arc::new(McpEdgeAuth::headless()),
        RevalidationConfig::default(),
        Some(&metadata()),
    )
}

const MCP_CHALLENGE: &str =
    "Bearer resource_metadata=\"https://p.example.com/.well-known/oauth-protected-resource/mcp\"";
const ORIGIN_CHALLENGE: &str =
    "Bearer resource_metadata=\"https://p.example.com/.well-known/oauth-protected-resource\"";

// The 401 points each surface at its own document. Both stack shapes hosts
// use are covered: the layer over the whole router (Proxima's runtime) and
// a route layer over a nested `/mcp` service (an embedding host), where the
// nested service would otherwise see a stripped path.
#[tokio::test]
async fn missing_bearer_401_points_at_the_requested_resource() {
    let stub = || any(|| async { StatusCode::OK });
    let layered = Router::new()
        .nest_service(MCP_PATH, stub())
        .route("/v1/tools", stub())
        .layer(auth_layer());
    let route_layered = Router::new()
        .nest_service(MCP_PATH, stub())
        .route("/v1/sealed-call", stub())
        .route_layer(auth_layer());

    for app in [layered.clone(), route_layered.clone()] {
        assert_eq!(challenge(app.clone(), "/mcp").await, MCP_CHALLENGE);
    }
    assert_eq!(challenge(layered, "/v1/tools").await, ORIGIN_CHALLENGE);
    assert_eq!(
        challenge(route_layered, "/v1/sealed-call").await,
        ORIGIN_CHALLENGE
    );
}

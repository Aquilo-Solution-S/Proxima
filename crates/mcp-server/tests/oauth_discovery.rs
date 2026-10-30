use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use axum::routing::any;
use proxima_core::RevalidationConfig;
use proxima_mcp_server::{
    AuthorizationServers, MCP_PATH, McpEdgeAuth, ResourceServerMetadata,
    mcp_auth_layer_with_metadata, public_discovery_router,
};
use tower::ServiceExt;

fn metadata() -> ResourceServerMetadata {
    ResourceServerMetadata {
        public_url: "https://p.example.com".into(),
        authorization_servers: AuthorizationServers::Issuers(vec![
            "https://idp.example.com".into(),
        ]),
        additional_scopes: Vec::new(),
    }
}

async fn document(path: &str) -> serde_json::Value {
    let resp = public_discovery_router(&metadata())
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

fn public_client_metadata() -> ResourceServerMetadata {
    ResourceServerMetadata {
        public_url: "https://p.example.com/".into(),
        authorization_servers: AuthorizationServers::PublicClient(
            proxima_mcp_server::PublicClientFacade::try_new(
                "shared-client",
                "https://idp.example.com/authorize?x=1",
                "https://idp.example.com/token",
            )
            .unwrap(),
        ),
        additional_scopes: vec![proxima_mcp_server::ScopeToken::parse("extra").unwrap()],
    }
}

async fn request(app: Router, method: &str, path: &str, body: &str) -> (StatusCode, String) {
    let response = app
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_owned()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

#[tokio::test]
async fn direct_issuers_preserve_documents_and_do_not_offer_facade_routes() {
    let app = public_discovery_router(&metadata());
    for (path, resource) in [
        (
            "/.well-known/oauth-protected-resource",
            "https://p.example.com",
        ),
        (
            "/.well-known/oauth-protected-resource/mcp",
            "https://p.example.com/mcp",
        ),
    ] {
        let (status, body) = request(app.clone(), "GET", path, "").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body,
            format!(
                r#"{{"resource":"{resource}","authorization_servers":["https://idp.example.com"],"bearer_methods_supported":["header"],"scopes_supported":["openid","offline_access"]}}"#
            )
        );
    }
    for (method, path) in [
        ("GET", "/.well-known/oauth-authorization-server"),
        ("GET", "/.well-known/oauth-authorization-server/mcp"),
        ("POST", "/oauth/register"),
    ] {
        assert_eq!(
            request(app.clone(), method, path, "{}").await.0,
            StatusCode::NOT_FOUND,
            "{path}"
        );
    }
}

#[tokio::test]
async fn public_client_publishes_consistent_metadata() {
    let app = public_discovery_router(&public_client_metadata());
    for path in [
        "/.well-known/oauth-protected-resource",
        "/.well-known/oauth-protected-resource/mcp",
    ] {
        let (status, body) = request(app.clone(), "GET", path, "").await;
        assert_eq!(status, StatusCode::OK);
        let prm: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            prm["authorization_servers"],
            serde_json::json!(["https://p.example.com"])
        );
        assert_eq!(
            prm["scopes_supported"],
            serde_json::json!(["openid", "offline_access", "extra"])
        );
    }
    let expected = serde_json::json!({
        "issuer": "https://p.example.com",
        "authorization_endpoint": "https://idp.example.com/authorize?x=1",
        "token_endpoint": "https://idp.example.com/token",
        "registration_endpoint": "https://p.example.com/oauth/register",
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
        "scopes_supported": ["openid", "offline_access", "extra"],
    });
    for path in [
        "/.well-known/oauth-authorization-server",
        "/.well-known/oauth-authorization-server/mcp",
    ] {
        let (status, body) = request(app.clone(), "GET", path, "").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&body).unwrap(),
            expected
        );
    }
}

#[tokio::test]
async fn registration_returns_shared_client_and_rejects_invalid_requests() {
    let app = public_discovery_router(&public_client_metadata());
    let (status, body) = request(app.clone(), "POST", "/oauth/register", r#"{"redirect_uris":["https://client.example/cb","custom://callback"],"client_name":"Client","token_endpoint_auth_method":"none","unrecognized":true}"#).await;
    assert_eq!(status, StatusCode::CREATED);
    let body: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["client_id"], "shared-client");
    assert_eq!(
        body["redirect_uris"],
        serde_json::json!(["https://client.example/cb", "custom://callback"])
    );
    assert_eq!(body["token_endpoint_auth_method"], "none");
    assert_eq!(
        body["grant_types"],
        serde_json::json!(["authorization_code", "refresh_token"])
    );
    assert_eq!(body["response_types"], serde_json::json!(["code"]));
    assert_eq!(body["client_name"], "Client");
    for (body, expected) in [
        ("{}", "invalid_redirect_uri"),
        (r#"{"redirect_uris":[]}"#, "invalid_redirect_uri"),
        (
            r#"{"redirect_uris":"https://client.example/cb"}"#,
            "invalid_redirect_uri",
        ),
        (r#"{"redirect_uris":["/relative"]}"#, "invalid_redirect_uri"),
        (
            r#"{"redirect_uris":["https://client.example/cb"],"token_endpoint_auth_method":"client_secret_basic"}"#,
            "invalid_client_metadata",
        ),
        ("not JSON", "invalid_client_metadata"),
    ] {
        let (status, result) = request(app.clone(), "POST", "/oauth/register", body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&result).unwrap()["error"],
            expected,
            "{body}"
        );
    }
}

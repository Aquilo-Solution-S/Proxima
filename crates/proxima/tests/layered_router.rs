use std::sync::Arc;

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use axum::routing::get;
use proxima::{Authz, layered_router};
use proxima_core::{
    AuthError, AuthPath, Authenticator, AuthzContext, Credentials, FlavorRegistry, OrgId, Owner,
    Principal, UserId,
};
use proxima_mcp_server::{
    MASTER_TOKEN_PREFIX, McpEdgeAuth, McpToolHost, default_allowlist, streamable_http_service,
};
use tokio_util::sync::CancellationToken;
use tower::util::ServiceExt;
use uuid::Uuid;

fn owner() -> Owner {
    Owner {
        principal: Principal::User(UserId::new(Uuid::now_v7())),
        org_id: OrgId::new(Uuid::now_v7()),
    }
}

async fn ping(Authz(_authz): Authz) -> StatusCode {
    StatusCode::OK
}

struct StubHostAuth {
    owner: Owner,
}

#[async_trait]
impl Authenticator for StubHostAuth {
    async fn authenticate(&self, creds: &Credentials) -> Result<AuthzContext, AuthError> {
        match creds {
            Credentials::Bearer(token) if token == "host-token" => Ok(AuthzContext::single_owner(
                &self.owner,
                AuthPath::HostBearer,
            )),
            _ => Err(AuthError::InvalidCredentials),
        }
    }
}

fn router(auth: Arc<McpEdgeAuth>, owner: Owner) -> Router {
    let pool = sqlx::PgPool::connect_lazy("postgres://placeholder/db").expect("lazy pool");
    let host = McpToolHost::from_pool(pool, owner, Arc::new(FlavorRegistry::new().freeze()));
    let cancel = CancellationToken::new();
    let allowlist = default_allowlist();
    let service = streamable_http_service(host, &allowlist, &cancel);
    let app_router = Router::new().route("/app/ping", get(ping));
    layered_router(service, app_router, auth, allowlist)
}

async fn status(app: Router, method: Method, uri: &str, bearer: Option<String>) -> StatusCode {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(bearer) = bearer {
        builder = builder.header(header::AUTHORIZATION, bearer);
    }
    app.oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn layered_router_protects_app_and_mcp_with_master_token() {
    let owner = owner();
    let token = Uuid::now_v7();
    let auth = Arc::new(McpEdgeAuth::headless());
    auth.replace_local_master_token(token, owner.clone()).await;
    let app = router(auth, owner);
    let bearer = format!("Bearer {MASTER_TOKEN_PREFIX}{token}");

    assert_eq!(
        status(app.clone(), Method::GET, "/app/ping", None).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        status(app.clone(), Method::POST, "/mcp", None).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        status(
            app.clone(),
            Method::GET,
            "/app/ping",
            Some("Bearer garbage".to_string())
        )
        .await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        status(
            app.clone(),
            Method::POST,
            "/mcp",
            Some("Bearer garbage".to_string())
        )
        .await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        status(app.clone(), Method::GET, "/app/ping", Some(bearer.clone())).await,
        StatusCode::OK
    );
    let mcp_status = status(app, Method::POST, "/mcp", Some(bearer)).await;
    assert_ne!(mcp_status, StatusCode::UNAUTHORIZED);
    assert_ne!(mcp_status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn layered_router_protects_app_and_mcp_with_host_token() {
    let owner = owner();
    let auth = Arc::new(McpEdgeAuth::headless().with_host(
        Arc::new(StubHostAuth {
            owner: owner.clone(),
        }),
        owner.clone(),
    ));
    let app = router(auth, owner);
    let bearer = "Bearer host-token".to_string();

    assert_eq!(
        status(app.clone(), Method::GET, "/app/ping", None).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        status(app.clone(), Method::POST, "/mcp", None).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        status(
            app.clone(),
            Method::GET,
            "/app/ping",
            Some("Bearer garbage".to_string())
        )
        .await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        status(
            app.clone(),
            Method::POST,
            "/mcp",
            Some("Bearer garbage".to_string())
        )
        .await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        status(app.clone(), Method::GET, "/app/ping", Some(bearer.clone())).await,
        StatusCode::OK
    );
    let mcp_status = status(app, Method::POST, "/mcp", Some(bearer)).await;
    assert_ne!(mcp_status, StatusCode::UNAUTHORIZED);
    assert_ne!(mcp_status, StatusCode::FORBIDDEN);
}

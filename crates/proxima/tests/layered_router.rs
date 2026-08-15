use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use axum::routing::get;
use proxima::{Authz, layered_router};
use proxima_core::{
    AuthError, AuthPath, Authenticator, AuthzContext, Credentials, FlavorRegistry, FlavorServices,
    Owner, OwnerRef, UserId,
};
use proxima_mcp_server::{
    HostAllowlist, McpEdgeAuth, McpToolHost, default_allowlist, owner_key, streamable_http_service,
};
use tokio_util::sync::CancellationToken;
use tower::util::ServiceExt;
use uuid::Uuid;

fn owner() -> Owner {
    OwnerRef::Personal(UserId::new(Uuid::now_v7()))
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
            Credentials::Bearer(_) => Err(AuthError::InvalidCredentials),
        }
    }
}

struct CountingHostAuth {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Authenticator for CountingHostAuth {
    async fn authenticate(&self, _creds: &Credentials) -> Result<AuthzContext, AuthError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(AuthError::InvalidCredentials)
    }
}

fn edge_auth() -> McpEdgeAuth {
    McpEdgeAuth::headless()
}

fn router(auth: Arc<McpEdgeAuth>, owner: Owner) -> Router {
    router_with_hosts(auth, owner, &[])
}

fn router_with_hosts(auth: Arc<McpEdgeAuth>, _owner: Owner, allowed_hosts: &[String]) -> Router {
    let host = McpToolHost::from_parts(
        Arc::new(FlavorRegistry::new().freeze_or_panic_for_tests()),
        FlavorServices::default(),
    );
    let cancel = CancellationToken::new();
    let allowlist = default_allowlist();
    let host_allowlist = HostAllowlist::new(allowed_hosts);
    let service = streamable_http_service(host, &allowlist, &host_allowlist, &cancel);
    let app_router = Router::new().route("/app/ping", get(ping));
    layered_router(service, app_router, auth, allowlist, host_allowlist)
}

async fn status(
    app: Router,
    method: Method,
    uri: &str,
    selected_owner: Owner,
    bearer: Option<String>,
) -> StatusCode {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, "localhost");
    if let Some(bearer) = bearer {
        builder = builder
            .header(header::AUTHORIZATION, bearer)
            .header("X-Proxima-Owner", owner_key(selected_owner));
    }
    app.oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap()
        .status()
}

async fn status_with_host(
    app: Router,
    method: Method,
    uri: &str,
    bearer: Option<&str>,
    host: &str,
    selected_owner: Owner,
) -> StatusCode {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, host);
    if let Some(bearer) = bearer {
        request = request
            .header(header::AUTHORIZATION, bearer)
            .header("X-Proxima-Owner", owner_key(selected_owner));
    }
    let request = request.body(Body::empty()).unwrap();
    app.oneshot(request).await.unwrap().status()
}

// rmcp's Host guard must honor a configured public host; loopback-only
// default 403s every non-loopback `Host`.
#[tokio::test]
async fn host_guard_allows_configured_host_and_rejects_foreign() {
    let owner = owner();
    let auth = Arc::new(edge_auth().with_host(Arc::new(StubHostAuth { owner })));
    let app = router_with_hosts(auth, owner, &["proxima.test".to_string()]);
    let bearer = "Bearer host-token";

    // Configured public Host passes the guard (auth + handshake run).
    let allowed = status_with_host(
        app.clone(),
        Method::POST,
        "/mcp",
        Some(bearer),
        "proxima.test",
        owner,
    )
    .await;
    assert_ne!(allowed, StatusCode::UNAUTHORIZED);
    assert_ne!(allowed, StatusCode::FORBIDDEN);
    assert_eq!(
        status_with_host(
            app.clone(),
            Method::GET,
            "/app/ping",
            Some(bearer),
            "proxima.test",
            owner,
        )
        .await,
        StatusCode::OK
    );

    // A foreign Host is rejected before auth on every merged protected route.
    assert_eq!(
        status_with_host(
            app.clone(),
            Method::POST,
            "/mcp",
            Some(bearer),
            "evil.test",
            owner,
        )
        .await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        status_with_host(
            app.clone(),
            Method::GET,
            "/app/ping",
            Some(bearer),
            "evil.test",
            owner,
        )
        .await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        status_with_host(app, Method::GET, "/app/ping", None, "evil.test", owner,).await,
        StatusCode::FORBIDDEN
    );
}

// Exposure proof: an empty deployment override must NOT become rmcp's
// allow-all state. HostAllowlist materializes loopback before either guard sees
// the policy, so a foreign Host is still 403'd while loopback passes.
#[tokio::test]
async fn host_guard_empty_override_stays_loopback_only_not_allow_all() {
    let owner = owner();
    let auth = Arc::new(edge_auth().with_host(Arc::new(StubHostAuth { owner })));
    let app = router_with_hosts(auth, owner, &[]);
    let bearer = "Bearer host-token";

    assert_eq!(
        status_with_host(
            app.clone(),
            Method::POST,
            "/mcp",
            Some(bearer),
            "evil.test",
            owner,
        )
        .await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        status_with_host(
            app.clone(),
            Method::GET,
            "/app/ping",
            Some(bearer),
            "evil.test",
            owner,
        )
        .await,
        StatusCode::FORBIDDEN
    );
    let loopback = status_with_host(
        app.clone(),
        Method::POST,
        "/mcp",
        Some(bearer),
        "127.0.0.1",
        owner,
    )
    .await;
    assert_ne!(loopback, StatusCode::FORBIDDEN);
    assert_eq!(
        status_with_host(
            app,
            Method::GET,
            "/app/ping",
            Some(bearer),
            "localhost:31415",
            owner,
        )
        .await,
        StatusCode::OK
    );
}

#[tokio::test]
async fn host_guard_rejects_before_calling_the_authenticator() {
    let owner = owner();
    let calls = Arc::new(AtomicUsize::new(0));
    let auth = Arc::new(edge_auth().with_host(Arc::new(CountingHostAuth {
        calls: Arc::clone(&calls),
    })));
    let app = router(auth, owner);

    assert_eq!(
        status_with_host(
            app,
            Method::GET,
            "/app/ping",
            Some("Bearer must-not-be-checked"),
            "evil.test",
            owner,
        )
        .await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn allowed_preflight_covers_mcp_and_host_routes_without_authentication() {
    let owner = owner();
    let calls = Arc::new(AtomicUsize::new(0));
    let auth = Arc::new(edge_auth().with_host(Arc::new(CountingHostAuth {
        calls: Arc::clone(&calls),
    })));
    let app = router(auth, owner);

    for uri in ["/mcp", "/app/ping"] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::OPTIONS)
                    .uri(uri)
                    .header(header::HOST, "localhost")
                    .header(header::ORIGIN, "http://localhost:5173")
                    .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
                    .header(
                        header::ACCESS_CONTROL_REQUEST_HEADERS,
                        "authorization,content-type,x-proxima-owner",
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT, "{uri}");
        assert_eq!(
            response.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN),
            Some(&header::HeaderValue::from_static("http://localhost:5173")),
            "{uri}"
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn origin_rejection_and_host_rejection_both_run_before_auth() {
    let owner = owner();
    let calls = Arc::new(AtomicUsize::new(0));
    let auth = Arc::new(edge_auth().with_host(Arc::new(CountingHostAuth {
        calls: Arc::clone(&calls),
    })));
    let app = router(auth, owner);

    let disallowed_origin = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/app/ping")
                .header(header::HOST, "localhost")
                .header(header::ORIGIN, "https://evil.test")
                .header(header::AUTHORIZATION, "Bearer must-not-be-checked")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(disallowed_origin.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        to_bytes(disallowed_origin.into_body(), usize::MAX)
            .await
            .unwrap(),
        "origin not allowed"
    );

    let foreign_host_preflight = app
        .oneshot(
            Request::builder()
                .method(Method::OPTIONS)
                .uri("/mcp")
                .header(header::HOST, "evil.test")
                .header(header::ORIGIN, "http://localhost")
                .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(foreign_host_preflight.status(), StatusCode::FORBIDDEN);
    assert!(
        !foreign_host_preflight
            .headers()
            .contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn allowed_actual_responses_carry_cors_headers_across_auth_results() {
    let owner = owner();
    let auth = Arc::new(edge_auth().with_host(Arc::new(StubHostAuth { owner })));
    let app = router(auth, owner);

    let unauthorized = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/app/ping")
                .header(header::HOST, "localhost")
                .header(header::ORIGIN, "http://localhost:5173")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        unauthorized
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN),
        Some(&header::HeaderValue::from_static("http://localhost:5173"))
    );
    assert_eq!(
        unauthorized
            .headers()
            .get(header::ACCESS_CONTROL_EXPOSE_HEADERS),
        Some(&header::HeaderValue::from_static(
            "Mcp-Session-Id, WWW-Authenticate"
        ))
    );

    let authorized = app
        .oneshot(
            Request::builder()
                .uri("/app/ping")
                .header(header::HOST, "localhost")
                .header(header::ORIGIN, "http://localhost:5173")
                .header(header::AUTHORIZATION, "Bearer host-token")
                .header("X-Proxima-Owner", owner_key(owner))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(authorized.status(), StatusCode::OK);
    assert_eq!(
        authorized
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN),
        Some(&header::HeaderValue::from_static("http://localhost:5173"))
    );
}

// `layered_router`/`layered_router_with_revalidation` carry the same
// body-size cap as `build_router` and the streamable transport. A declared
// oversized `Content-Length` must 413 before Host or auth runs (foreign
// Host, no Authorization).
#[tokio::test]
async fn layered_router_rejects_oversized_body_before_auth() {
    const OVER_CAP_BYTES: usize = 8 * 1024 * 1024;

    let owner = owner();
    let auth = Arc::new(edge_auth().with_host(Arc::new(StubHostAuth { owner })));
    let app = router(auth, owner);

    let request = Request::builder()
        .method(Method::POST)
        .uri("/mcp")
        .header(header::HOST, "evil.test")
        .header(header::CONTENT_LENGTH, OVER_CAP_BYTES.to_string())
        .body(Body::from("x"))
        .unwrap();
    let status = app.oneshot(request).await.unwrap().status();

    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn layered_router_protects_app_and_mcp_with_host_token() {
    let owner = owner();
    let auth = Arc::new(edge_auth().with_host(Arc::new(StubHostAuth { owner })));
    let app = router(auth, owner);
    let bearer = "Bearer host-token".to_string();

    assert_eq!(
        status(app.clone(), Method::GET, "/app/ping", owner, None).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        status(app.clone(), Method::POST, "/mcp", owner, None).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        status(
            app.clone(),
            Method::GET,
            "/app/ping",
            owner,
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
            owner,
            Some("Bearer garbage".to_string())
        )
        .await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        status(
            app.clone(),
            Method::GET,
            "/app/ping",
            owner,
            Some(bearer.clone())
        )
        .await,
        StatusCode::OK
    );
    let mcp_status = status(app, Method::POST, "/mcp", owner, Some(bearer)).await;
    assert_ne!(mcp_status, StatusCode::UNAUTHORIZED);
    assert_ne!(mcp_status, StatusCode::FORBIDDEN);
}

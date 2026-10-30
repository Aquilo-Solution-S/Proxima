//! The resolved MCP edge of a booted runtime, for a host composing its own
//! router around Proxima's `/mcp` instead of serving [`BuiltProxima::service`].
//!
//! [`BuiltProxima::service`]: crate::BuiltProxima::service

use std::convert::Infallible;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::Request;
use axum::response::IntoResponse;
use proxima_core::RevalidationConfig;
use proxima_mcp_server::{
    HostAllowlist, McpEdgeAuth, McpStreamableService, McpToolHost, McpTransportConfig,
    OriginAllowlist, ResourceServerMetadata, guard_listener,
    streamable_http_service_with_transport,
};
use tokio_util::sync::CancellationToken;
use tower::Service;

/// What the runtime resolved for its MCP edge: the tool host (host tools,
/// call recording, request headers, engine), the bearer authentication, the
/// Origin and Host allowlists, stream revalidation, OAuth resource metadata
/// and the transport. [`BuiltProxima::mcp_edge`](crate::BuiltProxima::mcp_edge)
/// returns it when MCP is enabled, so a host never resolves the builder a
/// second time to learn them.
#[derive(Clone)]
pub struct McpEdge {
    pub(crate) tool_host: McpToolHost,
    pub(crate) edge_auth: Arc<McpEdgeAuth>,
    pub(crate) origin_allowlist: OriginAllowlist,
    pub(crate) host_allowlist: HostAllowlist,
    pub(crate) revalidation: RevalidationConfig,
    pub(crate) resource_metadata: Option<ResourceServerMetadata>,
    pub(crate) transport: McpTransportConfig,
    pub(crate) rest_router: Router,
    pub(crate) cancel: CancellationToken,
}

impl std::fmt::Debug for McpEdge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpEdge")
            .field("tool_host", &self.tool_host)
            .field("edge_auth", &self.edge_auth)
            .field("origin_allowlist", &self.origin_allowlist)
            .field("host_allowlist", &self.host_allowlist)
            .field("revalidation", &self.revalidation)
            .field("resource_metadata", &self.resource_metadata.is_some())
            .field("transport", &self.transport)
            .finish_non_exhaustive()
    }
}

impl McpEdge {
    /// The tool host `/mcp` serves: registry tools, host tools, recording.
    #[must_use]
    pub fn tool_host(&self) -> &McpToolHost {
        &self.tool_host
    }

    /// The bearer authentication `/mcp` runs behind.
    #[must_use]
    pub fn edge_auth(&self) -> &Arc<McpEdgeAuth> {
        &self.edge_auth
    }

    /// Browser `Origin` allowlist (`PROXIMA_ALLOWED_ORIGINS`). A portless
    /// entry accepts any port; an explicit port also matches a browser
    /// `Origin` that omits it when it is the scheme default (HTTP 80 or
    /// HTTPS 443).
    #[must_use]
    pub fn origin_allowlist(&self) -> &OriginAllowlist {
        &self.origin_allowlist
    }

    /// Inbound `Host` allowlist, loopback included.
    #[must_use]
    pub fn host_allowlist(&self) -> &HostAllowlist {
        &self.host_allowlist
    }

    /// Stream lifetime and auth-epoch revalidation.
    #[must_use]
    pub const fn revalidation(&self) -> RevalidationConfig {
        self.revalidation
    }

    /// OAuth protected-resource metadata, when configured.
    #[must_use]
    pub fn resource_metadata(&self) -> Option<&ResourceServerMetadata> {
        self.resource_metadata.as_ref()
    }

    /// rmcp transport settings; `max_request_body_bytes` is the body cap.
    #[must_use]
    pub const fn transport(&self) -> &McpTransportConfig {
        &self.transport
    }

    /// A fresh rmcp service for `/mcp` over [`Self::tool_host`], with this
    /// edge's allowlists and transport, cancelled with the runtime.
    #[must_use]
    pub fn mcp_service(&self) -> McpStreamableService {
        streamable_http_service_with_transport(
            self.tool_host.clone(),
            &self.origin_allowlist,
            &self.host_allowlist,
            &self.cancel,
            &self.transport,
        )
    }

    /// `/mcp` (and `/v1` when REST is enabled) behind this edge's bearer
    /// auth, `app_router` beside it **without** bearer auth, the whole
    /// listener behind the body cap, the Host guard and CORS; the OAuth
    /// metadata routes are public. `app_router` authenticates its own routes.
    pub fn router(&self, app_router: Router) -> Router {
        let protected = Router::new()
            .nest_service(proxima_mcp_server::MCP_PATH, self.mcp_service())
            .merge(self.rest_router.clone())
            .layer(proxima_mcp_server::mcp_auth_layer_with_metadata(
                Arc::clone(&self.edge_auth),
                self.revalidation,
                self.resource_metadata.as_ref(),
            ));
        let mut router = protected.merge(app_router);
        if let Some(metadata) = &self.resource_metadata {
            router = router.merge(proxima_mcp_server::public_discovery_router(metadata));
        }
        guard_listener(
            router,
            self.origin_allowlist.clone(),
            self.host_allowlist.clone(),
            self.transport.max_request_body_bytes,
        )
    }
}

/// [`crate::layered_router_with_revalidation`] with bearer auth on `/mcp`
/// only: `app_router` is served beside it under the listener-wide body
/// limit, Host guard and CORS, and authenticates its own routes (or none).
/// Unlike [`McpEdge::router`] it mounts no resource-metadata routes and caps
/// bodies at the default [`MAX_REQUEST_BODY_BYTES`](proxima_mcp_server::MAX_REQUEST_BODY_BYTES).
///
/// `host_allowlist` must also be passed to
/// [`streamable_http_service`](proxima_mcp_server::streamable_http_service).
pub fn layered_router_mcp_only<S>(
    mcp_service: S,
    app_router: Router,
    edge_auth: Arc<McpEdgeAuth>,
    allowlist: OriginAllowlist,
    host_allowlist: HostAllowlist,
    revalidation: RevalidationConfig,
) -> Router
where
    S: Service<Request<Body>, Error = Infallible> + Clone + Send + Sync + 'static,
    S::Response: IntoResponse,
    S::Future: Send + 'static,
{
    let router = Router::new()
        .nest_service(proxima_mcp_server::MCP_PATH, mcp_service)
        .layer(proxima_mcp_server::mcp_auth_layer_with_config(
            edge_auth,
            revalidation,
        ))
        .merge(app_router);
    guard_listener(
        router,
        allowlist,
        host_allowlist,
        proxima_mcp_server::MAX_REQUEST_BODY_BYTES,
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use proxima_core::{FlavorRegistry, FlavorServices, RevalidationConfig};
    use tower::ServiceExt;

    use super::*;

    #[tokio::test]
    async fn public_client_discovery_bypasses_auth_but_mcp_remains_protected() {
        let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
        let metadata = ResourceServerMetadata {
            public_url: "https://proxima.example.com".to_string(),
            authorization_servers: proxima_mcp_server::AuthorizationServers::PublicClient(
                proxima_mcp_server::PublicClientFacade::try_new(
                    "public-client",
                    "https://idp.example.com/authorize",
                    "https://idp.example.com/token",
                )
                .unwrap(),
            ),
            additional_scopes: Vec::new(),
        };
        let edge = McpEdge {
            tool_host: McpToolHost::from_parts(Arc::new(registry), FlavorServices::default()),
            edge_auth: Arc::new(McpEdgeAuth::headless()),
            origin_allowlist: OriginAllowlist::parse(["https://proxima.example.com"]).unwrap(),
            host_allowlist: HostAllowlist::parse(["proxima.example.com"]).unwrap(),
            revalidation: RevalidationConfig::default(),
            resource_metadata: Some(metadata),
            transport: McpTransportConfig::default(),
            rest_router: Router::new(),
            cancel: CancellationToken::new(),
        };
        let app = edge.router(Router::new());

        for path in [
            "/.well-known/oauth-authorization-server",
            "/.well-known/oauth-authorization-server/mcp",
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(path)
                        .header("host", "proxima.example.com")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{path}");
            let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(json["issuer"], "https://proxima.example.com");
        }

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/oauth/register")
                    .header("host", "proxima.example.com")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"redirect_uris":["https://client.example/cb"]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(proxima_mcp_server::MCP_PATH)
                    .header("host", "proxima.example.com")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}

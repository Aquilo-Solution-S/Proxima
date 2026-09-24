//! OAuth 2.0 Protected Resource Metadata (RFC 9728) for the deployment.
//!
//! Two protected resources, two documents. The origin covers `/v1` and host
//! routes; the MCP endpoint is its own resource, `{origin}/mcp`, because an
//! MCP client connects to that URL and requires the document's `resource`
//! to equal it, path included. RFC 9728 §3.1 inserts the well-known suffix
//! before the path, so the MCP document lives at
//! `/.well-known/oauth-protected-resource/mcp` and the root document keeps
//! answering the origin (§3.3: `resource` equals the identifier the
//! document's URL was built from).

use axum::{Router, http::HeaderValue, http::header::CONTENT_TYPE, routing::get};

/// The origin's discovery path.
pub const PROTECTED_RESOURCE_METADATA_PATH: &str = "/.well-known/oauth-protected-resource";

/// The path the Streamable HTTP MCP endpoint is served at.
pub const MCP_PATH: &str = "/mcp";

/// The MCP endpoint's discovery path (RFC 9728 §3.1 path insertion).
pub const MCP_PROTECTED_RESOURCE_METADATA_PATH: &str = "/.well-known/oauth-protected-resource/mcp";

/// Scopes a client requests: `openid` for the OIDC sign-in, `offline_access`
/// for a refresh token. A client that finds no `scopes_supported` requests
/// none, and its session ends with the first access token.
pub const SCOPES_SUPPORTED: [&str; 2] = ["openid", "offline_access"];

/// Which protected resource a document or a 401 challenge describes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtectedResource {
    /// The public origin: `/v1` and host routes.
    Origin,
    /// The MCP endpoint, `{origin}/mcp`.
    Mcp,
}

impl ProtectedResource {
    /// The resource a request path belongs to.
    #[must_use]
    pub fn for_path(path: &str) -> Self {
        match path.strip_prefix(MCP_PATH) {
            Some("") => Self::Mcp,
            Some(rest) if rest.starts_with('/') => Self::Mcp,
            _ => Self::Origin,
        }
    }

    const fn path(self) -> &'static str {
        match self {
            Self::Origin => "",
            Self::Mcp => MCP_PATH,
        }
    }

    const fn metadata_path(self) -> &'static str {
        match self {
            Self::Origin => PROTECTED_RESOURCE_METADATA_PATH,
            Self::Mcp => MCP_PROTECTED_RESOURCE_METADATA_PATH,
        }
    }
}

/// Advertised resource-server metadata pointing MCP clients at the `IdP`.
#[derive(Clone, Debug)]
pub struct ResourceServerMetadata {
    /// Public base URL (scheme+host, no trailing slash), e.g. `https://proxima.example.com`.
    pub public_url: String,
    /// Authorization servers (the Zitadel issuer URL(s)).
    pub authorization_servers: Vec<String>,
}

impl ResourceServerMetadata {
    fn origin(&self) -> &str {
        self.public_url.trim_end_matches('/')
    }

    /// RFC 9728 protected-resource identifier of `which`.
    ///
    /// Not the token audience: the bearer's `aud` is checked against the
    /// configured audience, so a client sending either identifier as its
    /// RFC 8707 `resource` holds a token both surfaces accept unless its
    /// authorization server stamps `resource` into `aud` (Zitadel does not).
    #[must_use]
    pub fn resource(&self, which: ProtectedResource) -> String {
        format!("{}{}", self.origin(), which.path())
    }

    #[must_use]
    pub fn metadata_url(&self, which: ProtectedResource) -> String {
        format!("{}{}", self.origin(), which.metadata_path())
    }

    #[must_use]
    pub fn to_json(&self, which: ProtectedResource) -> serde_json::Value {
        serde_json::json!({
            "resource": self.resource(which),
            "authorization_servers": self.authorization_servers,
            "bearer_methods_supported": ["header"],
            "scopes_supported": SCOPES_SUPPORTED,
        })
    }

    #[must_use]
    pub fn www_authenticate_value(&self, which: ProtectedResource) -> String {
        format!("Bearer resource_metadata=\"{}\"", self.metadata_url(which))
    }
}

/// The 401 challenge for each protected resource, validated once.
#[derive(Clone, Debug)]
pub(crate) struct ResourceChallenges {
    origin: HeaderValue,
    mcp: HeaderValue,
}

impl ResourceChallenges {
    /// `None` when a challenge is not a valid header value (a public URL
    /// with control characters); the 401 then carries no challenge, as
    /// before.
    pub(crate) fn new(metadata: &ResourceServerMetadata) -> Option<Self> {
        let challenge = |which| HeaderValue::from_str(&metadata.www_authenticate_value(which)).ok();
        Some(Self {
            origin: challenge(ProtectedResource::Origin)?,
            mcp: challenge(ProtectedResource::Mcp)?,
        })
    }

    pub(crate) fn for_path(&self, path: &str) -> &HeaderValue {
        match ProtectedResource::for_path(path) {
            ProtectedResource::Origin => &self.origin,
            ProtectedResource::Mcp => &self.mcp,
        }
    }
}

/// A router exposing only the unauthenticated discovery documents. Merge
/// this *after* the auth layer so it bypasses bearer enforcement.
#[must_use = "merge the returned router into the public MCP HTTP surface"]
pub fn protected_resource_router(metadata: &ResourceServerMetadata) -> Router {
    [ProtectedResource::Origin, ProtectedResource::Mcp]
        .into_iter()
        .fold(Router::new(), |router, which| {
            let body = metadata.to_json(which).to_string();
            router.route(
                which.metadata_path(),
                get(move || {
                    let body = body.clone();
                    async move { ([(CONTENT_TYPE, "application/json")], body) }
                }),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::{ProtectedResource, ResourceChallenges, ResourceServerMetadata};

    fn metadata() -> ResourceServerMetadata {
        ResourceServerMetadata {
            public_url: "https://proxima.example.com".to_string(),
            authorization_servers: vec!["https://idp.example.com".to_string()],
        }
    }

    /// The origin document still names the origin, so `/v1` clients see
    /// what they saw before; only the MCP document carries the path.
    #[test]
    fn each_resource_names_the_url_its_clients_connect_to() {
        assert_eq!(
            metadata().resource(ProtectedResource::Origin),
            "https://proxima.example.com"
        );
        assert_eq!(
            metadata().resource(ProtectedResource::Mcp),
            "https://proxima.example.com/mcp"
        );

        let trailing = ResourceServerMetadata {
            public_url: "https://proxima.example.com/".to_string(),
            authorization_servers: vec!["https://idp.example.com".to_string()],
        };
        assert_eq!(
            trailing.resource(ProtectedResource::Origin),
            "https://proxima.example.com"
        );
        assert_eq!(
            trailing.resource(ProtectedResource::Mcp),
            "https://proxima.example.com/mcp"
        );
    }

    /// RFC 9728 §3.1: the well-known suffix goes between the host and the
    /// resource's path.
    #[test]
    fn metadata_urls_insert_the_well_known_suffix_before_the_path() {
        assert_eq!(
            metadata().metadata_url(ProtectedResource::Origin),
            "https://proxima.example.com/.well-known/oauth-protected-resource"
        );
        assert_eq!(
            metadata().metadata_url(ProtectedResource::Mcp),
            "https://proxima.example.com/.well-known/oauth-protected-resource/mcp"
        );
    }

    #[test]
    fn json_contains_rfc_9728_fields() {
        for which in [ProtectedResource::Origin, ProtectedResource::Mcp] {
            let json = metadata().to_json(which);
            assert_eq!(json["resource"], metadata().resource(which));
            assert_eq!(json["authorization_servers"][0], "https://idp.example.com");
            assert_eq!(json["bearer_methods_supported"][0], "header");
            assert_eq!(
                json["scopes_supported"],
                serde_json::json!(["openid", "offline_access"])
            );
        }
    }

    #[test]
    fn only_the_mcp_endpoint_and_below_are_the_mcp_resource() {
        for path in ["/mcp", "/mcp/", "/mcp/anything"] {
            assert_eq!(ProtectedResource::for_path(path), ProtectedResource::Mcp);
        }
        for path in ["/", "/v1/tools", "/mcpx", "/v1/mcp", ""] {
            assert_eq!(ProtectedResource::for_path(path), ProtectedResource::Origin);
        }
    }

    #[test]
    fn a_challenge_points_at_the_document_for_the_requested_path() {
        let challenges = ResourceChallenges::new(&metadata()).expect("valid header values");
        assert_eq!(
            challenges.for_path("/mcp"),
            "Bearer resource_metadata=\"https://proxima.example.com/.well-known/oauth-protected-resource/mcp\""
        );
        assert_eq!(
            challenges.for_path("/v1/tools"),
            "Bearer resource_metadata=\"https://proxima.example.com/.well-known/oauth-protected-resource\""
        );
    }
}

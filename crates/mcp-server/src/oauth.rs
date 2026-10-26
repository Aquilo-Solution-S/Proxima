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
/// none, and its session ends with the first access token. A deployment adds
/// its own after these ([`ResourceServerMetadata::additional_scopes`]).
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

/// One OAuth scope: an RFC 6749 §3.3 `scope-token`, one or more printable
/// ASCII characters other than space, `"` and `\`. A client joins the
/// published scopes with spaces, so any other value would request scopes
/// nobody configured.
///
/// The constructor is the only way in; a literal cannot skip its checks:
///
/// ```compile_fail,E0423
/// let _ = proxima_mcp_server::ScopeToken(String::from("two scopes"));
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopeToken(String);

impl ScopeToken {
    /// # Errors
    ///
    /// Returns [`ScopeTokenError::Invalid`] when `raw` is empty or holds a
    /// character outside the `scope-token` grammar.
    pub fn parse(raw: impl Into<String>) -> Result<Self, ScopeTokenError> {
        let value = raw.into();
        let is_scope_token = !value.is_empty()
            && value
                .bytes()
                .all(|byte| matches!(byte, 0x21 | 0x23..=0x5B | 0x5D..=0x7E));
        if is_scope_token {
            Ok(Self(value))
        } else {
            Err(ScopeTokenError::Invalid { value })
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ScopeTokenError {
    #[error("invalid OAuth scope {value:?}: expected an RFC 6749 scope-token")]
    Invalid { value: String },
}

/// Advertised resource-server metadata pointing MCP clients at the `IdP`.
///
/// A host whose bearer must carry more than the sign-in scopes lists them in
/// `additional_scopes`:
///
/// ```
/// use proxima_mcp_server::{ProtectedResource, ResourceServerMetadata, ScopeToken};
///
/// let metadata = ResourceServerMetadata {
///     public_url: "https://erp.example.com".to_string(),
///     authorization_servers: vec!["https://auth.example.com".to_string()],
///     additional_scopes: vec![ScopeToken::parse("urn:zitadel:iam:org:project:id:42:aud")?],
/// };
/// assert_eq!(
///     metadata.to_json(ProtectedResource::Mcp)["scopes_supported"],
///     serde_json::json!(["openid", "offline_access", "urn:zitadel:iam:org:project:id:42:aud"]),
/// );
/// # Ok::<(), proxima_mcp_server::ScopeTokenError>(())
/// ```
#[derive(Clone, Debug)]
pub struct ResourceServerMetadata {
    /// Public base URL (scheme+host, no trailing slash), e.g. `https://proxima.example.com`.
    pub public_url: String,
    /// Authorization servers (the Zitadel issuer URL(s)).
    pub authorization_servers: Vec<String>,
    /// Scopes published after [`SCOPES_SUPPORTED`], for tokens that must
    /// carry more than the sign-in: an MCP client requests what
    /// `scopes_supported` lists. Example: a host that exchanges the bearer
    /// (RFC 8693) at Zitadel, which grants an exchange only scopes the
    /// subject token already holds, lists the audience scope it will ask
    /// for. Usually empty.
    pub additional_scopes: Vec<ScopeToken>,
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

    /// [`SCOPES_SUPPORTED`], then each additional scope not already listed.
    #[must_use]
    pub fn scopes_supported(&self) -> Vec<&str> {
        let mut scopes = SCOPES_SUPPORTED.to_vec();
        for scope in &self.additional_scopes {
            if !scopes.contains(&scope.as_str()) {
                scopes.push(scope.as_str());
            }
        }
        scopes
    }

    #[must_use]
    pub fn to_json(&self, which: ProtectedResource) -> serde_json::Value {
        serde_json::json!({
            "resource": self.resource(which),
            "authorization_servers": self.authorization_servers,
            "bearer_methods_supported": ["header"],
            "scopes_supported": self.scopes_supported(),
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
    use super::{
        ProtectedResource, ResourceChallenges, ResourceServerMetadata, ScopeToken, ScopeTokenError,
    };

    fn metadata() -> ResourceServerMetadata {
        ResourceServerMetadata {
            public_url: "https://proxima.example.com".to_string(),
            authorization_servers: vec!["https://idp.example.com".to_string()],
            additional_scopes: Vec::new(),
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
            ..metadata()
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

    /// A deployment's scopes follow the sign-in scopes in both documents,
    /// each once.
    #[test]
    fn additional_scopes_follow_the_sign_in_scopes_once() {
        let metadata = ResourceServerMetadata {
            additional_scopes: vec![
                ScopeToken::parse("urn:zitadel:iam:org:project:id:42:aud").unwrap(),
                ScopeToken::parse("openid").unwrap(),
            ],
            ..metadata()
        };
        for which in [ProtectedResource::Origin, ProtectedResource::Mcp] {
            assert_eq!(
                metadata.to_json(which)["scopes_supported"],
                serde_json::json!([
                    "openid",
                    "offline_access",
                    "urn:zitadel:iam:org:project:id:42:aud"
                ])
            );
        }
    }

    #[test]
    fn a_scope_outside_the_scope_token_grammar_is_refused() {
        for raw in [
            "",
            "two scopes",
            "quote\"d",
            "back\\slash",
            "tab\t",
            "ümlaut",
        ] {
            assert_eq!(
                ScopeToken::parse(raw),
                Err(ScopeTokenError::Invalid {
                    value: raw.to_string()
                }),
            );
        }
        assert_eq!(
            ScopeToken::parse("urn:zitadel:iam:org:project:id:42:aud")
                .unwrap()
                .as_str(),
            "urn:zitadel:iam:org:project:id:42:aud"
        );
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

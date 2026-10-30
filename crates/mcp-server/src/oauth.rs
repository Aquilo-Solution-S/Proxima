//! OAuth 2.0 protected-resource and authorization-server discovery for deployments.
//!
//! Two protected resources, two documents. The origin covers `/v1` and host
//! routes; the MCP endpoint is its own resource, `{origin}/mcp`, because an
//! MCP client connects to that URL and requires the document's `resource`
//! to equal it, path included. RFC 9728 §3.1 inserts the well-known suffix
//! before the path, so the MCP document lives at
//! `/.well-known/oauth-protected-resource/mcp` and the root document keeps
//! answering the origin (§3.3: `resource` equals the identifier the
//! document's URL was built from).

use axum::{
    Router,
    body::Bytes,
    http::{HeaderValue, StatusCode, header::CONTENT_TYPE},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use url::Url;

/// The origin's discovery path.
pub const PROTECTED_RESOURCE_METADATA_PATH: &str = "/.well-known/oauth-protected-resource";

/// The path the Streamable HTTP MCP endpoint is served at.
pub const MCP_PATH: &str = "/mcp";

/// The MCP endpoint's discovery path (RFC 9728 §3.1 path insertion).
pub const MCP_PROTECTED_RESOURCE_METADATA_PATH: &str = "/.well-known/oauth-protected-resource/mcp";

/// RFC 8414 authorization-server metadata, served under
/// [`AuthorizationServers::PublicClient`] only.
pub const AUTHORIZATION_SERVER_METADATA_PATH: &str = "/.well-known/oauth-authorization-server";

/// The same document at the path-inserted location some MCP clients probe.
pub const MCP_AUTHORIZATION_SERVER_METADATA_PATH: &str =
    "/.well-known/oauth-authorization-server/mcp";

/// RFC 7591 registration, served under [`AuthorizationServers::PublicClient`]
/// only. Outside [`MCP_PATH`], which is bearer-gated.
pub const REGISTRATION_PATH: &str = "/oauth/register";

/// What a registration grants: the authorization-code flow with refresh.
const GRANT_TYPES: [&str; 2] = ["authorization_code", "refresh_token"];

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

/// Advertised resource-server metadata pointing clients at direct issuers or a public-client facade.
///
/// The facade advertises this origin for discovery and registration while the
/// upstream identity provider handles authorization and token issuance.
///
/// A host whose bearer must carry more than the sign-in scopes lists them in
/// `additional_scopes`:
///
/// ```
/// use proxima_mcp_server::{AuthorizationServers, ProtectedResource, ResourceServerMetadata, ScopeToken};
///
/// let metadata = ResourceServerMetadata {
///     public_url: "https://erp.example.com".to_string(),
///     authorization_servers: AuthorizationServers::Issuers(vec!["https://auth.example.com".to_string()]),
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
    /// Direct issuer URLs or this origin's stateless public-client facade.
    pub authorization_servers: AuthorizationServers,
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
        let authorization_servers = match &self.authorization_servers {
            AuthorizationServers::Issuers(issuers) => issuers.clone(),
            AuthorizationServers::PublicClient(_) => vec![self.origin().to_string()],
        };
        serde_json::json!({
            "resource": self.resource(which),
            "authorization_servers": authorization_servers,
            "bearer_methods_supported": ["header"],
            "scopes_supported": self.scopes_supported(),
        })
    }

    #[must_use]
    pub fn www_authenticate_value(&self, which: ProtectedResource) -> String {
        format!("Bearer resource_metadata=\"{}\"", self.metadata_url(which))
    }
}

/// How clients discover the authorization server for a protected resource.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthorizationServers {
    /// Clients go to these issuers directly.
    Issuers(Vec<String>),
    /// This origin fronts the upstream identity provider for one public PKCE
    /// client: for a provider without dynamic client registration, whose
    /// clients could otherwise not register. Every registration receives
    /// that client's id; sign-in and tokens stay the provider's, so nothing
    /// is stored here.
    PublicClient(PublicClientFacade),
}

/// Upstream endpoints and client identifier used by the stateless facade.
///
/// The constructor is the only way in; a literal cannot skip validation:
/// ```compile_fail,E0451
/// let _ = proxima_mcp_server::PublicClientFacade {
///     client_id: String::new(),
///     authorization_endpoint: String::new(),
///     token_endpoint: String::new(),
/// };
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicClientFacade {
    client_id: String,
    authorization_endpoint: String,
    token_endpoint: String,
}

impl PublicClientFacade {
    /// Creates a validated facade configuration.
    /// # Errors
    ///
    /// Returns an error when the client ID or either endpoint violates its invariant.
    pub fn try_new(
        client_id: impl Into<String>,
        authorization_endpoint: impl AsRef<str>,
        token_endpoint: impl AsRef<str>,
    ) -> Result<Self, PublicClientFacadeError> {
        let client_id = client_id.into();
        if client_id.is_empty() || client_id.chars().any(char::is_whitespace) {
            return Err(PublicClientFacadeError::InvalidClientId);
        }
        let authorization_endpoint = validate_endpoint(authorization_endpoint.as_ref())?;
        let token_endpoint = validate_endpoint(token_endpoint.as_ref())?;
        Ok(Self {
            client_id,
            authorization_endpoint,
            token_endpoint,
        })
    }

    #[must_use]
    pub fn client_id(&self) -> &str {
        &self.client_id
    }
    #[must_use]
    pub fn authorization_endpoint(&self) -> &str {
        &self.authorization_endpoint
    }
    #[must_use]
    pub fn token_endpoint(&self) -> &str {
        &self.token_endpoint
    }
}

fn validate_endpoint(raw: &str) -> Result<String, PublicClientFacadeError> {
    if raw
        .chars()
        .any(|character| character.is_whitespace() || character.is_control())
    {
        return Err(PublicClientFacadeError::InvalidEndpoint);
    }
    let url = Url::parse(raw).map_err(|_| PublicClientFacadeError::InvalidEndpoint)?;
    let valid_scheme = url.scheme() == "https"
        || (url.scheme() == "http"
            && match url.host() {
                Some(url::Host::Domain(host)) => host == "localhost",
                Some(url::Host::Ipv4(address)) => address.is_loopback(),
                Some(url::Host::Ipv6(address)) => address.is_loopback(),
                None => false,
            });
    // Both endpoints are published unauthenticated: userinfo would leak a
    // credential, and a fragment is not allowed in an OAuth endpoint.
    let has_userinfo = !url.username().is_empty() || url.password().is_some();
    if !valid_scheme || url.host_str().is_none() || has_userinfo || url.fragment().is_some() {
        return Err(PublicClientFacadeError::InvalidEndpoint);
    }
    Ok(raw.to_owned())
}

/// Why a public-client facade could not be constructed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PublicClientFacadeError {
    #[error("client id must be non-empty and contain no whitespace")]
    InvalidClientId,
    #[error(
        "endpoint must be an absolute https URL, or http on loopback, without userinfo or fragment"
    )]
    InvalidEndpoint,
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

/// Returns the complete unauthenticated discovery surface for this metadata.
#[must_use = "merge the returned router into the public MCP HTTP surface"]
pub fn public_discovery_router(metadata: &ResourceServerMetadata) -> Router {
    let router = [ProtectedResource::Origin, ProtectedResource::Mcp]
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
        });
    let AuthorizationServers::PublicClient(facade) = &metadata.authorization_servers else {
        return router;
    };
    let origin = metadata.origin().to_string();
    let as_json = serde_json::json!({
        "issuer": origin,
        "authorization_endpoint": facade.authorization_endpoint(),
        "token_endpoint": facade.token_endpoint(),
        "registration_endpoint": format!("{}{REGISTRATION_PATH}", metadata.origin()),
        "response_types_supported": ["code"],
        "grant_types_supported": GRANT_TYPES,
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
        "scopes_supported": metadata.scopes_supported(),
    })
    .to_string();
    let first_body = as_json.clone();
    let second_body = as_json;
    let facade = facade.clone();
    router
        .route(
            AUTHORIZATION_SERVER_METADATA_PATH,
            get(move || {
                let body = first_body.clone();
                async move { ([(CONTENT_TYPE, "application/json")], body) }
            }),
        )
        .route(
            MCP_AUTHORIZATION_SERVER_METADATA_PATH,
            get(move || {
                let body = second_body.clone();
                async move { ([(CONTENT_TYPE, "application/json")], body) }
            }),
        )
        // The upstream public client's redirect allowlist is the sole enforcer;
        // duplicating it here would drift while adding no protection.
        .route(
            REGISTRATION_PATH,
            post(move |body: Bytes| {
                let facade = facade.clone();
                async move { register(&body, &facade) }
            }),
        )
}

#[derive(serde::Deserialize)]
struct RegistrationRequest {
    redirect_uris: Option<serde_json::Value>,
    client_name: Option<String>,
    /// Present-but-`null` must refuse too, so this stays a raw value.
    #[serde(default, deserialize_with = "present")]
    token_endpoint_auth_method: Option<serde_json::Value>,
}

/// `Some(value)` for any present field, `null` included.
fn present<'de, D>(deserializer: D) -> Result<Option<serde_json::Value>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    serde::Deserialize::deserialize(deserializer).map(Some)
}

/// Handles dynamic client registration without persistence.
///
/// Redirect hosts are deliberately not filtered here: the upstream public
/// client's redirect allowlist is the single enforcer and this facade is stateless.
fn register(body: &[u8], facade: &PublicClientFacade) -> Response {
    let Ok(request) = serde_json::from_slice::<RegistrationRequest>(body) else {
        return registration_error("invalid_client_metadata", "Request body must be JSON");
    };
    let Some(redirect_uris) = request.redirect_uris else {
        return registration_error(
            "invalid_redirect_uri",
            "redirect_uris must be a non-empty array",
        );
    };
    let Some(redirect_uris) = redirect_uris.as_array() else {
        return registration_error(
            "invalid_redirect_uri",
            "redirect_uris must be a non-empty array",
        );
    };
    let redirect_uris = redirect_uris
        .iter()
        .map(serde_json::Value::as_str)
        .collect::<Option<Vec<_>>>();
    let Some(redirect_uris) = redirect_uris else {
        return registration_error("invalid_redirect_uri", "redirect_uris must contain strings");
    };
    if redirect_uris.is_empty() || redirect_uris.iter().any(|uri| Url::parse(uri).is_err()) {
        return registration_error(
            "invalid_redirect_uri",
            "redirect_uris must contain absolute URLs",
        );
    }
    if request
        .token_endpoint_auth_method
        .is_some_and(|method| method != "none")
    {
        return registration_error(
            "invalid_client_metadata",
            "token_endpoint_auth_method must be none",
        );
    }
    let mut response = serde_json::json!({
        "client_id": facade.client_id(),
        "redirect_uris": redirect_uris,
        "token_endpoint_auth_method": "none",
        "grant_types": GRANT_TYPES,
        "response_types": ["code"],
    });
    if let Some(client_name) = request.client_name {
        response["client_name"] = serde_json::Value::String(client_name);
    }
    (
        StatusCode::CREATED,
        [(CONTENT_TYPE, "application/json")],
        response.to_string(),
    )
        .into_response()
}

fn registration_error(code: &str, description: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        [(CONTENT_TYPE, "application/json")],
        serde_json::json!({"error": code, "error_description": description}).to_string(),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use super::{
        AuthorizationServers, ProtectedResource, PublicClientFacade, PublicClientFacadeError,
        ResourceChallenges, ResourceServerMetadata, ScopeToken, ScopeTokenError, register,
    };

    fn metadata() -> ResourceServerMetadata {
        ResourceServerMetadata {
            public_url: "https://proxima.example.com".to_string(),
            authorization_servers: AuthorizationServers::Issuers(vec![
                "https://idp.example.com".to_string(),
            ]),
            additional_scopes: Vec::new(),
        }
    }

    #[test]
    fn public_client_facade_validates_once() {
        assert_eq!(
            PublicClientFacade::try_new(
                "",
                "https://idp.example/authorize",
                "https://idp.example/token"
            ),
            Err(PublicClientFacadeError::InvalidClientId)
        );
        assert_eq!(
            PublicClientFacade::try_new(
                "two words",
                "https://idp.example/authorize",
                "https://idp.example/token"
            ),
            Err(PublicClientFacadeError::InvalidClientId)
        );
        assert_eq!(
            PublicClientFacade::try_new(
                "client",
                "http://example.test/authorize",
                "https://idp.example/token"
            ),
            Err(PublicClientFacadeError::InvalidEndpoint)
        );
        let facade = PublicClientFacade::try_new(
            "client",
            "http://127.0.0.2:8080/authorize",
            "https://idp.example/token",
        )
        .expect("loopback HTTP endpoint is valid");
        assert_eq!(
            facade.authorization_endpoint(),
            "http://127.0.0.2:8080/authorize"
        );
        assert!(
            PublicClientFacade::try_new(
                "client",
                "http://[::1]:8080/authorize",
                "https://idp.example/token",
            )
            .is_ok()
        );
        for endpoint in [
            "https://user:secret@idp.example/authorize",
            "https://user@idp.example/authorize",
            "https://idp.example/authorize#fragment",
        ] {
            assert_eq!(
                PublicClientFacade::try_new("client", endpoint, "https://idp.example/token"),
                Err(PublicClientFacadeError::InvalidEndpoint),
                "{endpoint}"
            );
        }
        assert_eq!(
            PublicClientFacade::try_new(
                "client",
                " https://idp.example/authorize",
                "https://idp.example/token",
            ),
            Err(PublicClientFacadeError::InvalidEndpoint)
        );
    }

    #[test]
    fn registration_rejects_null_auth_method_and_omits_missing_name() {
        let facade = PublicClientFacade::try_new(
            "client",
            "https://idp.example/authorize",
            "https://idp.example/token",
        )
        .expect("test facade");
        let response = register(
            br#"{"redirect_uris":["https://client.example/cb"],"token_endpoint_auth_method":null}"#,
            &facade,
        );
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let response = register(
            br#"{"redirect_uris":["https://client.example/cb"]}"#,
            &facade,
        );
        assert_eq!(response.status(), StatusCode::CREATED);
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

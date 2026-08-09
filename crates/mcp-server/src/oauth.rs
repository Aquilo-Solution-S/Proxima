//! OAuth 2.0 Protected Resource Metadata (RFC 9728) for the deployment.

use axum::{Router, http::header::CONTENT_TYPE, routing::get};

/// The single unauthenticated discovery path.
pub const PROTECTED_RESOURCE_METADATA_PATH: &str = "/.well-known/oauth-protected-resource";

/// Advertised resource-server metadata pointing MCP clients at the `IdP`.
#[derive(Clone, Debug)]
pub struct ResourceServerMetadata {
    /// Public base URL (scheme+host, no trailing slash), e.g. `https://proxima.example.com`.
    pub public_url: String,
    /// Authorization servers (the Zitadel issuer URL(s)).
    pub authorization_servers: Vec<String>,
}

impl ResourceServerMetadata {
    /// The RFC 9728 protected-resource identifier: the deployment's public
    /// origin, covering every surface it serves.
    ///
    /// It used to be `{public_url}/mcp`. The public origin is now the one RFC
    /// 8707 `resource` and token audience for both `/mcp` and an enabled
    /// `/v1`; clients therefore do not need surface-specific credentials.
    /// See `MIGRATING.md` for the completed audience migration.
    #[must_use]
    pub fn resource(&self) -> String {
        self.public_url.trim_end_matches('/').to_string()
    }

    #[must_use]
    pub fn metadata_url(&self) -> String {
        format!(
            "{}{}",
            self.public_url.trim_end_matches('/'),
            PROTECTED_RESOURCE_METADATA_PATH
        )
    }

    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "resource": self.resource(),
            "authorization_servers": self.authorization_servers,
            "bearer_methods_supported": ["header"],
        })
    }

    #[must_use]
    pub fn www_authenticate_value(&self) -> String {
        format!("Bearer resource_metadata=\"{}\"", self.metadata_url())
    }
}

/// A router exposing only the unauthenticated discovery document. Merge this
/// *after* the auth layer so it bypasses bearer enforcement.
#[must_use = "merge the returned router into the public MCP HTTP surface"]
pub fn protected_resource_router(metadata: &ResourceServerMetadata) -> Router {
    let body = metadata.to_json().to_string();
    Router::new().route(
        PROTECTED_RESOURCE_METADATA_PATH,
        get(move || {
            let body = body.clone();
            async move { ([(CONTENT_TYPE, "application/json")], body) }
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::ResourceServerMetadata;

    fn metadata() -> ResourceServerMetadata {
        ResourceServerMetadata {
            public_url: "https://proxima.example.com".to_string(),
            authorization_servers: vec!["https://idp.example.com".to_string()],
        }
    }

    /// One identifier for the whole deployment, not one per surface — so a
    /// token minted for it reaches `/mcp` and `/v1` alike. A regression to
    /// `{public_url}/mcp` would silently re-split the audience.
    #[test]
    fn resource_is_the_public_origin_not_a_per_surface_path() {
        assert_eq!(metadata().resource(), "https://proxima.example.com");
        assert!(!metadata().resource().ends_with("/mcp"));

        let trailing = ResourceServerMetadata {
            public_url: "https://proxima.example.com/".to_string(),
            authorization_servers: vec!["https://idp.example.com".to_string()],
        };
        assert_eq!(trailing.resource(), "https://proxima.example.com");
    }

    #[test]
    fn metadata_url_uses_well_known_path() {
        assert_eq!(
            metadata().metadata_url(),
            "https://proxima.example.com/.well-known/oauth-protected-resource"
        );
    }

    #[test]
    fn json_contains_rfc_9728_fields() {
        let json = metadata().to_json();

        assert_eq!(json["resource"], "https://proxima.example.com");
        assert_eq!(json["authorization_servers"][0], "https://idp.example.com");
        assert_eq!(json["bearer_methods_supported"][0], "header");
    }

    #[test]
    fn www_authenticate_header_value_points_to_metadata() {
        assert_eq!(
            metadata().www_authenticate_value(),
            "Bearer resource_metadata=\"https://proxima.example.com/.well-known/oauth-protected-resource\""
        );
    }
}

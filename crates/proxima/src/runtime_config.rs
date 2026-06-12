use std::net::SocketAddr;
use std::sync::Arc;

use proxima_blob_s3::S3RuntimeConfig;
use proxima_core::{AnthropicClient, Authenticator, EmbeddingClient, Owner};

use crate::config::{parse_bool_value, s3_from_lookup};
use crate::{EmbedError, company_owner};

const DEFAULT_MCP_BIND: &str = "127.0.0.1:31415";

/// Runtime configuration builder for host applications.
#[derive(Default)]
pub struct RuntimeBuilder {
    database_url: Option<String>,
    s3: Option<S3RuntimeConfig>,
    owner: Option<Owner>,
    org_id: Option<uuid::Uuid>,
    mcp_enabled: bool,
    mcp_bind: Option<SocketAddr>,
    expose_network: Option<bool>,
    allowed_origins: Option<Vec<String>>,
    insecure_single_owner: bool,
    authenticator: Option<Arc<dyn Authenticator>>,
    embed_client: Option<Arc<dyn EmbeddingClient>>,
    anthropic: Option<Arc<dyn AnthropicClient>>,
}

impl std::fmt::Debug for RuntimeBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeBuilder")
            .field("database_url", &self.database_url)
            .field("s3", &self.s3)
            .field("owner", &self.owner)
            .field("org_id", &self.org_id)
            .field("mcp_enabled", &self.mcp_enabled)
            .field("mcp_bind", &self.mcp_bind)
            .field("expose_network", &self.expose_network)
            .field("allowed_origins", &self.allowed_origins)
            .field("insecure_single_owner", &self.insecure_single_owner)
            .field("has_authenticator", &self.authenticator.is_some())
            .field("has_embed_client", &self.embed_client.is_some())
            .field("has_anthropic", &self.anthropic.is_some())
            .finish()
    }
}

impl RuntimeBuilder {
    #[must_use]
    pub(crate) fn merge_over(self, base: Self) -> Self {
        Self {
            database_url: self.database_url.or(base.database_url),
            s3: self.s3.or(base.s3),
            owner: self.owner.or(base.owner),
            org_id: self.org_id.or(base.org_id),
            mcp_enabled: self.mcp_enabled || base.mcp_enabled,
            mcp_bind: self.mcp_bind.or(base.mcp_bind),
            expose_network: self.expose_network.or(base.expose_network),
            allowed_origins: self.allowed_origins.or(base.allowed_origins),
            insecure_single_owner: self.insecure_single_owner || base.insecure_single_owner,
            authenticator: self.authenticator.or(base.authenticator),
            embed_client: self.embed_client.or(base.embed_client),
            anthropic: self.anthropic.or(base.anthropic),
        }
    }

    #[must_use]
    pub fn database_url(mut self, database_url: impl Into<String>) -> Self {
        self.database_url = Some(database_url.into());
        self
    }

    #[must_use]
    pub fn s3(mut self, s3: S3RuntimeConfig) -> Self {
        self.s3 = Some(s3);
        self
    }

    #[must_use]
    pub fn owner(mut self, owner: Owner) -> Self {
        self.owner = Some(owner);
        self
    }

    #[must_use]
    pub fn org_id(mut self, org_id: uuid::Uuid) -> Self {
        self.org_id = Some(org_id);
        self
    }

    #[must_use]
    pub fn with_mcp(mut self) -> Self {
        self.mcp_enabled = true;
        self
    }

    #[must_use]
    pub fn mcp_bind(mut self, bind: SocketAddr) -> Self {
        self.mcp_enabled = true;
        self.mcp_bind = Some(bind);
        self
    }

    #[must_use]
    pub fn expose_network(mut self, expose_network: bool) -> Self {
        self.expose_network = Some(expose_network);
        self
    }

    #[must_use]
    pub fn allowed_origins(mut self, allowed_origins: Vec<String>) -> Self {
        self.allowed_origins = Some(allowed_origins);
        self
    }

    #[must_use]
    pub fn allow_insecure_single_owner(mut self) -> Self {
        self.insecure_single_owner = true;
        self
    }

    #[must_use]
    pub fn authenticator(mut self, authenticator: Arc<dyn Authenticator>) -> Self {
        self.authenticator = Some(authenticator);
        self
    }

    #[must_use]
    pub fn embed_client(mut self, client: Arc<dyn EmbeddingClient>) -> Self {
        self.embed_client = Some(client);
        self
    }

    #[must_use]
    pub fn anthropic(mut self, client: Arc<dyn AnthropicClient>) -> Self {
        self.anthropic = Some(client);
        self
    }

    /// Apply process environment variables to unset fields.
    ///
    /// # Errors
    ///
    /// Returns `ProximaError::Config` when an environment value is malformed.
    pub fn apply_env(self) -> Result<Self, ProximaError> {
        self.apply_lookup(|key| std::env::var(key).ok())
    }

    /// Apply injected environment values to unset fields.
    ///
    /// # Errors
    ///
    /// Returns `ProximaError::Config` when a supplied value is malformed.
    pub fn apply_lookup(
        mut self,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, ProximaError> {
        if self.database_url.is_none() {
            self.database_url = lookup("DATABASE_URL");
        }
        if self.s3.is_none() {
            self.s3 = s3_from_lookup(&lookup)?;
        }
        if self.owner.is_none() && self.org_id.is_none() {
            self.org_id = lookup("PROXIMA_ORG_ID")
                .map(|raw| {
                    raw.parse().map_err(|_| {
                        ProximaError::Config(format!("PROXIMA_ORG_ID must be a UUID, got {raw:?}"))
                    })
                })
                .transpose()?;
        }
        if self.mcp_bind.is_none()
            && let Some(raw) = lookup("PROXIMA_MCP_BIND")
        {
            self.mcp_bind = Some(raw.parse().map_err(|_| {
                ProximaError::Config(format!(
                    "PROXIMA_MCP_BIND must be a socket address, got {raw:?}"
                ))
            })?);
            self.mcp_enabled = true;
        }
        if self.expose_network.is_none() {
            self.expose_network = lookup("PROXIMA_EXPOSE_NETWORK")
                .map(|raw| parse_bool_value("PROXIMA_EXPOSE_NETWORK", &raw))
                .transpose()?;
        }
        if self.allowed_origins.is_none() {
            self.allowed_origins =
                lookup("PROXIMA_ALLOWED_ORIGINS").map(|raw| parse_allowed_origins(&raw));
        }
        Ok(self)
    }

    /// Resolve the builder into pure config plus host-provided runtime parts.
    ///
    /// # Errors
    ///
    /// Returns `ProximaError::Config` for missing required config and
    /// `ProximaError::Security` for fail-closed validation failures.
    pub fn resolve(self) -> Result<(RuntimeConfig, RuntimeParts), ProximaError> {
        let database_url = self
            .database_url
            .ok_or_else(|| ProximaError::Config("DATABASE_URL is required".into()))?;
        let owner = match (self.owner, self.org_id) {
            (Some(owner), _) => owner,
            (None, Some(org_id)) => company_owner(org_id),
            (None, None) => {
                return Err(ProximaError::Config(
                    "owner or PROXIMA_ORG_ID is required".into(),
                ));
            }
        };
        let mcp = if self.mcp_enabled {
            Some(McpSettings {
                bind: self.mcp_bind.unwrap_or_else(default_mcp_bind),
            })
        } else {
            None
        };
        let parts = RuntimeParts {
            authenticator: self.authenticator,
            embed_client: self.embed_client,
            anthropic: self.anthropic,
        };
        let config = RuntimeConfig {
            database_url,
            s3: self.s3,
            owner,
            mcp,
            expose_network: self.expose_network.unwrap_or(false),
            allowed_origins: self.allowed_origins.unwrap_or_default(),
            insecure_single_owner: self.insecure_single_owner,
            has_host_authenticator: parts.authenticator.is_some(),
        };
        config.validate()?;
        Ok((config, parts))
    }
}

/// Pure, validated runtime config.
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    pub database_url: String,
    pub s3: Option<S3RuntimeConfig>,
    pub owner: Owner,
    pub mcp: Option<McpSettings>,
    pub expose_network: bool,
    pub allowed_origins: Vec<String>,
    pub insecure_single_owner: bool,
    pub has_host_authenticator: bool,
}

impl RuntimeConfig {
    /// Validate the fail-closed network/auth matrix.
    ///
    /// # Errors
    ///
    /// Returns `ProximaError::Security` when transport exposure is unsafe.
    pub fn validate(&self) -> Result<(), ProximaError> {
        let Some(mcp) = &self.mcp else {
            return Ok(());
        };

        if self.insecure_single_owner && self.expose_network {
            return Err(ProximaError::Security(
                "insecure single-owner mode cannot expose network transports".into(),
            ));
        }
        if self.insecure_single_owner && !mcp.bind.ip().is_loopback() {
            return Err(ProximaError::Security(
                "insecure single-owner MCP bind must be loopback".into(),
            ));
        }
        if self.expose_network && self.allowed_origins.is_empty() {
            return Err(ProximaError::Security(
                "network exposure requires at least one allowed origin".into(),
            ));
        }
        if self.expose_network && self.allowed_origins.iter().any(|origin| origin == "*") {
            return Err(ProximaError::Security(
                "network exposure forbids wildcard allowed origins".into(),
            ));
        }
        if self.expose_network && !self.has_host_authenticator {
            return Err(ProximaError::Security(
                "network exposure requires a host authenticator".into(),
            ));
        }
        if !self.expose_network && !mcp.bind.ip().is_loopback() {
            return Err(ProximaError::Security(
                "non-exposed MCP bind must be loopback".into(),
            ));
        }
        if !self.has_host_authenticator && !self.insecure_single_owner {
            return Err(ProximaError::Security(
                "MCP requires a host authenticator or insecure single-owner mode".into(),
            ));
        }

        Ok(())
    }
}

/// MCP transport settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McpSettings {
    pub bind: SocketAddr,
}

/// Host-provided runtime objects kept out of pure config.
#[derive(Clone, Default)]
pub struct RuntimeParts {
    pub authenticator: Option<Arc<dyn Authenticator>>,
    pub embed_client: Option<Arc<dyn EmbeddingClient>>,
    pub anthropic: Option<Arc<dyn AnthropicClient>>,
}

impl std::fmt::Debug for RuntimeParts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeParts")
            .field("has_authenticator", &self.authenticator.is_some())
            .field("has_embed_client", &self.embed_client.is_some())
            .field("has_anthropic", &self.anthropic.is_some())
            .finish()
    }
}

/// Errors from the application-facing facade.
#[derive(Debug, thiserror::Error)]
pub enum ProximaError {
    #[error("config: {0}")]
    Config(String),
    #[error("storage: {0}")]
    Storage(String),
    #[error("engine: {0}")]
    Engine(String),
    #[error("security: {0}")]
    Security(String),
    #[error("mcp: {0}")]
    Mcp(String),
}

impl From<EmbedError> for ProximaError {
    fn from(value: EmbedError) -> Self {
        match value {
            EmbedError::Config(err) => Self::Config(err),
            EmbedError::Storage(err) => Self::Storage(err),
            EmbedError::Engine(err) => Self::Engine(err),
        }
    }
}

fn default_mcp_bind() -> SocketAddr {
    DEFAULT_MCP_BIND
        .parse()
        .expect("DEFAULT_MCP_BIND must be a valid SocketAddr")
}

fn parse_allowed_origins(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|origin| !origin.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use proxima_core::{GroupId, OrgId, Principal};

    use super::*;

    fn lookup<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| {
            pairs
                .iter()
                .find(|(candidate, _)| *candidate == key)
                .map(|(_, value)| (*value).to_string())
        }
    }

    fn owner(id: uuid::Uuid) -> Owner {
        Owner {
            principal: Principal::Group(GroupId::new(id)),
            org_id: OrgId::new(id),
        }
    }

    fn base_config(mcp: Option<SocketAddr>) -> RuntimeConfig {
        RuntimeConfig {
            database_url: "postgres://localhost/proxima".to_string(),
            s3: None,
            owner: company_owner(uuid::Uuid::now_v7()),
            mcp: mcp.map(|bind| McpSettings { bind }),
            expose_network: false,
            allowed_origins: Vec::new(),
            insecure_single_owner: false,
            has_host_authenticator: true,
        }
    }

    fn addr(ip: [u8; 4]) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::from(ip)), 31415)
    }

    #[test]
    fn precedence_env_fills_unset_and_preserves_explicit() {
        let builder = RuntimeBuilder::default()
            .database_url("postgres://explicit/proxima")
            .apply_lookup(lookup(&[
                ("DATABASE_URL", "postgres://env/proxima"),
                ("PROXIMA_ALLOWED_ORIGINS", "https://a.test, https://b.test"),
            ]))
            .unwrap();

        assert_eq!(
            builder.database_url.as_deref(),
            Some("postgres://explicit/proxima")
        );
        assert_eq!(
            builder.allowed_origins.as_deref(),
            Some(["https://a.test".to_string(), "https://b.test".to_string()].as_slice())
        );
    }

    #[test]
    fn validate_mcp_requires_authenticator_or_insecure_single_owner() {
        let mut config = base_config(Some(addr([127, 0, 0, 1])));
        config.has_host_authenticator = false;

        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("MCP requires"));
    }

    #[test]
    fn validate_insecure_single_owner_cannot_expose_network() {
        let mut config = base_config(Some(addr([127, 0, 0, 1])));
        config.insecure_single_owner = true;
        config.expose_network = true;
        config.allowed_origins = vec!["https://app.test".to_string()];

        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("cannot expose network"));
    }

    #[test]
    fn validate_insecure_single_owner_requires_loopback_mcp_bind() {
        let mut config = base_config(Some(addr([0, 0, 0, 0])));
        config.insecure_single_owner = true;
        config.has_host_authenticator = false;

        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("single-owner MCP bind"));
    }

    #[test]
    fn validate_exposed_network_requires_allowed_origins() {
        let mut config = base_config(Some(addr([127, 0, 0, 1])));
        config.expose_network = true;

        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("at least one allowed origin"));
    }

    #[test]
    fn validate_exposed_network_rejects_wildcard_allowed_origin() {
        let mut config = base_config(Some(addr([127, 0, 0, 1])));
        config.expose_network = true;
        config.allowed_origins = vec!["*".to_string()];

        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("wildcard"));
    }

    #[test]
    fn validate_exposed_network_requires_host_authenticator() {
        let mut config = base_config(Some(addr([127, 0, 0, 1])));
        config.expose_network = true;
        config.allowed_origins = vec!["https://app.test".to_string()];
        config.has_host_authenticator = false;

        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("host authenticator"));
    }

    #[test]
    fn validate_non_exposed_mcp_requires_loopback_bind() {
        let config = base_config(Some(addr([0, 0, 0, 0])));

        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("must be loopback"));
    }

    #[test]
    fn validate_without_mcp_is_ok_without_authenticator() {
        let mut config = base_config(None);
        config.has_host_authenticator = false;
        config.expose_network = true;

        config.validate().unwrap();
    }

    #[test]
    fn org_id_env_resolves_to_company_owner() {
        let org_id = uuid::Uuid::now_v7();
        let org_id_raw = org_id.to_string();
        let (config, _) = RuntimeBuilder::default()
            .database_url("postgres://localhost/proxima")
            .apply_lookup(lookup(&[("PROXIMA_ORG_ID", &org_id_raw)]))
            .unwrap()
            .resolve()
            .unwrap();

        assert_eq!(config.owner, company_owner(org_id));
    }

    #[test]
    fn explicit_owner_wins_over_org_id_env() {
        let explicit = owner(uuid::Uuid::now_v7());
        let env_org = uuid::Uuid::now_v7();
        let env_org_raw = env_org.to_string();
        let (config, _) = RuntimeBuilder::default()
            .database_url("postgres://localhost/proxima")
            .owner(explicit.clone())
            .apply_lookup(lookup(&[("PROXIMA_ORG_ID", &env_org_raw)]))
            .unwrap()
            .resolve()
            .unwrap();

        assert_eq!(config.owner, explicit);
    }

    #[test]
    fn missing_owner_or_org_id_errors() {
        let err = RuntimeBuilder::default()
            .database_url("postgres://localhost/proxima")
            .resolve()
            .unwrap_err();

        assert!(err.to_string().contains("owner or PROXIMA_ORG_ID"));
    }

    #[test]
    fn allowed_origins_are_split_trimmed_and_empty_values_dropped() {
        let builder = RuntimeBuilder::default()
            .apply_lookup(lookup(&[(
                "PROXIMA_ALLOWED_ORIGINS",
                " https://a.test, ,https://b.test, ",
            )]))
            .unwrap();

        assert_eq!(
            builder.allowed_origins.unwrap(),
            ["https://a.test".to_string(), "https://b.test".to_string()]
        );
    }

    #[test]
    fn malformed_mcp_bind_errors() {
        let err = RuntimeBuilder::default()
            .apply_lookup(lookup(&[("PROXIMA_MCP_BIND", "not-a-socket")]))
            .unwrap_err();

        assert!(err.to_string().contains("PROXIMA_MCP_BIND"));
    }

    #[test]
    fn default_mcp_bind_is_loopback_when_enabled_without_bind() {
        let (config, _) = RuntimeBuilder::default()
            .database_url("postgres://localhost/proxima")
            .owner(owner(uuid::Uuid::now_v7()))
            .with_mcp()
            .allow_insecure_single_owner()
            .resolve()
            .unwrap();

        assert_eq!(config.mcp.unwrap().bind, default_mcp_bind());
    }

    #[test]
    fn merge_over_prefers_self_options_and_ors_flags() {
        let base = RuntimeBuilder::default()
            .database_url("postgres://base/proxima")
            .owner(owner(uuid::Uuid::now_v7()))
            .mcp_bind(addr([127, 0, 0, 1]));
        let overlay = RuntimeBuilder::default()
            .database_url("postgres://overlay/proxima")
            .allowed_origins(vec!["https://overlay.test".to_string()])
            .allow_insecure_single_owner();

        let merged = overlay.merge_over(base);

        assert_eq!(
            merged.database_url.as_deref(),
            Some("postgres://overlay/proxima")
        );
        assert!(merged.owner.is_some());
        assert!(merged.mcp_enabled);
        assert!(merged.mcp_bind.is_some());
        assert_eq!(
            merged.allowed_origins.as_deref(),
            Some(["https://overlay.test".to_string()].as_slice())
        );
        assert!(merged.insecure_single_owner);
    }
}

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use proxima_blob_s3::S3RuntimeConfig;
use proxima_core::{
    AnthropicClient, Authenticator, EmbeddingClient, FlavorServiceError, Owner, RevalidationConfig,
    ToolScope, is_loopback_host,
};
use proxima_mcp_server::ResourceServerMetadata;
use proxima_storage_pg::PgTuning;

use crate::EmbedError;
use crate::config::{parse_bool_value, pg_tuning_from_lookup, s3_from_lookup};

const DEFAULT_MCP_BIND: &str = "127.0.0.1:31415";

/// Runtime configuration builder for host applications.
#[derive(Default)]
pub struct RuntimeBuilder {
    database_url: Option<String>,
    s3: Option<S3RuntimeConfig>,
    owner: Option<Owner>,
    mcp_enabled: bool,
    mcp_bind: Option<SocketAddr>,
    expose_network: Option<bool>,
    allowed_origins: Option<Vec<String>>,
    allowed_hosts: Option<Vec<String>>,
    tool_scope: Option<ToolScope>,
    stream_max_lifetime: Option<Duration>,
    epoch_check_interval: Option<Duration>,
    insecure_single_owner: bool,
    rest_enabled: Option<bool>,
    skip_migrations: Option<bool>,
    pg_tuning: Option<PgTuning>,
    authenticator: Option<Arc<dyn Authenticator>>,
    resource_metadata: Option<ResourceServerMetadata>,
    embed_client: Option<Arc<dyn EmbeddingClient>>,
    anthropic: Option<Arc<dyn AnthropicClient>>,
}

impl std::fmt::Debug for RuntimeBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeBuilder")
            .field("has_database_url", &self.database_url.is_some())
            .field("s3", &self.s3)
            .field("owner", &self.owner)
            .field("mcp_enabled", &self.mcp_enabled)
            .field("mcp_bind", &self.mcp_bind)
            .field("expose_network", &self.expose_network)
            .field("allowed_origins", &self.allowed_origins)
            .field("allowed_hosts", &self.allowed_hosts)
            .field("tool_scope", &self.tool_scope)
            .field("stream_max_lifetime", &self.stream_max_lifetime)
            .field("epoch_check_interval", &self.epoch_check_interval)
            .field("insecure_single_owner", &self.insecure_single_owner)
            .field("rest_enabled", &self.rest_enabled)
            .field("skip_migrations", &self.skip_migrations)
            .field("pg_tuning", &self.pg_tuning)
            .field("has_authenticator", &self.authenticator.is_some())
            .field("has_resource_metadata", &self.resource_metadata.is_some())
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
            mcp_enabled: self.mcp_enabled || base.mcp_enabled,
            mcp_bind: self.mcp_bind.or(base.mcp_bind),
            expose_network: self.expose_network.or(base.expose_network),
            allowed_origins: self.allowed_origins.or(base.allowed_origins),
            allowed_hosts: self.allowed_hosts.or(base.allowed_hosts),
            tool_scope: self.tool_scope.or(base.tool_scope),
            stream_max_lifetime: self.stream_max_lifetime.or(base.stream_max_lifetime),
            epoch_check_interval: self.epoch_check_interval.or(base.epoch_check_interval),
            insecure_single_owner: self.insecure_single_owner || base.insecure_single_owner,
            rest_enabled: self.rest_enabled.or(base.rest_enabled),
            skip_migrations: self.skip_migrations.or(base.skip_migrations),
            pg_tuning: self.pg_tuning.or(base.pg_tuning),
            authenticator: self.authenticator.or(base.authenticator),
            resource_metadata: self.resource_metadata.or(base.resource_metadata),
            embed_client: self.embed_client.or(base.embed_client),
            anthropic: self.anthropic.or(base.anthropic),
        }
    }

    /// Set the Postgres connection string. Env equivalent: `DATABASE_URL`.
    #[must_use]
    pub fn database_url(mut self, database_url: impl Into<String>) -> Self {
        self.database_url = Some(database_url.into());
        self
    }

    /// Configure the cited-blob S3 store. Env equivalent: the `PROXIMA_S3_*` block.
    #[must_use]
    pub fn s3(mut self, s3: S3RuntimeConfig) -> Self {
        self.s3 = Some(s3);
        self
    }

    /// Set the engine `Owner` (= principal) explicitly.
    #[must_use]
    pub fn owner(mut self, owner: Owner) -> Self {
        self.owner = Some(owner);
        self
    }

    /// Enable the MCP transport on the default loopback bind.
    #[must_use]
    pub fn with_mcp(mut self) -> Self {
        self.mcp_enabled = true;
        self
    }

    /// Enable MCP and bind it to `bind`. Env equivalent: `PROXIMA_MCP_BIND`.
    #[must_use]
    pub fn mcp_bind(mut self, bind: SocketAddr) -> Self {
        self.mcp_enabled = true;
        self.mcp_bind = Some(bind);
        self
    }

    /// Allow non-loopback MCP exposure (requires an authenticator and
    /// allowed origins). Env equivalent: `PROXIMA_EXPOSE_NETWORK`.
    #[must_use]
    pub fn expose_network(mut self, expose_network: bool) -> Self {
        self.expose_network = Some(expose_network);
        self
    }

    /// Set the CORS origin allowlist for exposed MCP. Env equivalent
    /// (comma-separated): `PROXIMA_ALLOWED_ORIGINS`.
    #[must_use]
    pub fn allowed_origins(mut self, allowed_origins: Vec<String>) -> Self {
        self.allowed_origins = Some(allowed_origins);
        self
    }

    /// Set the inbound `Host` allowlist for the shared HTTP listener.
    /// Entries are bare hostnames or `host:port`; loopback is always added
    /// on top. The same non-empty allowlist guards every listener route and
    /// rmcp's inner `/mcp` service. When unset, public hosts are derived from
    /// `PROXIMA_PUBLIC_URL` and the allowed origins. Env equivalent
    /// (comma-separated): `PROXIMA_ALLOWED_HOSTS`.
    #[must_use]
    pub fn allowed_hosts(mut self, allowed_hosts: Vec<String>) -> Self {
        self.allowed_hosts = Some(allowed_hosts);
        self
    }

    /// Set the deployment-wide MCP tool surface.
    #[must_use]
    pub fn tool_scope(mut self, tool_scope: ToolScope) -> Self {
        self.tool_scope = Some(tool_scope);
        self
    }

    /// Set the max authenticated stream lifetime.
    ///
    /// The environment equivalent is `PROXIMA_STREAM_MAX_LIFETIME`,
    /// parsed as integer seconds.
    #[must_use]
    pub fn stream_max_lifetime(mut self, duration: Duration) -> Self {
        self.stream_max_lifetime = Some(duration);
        self
    }

    /// Set the host-auth epoch polling interval for authenticated streams.
    ///
    /// The environment equivalent is `PROXIMA_STREAM_EPOCH_INTERVAL`,
    /// parsed as integer seconds.
    #[must_use]
    pub fn epoch_check_interval(mut self, duration: Duration) -> Self {
        self.epoch_check_interval = Some(duration);
        self
    }

    /// Opt into loopback-only single-owner mode (no authenticator).
    /// Never combined with network exposure; programmatic only.
    #[must_use]
    pub fn allow_insecure_single_owner(mut self) -> Self {
        self.insecure_single_owner = true;
        self
    }

    /// Serve the `/v1` REST rendering of the tool manifest beside `/mcp`
    /// (see docs/17). Off by default, and only effective when the crate is
    /// built with the `rest` feature. Env equivalent:
    /// `PROXIMA_REST_ENABLED`.
    #[must_use]
    pub fn rest_enabled(mut self, rest_enabled: bool) -> Self {
        self.rest_enabled = Some(rest_enabled);
        self
    }

    /// Boot without applying migrations (preflight only).
    ///
    /// For split-role `GitOps` deploys: migrate out-of-band under a DDL role,
    /// then boot the app under a DML-only role. Env equivalent:
    /// `PROXIMA_SKIP_MIGRATIONS`.
    #[must_use]
    pub fn skip_migrations(mut self) -> Self {
        self.skip_migrations = Some(true);
        self
    }

    /// Set the Postgres query tuning, bypassing the `PROXIMA_PG_*` block a
    /// deployment would otherwise be read from. Its defaults are this
    /// release's shipped behaviour, so a host that never calls this and an
    /// environment that sets nothing are the same deployment.
    #[must_use]
    pub fn pg_tuning(mut self, pg_tuning: PgTuning) -> Self {
        self.pg_tuning = Some(pg_tuning);
        self
    }

    /// Install the host authenticator used to resolve MCP credentials.
    #[must_use]
    pub fn authenticator(mut self, authenticator: Arc<dyn Authenticator>) -> Self {
        self.authenticator = Some(authenticator);
        self
    }

    /// Advertise OAuth protected-resource metadata (enables the public
    /// discovery route + `WWW-Authenticate` on 401).
    #[must_use]
    pub fn resource_metadata(mut self, metadata: ResourceServerMetadata) -> Self {
        self.resource_metadata = Some(metadata);
        self
    }

    /// Install the embedding client (`Engine::with_embed`).
    #[must_use]
    pub fn embed_client(mut self, client: Arc<dyn EmbeddingClient>) -> Self {
        self.embed_client = Some(client);
        self
    }

    /// Install the Anthropic model client (`Engine::with_anthropic`).
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
        self.apply_lookup(proxima_core::process_env)
    }

    /// Apply injected environment values to unset fields.
    ///
    /// A variable set to the empty string, or to nothing but whitespace, is
    /// treated as unset — the same rule the binary's own argument parsing and
    /// the `PROXIMA_S3_*` block already used, and which this function used to
    /// bypass for all nine of the variables below. Values are trimmed, so a
    /// trailing newline picked up from a here-doc or a mounted secret no
    /// longer turns into a parse error naming a value nobody typed.
    ///
    /// Consequence worth knowing when layering configs: because empty now
    /// means unset, `PROXIMA_ALLOWED_ORIGINS=` no longer overrides a value set
    /// programmatically on a base builder. It leaves the field untouched
    /// instead, which is what "apply to unset fields" says.
    ///
    /// # Errors
    ///
    /// Returns `ProximaError::Config` when a supplied value is malformed.
    pub fn apply_lookup(
        mut self,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, ProximaError> {
        let lookup = |key: &str| proxima_core::env_value(&lookup, key);
        if self.database_url.is_none() {
            self.database_url = lookup("DATABASE_URL");
        }
        if self.s3.is_none() {
            self.s3 = s3_from_lookup(&lookup)?;
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
        if self.rest_enabled.is_none() {
            self.rest_enabled = lookup("PROXIMA_REST_ENABLED")
                .map(|raw| parse_bool_value("PROXIMA_REST_ENABLED", &raw))
                .transpose()?;
        }
        if self.skip_migrations.is_none() {
            self.skip_migrations = lookup("PROXIMA_SKIP_MIGRATIONS")
                .map(|raw| parse_bool_value("PROXIMA_SKIP_MIGRATIONS", &raw))
                .transpose()?;
        }
        if self.pg_tuning.is_none() {
            self.pg_tuning = pg_tuning_from_lookup(&lookup)?;
        }
        if self.allowed_origins.is_none() {
            self.allowed_origins =
                lookup("PROXIMA_ALLOWED_ORIGINS").map(|raw| parse_allowed_origins(&raw));
        }
        if self.allowed_hosts.is_none() {
            self.allowed_hosts =
                lookup("PROXIMA_ALLOWED_HOSTS").map(|raw| parse_allowed_hosts(&raw));
        }
        if self.stream_max_lifetime.is_none() {
            self.stream_max_lifetime = lookup("PROXIMA_STREAM_MAX_LIFETIME")
                .map(|raw| parse_duration_seconds("PROXIMA_STREAM_MAX_LIFETIME", &raw))
                .transpose()?;
        }
        if self.epoch_check_interval.is_none() {
            self.epoch_check_interval = lookup("PROXIMA_STREAM_EPOCH_INTERVAL")
                .map(|raw| parse_duration_seconds("PROXIMA_STREAM_EPOCH_INTERVAL", &raw))
                .transpose()?;
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
        let mcp = if self.mcp_enabled {
            Some(McpSettings {
                bind: self.mcp_bind.unwrap_or_else(default_mcp_bind),
            })
        } else {
            None
        };
        let default_revalidation = RevalidationConfig::default();
        let stream_revalidation = RevalidationConfig {
            max_stream_lifetime: self
                .stream_max_lifetime
                .unwrap_or(default_revalidation.max_stream_lifetime),
            epoch_check_interval: self
                .epoch_check_interval
                .unwrap_or(default_revalidation.epoch_check_interval),
        };
        validate_revalidation_config(stream_revalidation)?;
        let tool_scope = self.tool_scope.ok_or_else(|| {
            ProximaError::Config(
                "tool_scope is required: pass ToolScope::All to expose the full tool surface \
                 (previous implicit default) or ToolScope::Palette([...]) for a restricted \
                 keep-set; agent-facing hosts should prefer a narrow palette"
                    .into(),
            )
        })?;
        let owner = self.owner;
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
            allowed_hosts: self.allowed_hosts.unwrap_or_default(),
            tool_scope,
            stream_revalidation,
            insecure_single_owner: self.insecure_single_owner,
            rest_enabled: self.rest_enabled.unwrap_or(false),
            skip_migrations: self.skip_migrations.unwrap_or(false),
            pg_tuning: self.pg_tuning.unwrap_or_default(),
            auth: RuntimeAuthState {
                has_host_authenticator: parts.authenticator.is_some(),
            },
            resource_metadata: self.resource_metadata,
        };
        config.validate()?;
        Ok((config, parts))
    }
}

/// Pure, validated runtime config.
///
/// The booleans are independent deployment gates — expose the network,
/// serve `/v1`, skip migrations, allow insecure single-owner — not the
/// states of one machine. Folding them into enums would invent
/// relationships between them that do not exist, and each already carries
/// its own env var and its own validation rule.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone)]
pub struct RuntimeConfig {
    pub database_url: String,
    pub s3: Option<S3RuntimeConfig>,
    pub owner: Option<Owner>,
    pub mcp: Option<McpSettings>,
    pub expose_network: bool,
    pub allowed_origins: Vec<String>,
    /// Explicit inbound `Host` allowlist (`PROXIMA_ALLOWED_HOSTS`). Empty
    /// ⇒ derive from `resource_metadata.public_url` + `allowed_origins`.
    /// Bare hostnames or `host:port`; the listener always adds loopback and
    /// shares the resulting non-empty allowlist with rmcp's inner guard.
    pub allowed_hosts: Vec<String>,
    pub tool_scope: ToolScope,
    pub stream_revalidation: RevalidationConfig,
    pub insecure_single_owner: bool,
    /// Serve `/v1` beside `/mcp` on the same listener, inside the same Host,
    /// auth, and body-limit layers (docs/17). Two gates, both required: the
    /// `rest` cargo feature compiles the module, this flag serves it. A second
    /// transport projection is opt-in twice on purpose — compiling it is a
    /// build decision, exposing it is a deployment one.
    pub rest_enabled: bool,
    /// Boot without applying migrations (preflight only) — schema is migrated
    /// out-of-band under a DDL role in split-role `GitOps` deploys.
    pub skip_migrations: bool,
    /// Postgres query tuning (`PROXIMA_PG_*`). Defaults are this release's
    /// shipped behaviour, so an unset environment is production;
    /// `PROXIMA_PG_SEMANTIC_INDEX_FIRST=off` restores the legacy semantic
    /// result membership.
    pub pg_tuning: PgTuning,
    pub auth: RuntimeAuthState,
    pub resource_metadata: Option<ResourceServerMetadata>,
}

#[derive(Debug, Clone, Copy)]
pub struct RuntimeAuthState {
    pub has_host_authenticator: bool,
}

impl std::fmt::Debug for RuntimeConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeConfig")
            .field("database_url", &"<redacted>")
            .field("s3", &self.s3)
            .field("owner", &self.owner)
            .field("mcp", &self.mcp)
            .field("expose_network", &self.expose_network)
            .field("allowed_origins", &self.allowed_origins)
            .field("allowed_hosts", &self.allowed_hosts)
            .field("tool_scope", &self.tool_scope)
            .field("stream_revalidation", &self.stream_revalidation)
            .field("insecure_single_owner", &self.insecure_single_owner)
            .field("rest_enabled", &self.rest_enabled)
            .field("skip_migrations", &self.skip_migrations)
            .field("pg_tuning", &self.pg_tuning)
            .field("auth", &self.auth)
            .field("resource_metadata", &self.resource_metadata)
            .finish()
    }
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

        if self.insecure_single_owner {
            return Err(ProximaError::Security(
                "insecure single-owner mode cannot serve MCP; configure host auth".into(),
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
        if self.allowed_hosts.iter().any(|host| host.contains('*')) {
            // The shared Host guard has no wildcard semantics — a `*` entry
            // matches nothing and fails closed, locking the operator out silently.
            // Reject it loudly instead of letting it look like "allow all".
            return Err(ProximaError::Security(
                "MCP Host allowlist forbids wildcard allowed hosts; list each host explicitly \
                 (PROXIMA_ALLOWED_HOSTS), or rely on PROXIMA_PUBLIC_URL"
                    .into(),
            ));
        }
        if self.expose_network && self.public_allowed_hosts().is_empty() {
            return Err(ProximaError::Security(
                "network exposure requires a resolvable public host: set PROXIMA_ALLOWED_HOSTS, \
                 PROXIMA_PUBLIC_URL, or a non-loopback host in PROXIMA_ALLOWED_ORIGINS"
                    .into(),
            ));
        }
        if self.expose_network && !self.auth.has_host_authenticator {
            return Err(ProximaError::Security(
                "network exposure requires a host authenticator".into(),
            ));
        }
        if !self.expose_network && !mcp.bind.ip().is_loopback() {
            return Err(ProximaError::Security(
                "non-exposed MCP bind must be loopback".into(),
            ));
        }
        if !self.auth.has_host_authenticator {
            return Err(ProximaError::Security(
                "MCP requires a host authenticator".into(),
            ));
        }

        Ok(())
    }

    /// Non-loopback public hosts for the inbound `Host` allowlist.
    ///
    /// Explicit `allowed_hosts` (`PROXIMA_ALLOWED_HOSTS`) win verbatim;
    /// otherwise hosts are derived from `resource_metadata.public_url`
    /// (i.e. `PROXIMA_PUBLIC_URL`) and `allowed_origins`, with loopback
    /// dropped — loopback is added unconditionally by `HostAllowlist`, so
    /// it never counts as a resolvable *public* host here. Empty ⇒ a
    /// network-exposed deployment would 403 every real request.
    #[must_use]
    pub fn public_allowed_hosts(&self) -> Vec<String> {
        if !self.allowed_hosts.is_empty() {
            return dedup_hosts(
                self.allowed_hosts
                    .iter()
                    .map(|host| host.trim().to_ascii_lowercase()),
            );
        }
        let derived = self
            .resource_metadata
            .as_ref()
            .and_then(|md| host_of_url(&md.public_url))
            .into_iter()
            .chain(self.allowed_origins.iter().filter_map(|o| host_of_url(o)))
            .filter(|host| !is_loopback_host(host));
        dedup_hosts(derived)
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
    #[error("registry: {0}")]
    Registry(#[from] proxima_core::FlavorRegistryError),
    #[error("flavor services: {0}")]
    FlavorServices(#[from] FlavorServiceError),
    #[error("storage: {0}")]
    Storage(String),
    #[error("engine: {0}")]
    Engine(String),
    #[error("security: {0}")]
    Security(String),
    #[error("mcp: {0}")]
    Mcp(String),
    /// The target database still carries pre-v0.0.4 Proxima schema
    /// artifacts and must be exported and reset before this host can boot.
    /// See `MIGRATING.md`. Kept distinct from [`Self::Storage`] so hosts
    /// can match on it instead of parsing the storage error string.
    #[error("database requires a v0.0.4 reset before boot (see MIGRATING.md): {details}")]
    V004ResetRequired { details: String },
}

impl From<EmbedError> for ProximaError {
    fn from(value: EmbedError) -> Self {
        match value {
            EmbedError::Config(err) => Self::Config(err),
            EmbedError::Registry(err) => Self::Registry(err),
            EmbedError::Storage(err) => Self::Storage(err),
            EmbedError::Engine(err) => Self::Engine(err),
            EmbedError::V004ResetRequired { details } => Self::V004ResetRequired { details },
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

fn parse_allowed_hosts(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|host| !host.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

/// Extract the bare, lowercased host from a URL or origin — scheme,
/// userinfo, port, and path stripped; IPv6 brackets removed. Returns
/// `None` for input with no host part.
fn host_of_url(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let after_scheme = raw.split_once("://").map_or(raw, |(_, rest)| rest);
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(after_scheme);
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let host = if let Some(rest) = authority.strip_prefix('[') {
        rest.split_once(']').map(|(host, _)| host)?
    } else {
        authority.split(':').next().unwrap_or(authority)
    };
    let host = host.trim().to_ascii_lowercase();
    (!host.is_empty()).then_some(host)
}

fn dedup_hosts(hosts: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    hosts
        .into_iter()
        .filter(|host| !host.is_empty())
        .filter(|host| seen.insert(host.clone()))
        .collect()
}

fn parse_duration_seconds(key: &str, raw: &str) -> Result<Duration, ProximaError> {
    let trimmed = raw.trim();
    let seconds = trimmed
        .parse::<u64>()
        .map_err(|_| ProximaError::Config(format!("{key} must be integer seconds, got {raw:?}")))?;
    Ok(Duration::from_secs(seconds))
}

fn validate_revalidation_config(config: RevalidationConfig) -> Result<(), ProximaError> {
    if config.max_stream_lifetime.is_zero() {
        return Err(ProximaError::Config(
            "stream max lifetime must be greater than 0 seconds".into(),
        ));
    }
    if config.epoch_check_interval.is_zero() {
        return Err(ProximaError::Config(
            "stream epoch check interval must be greater than 0 seconds".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use async_trait::async_trait;
    use proxima_core::{AuthError, AuthzContext, Credentials, GroupId, OwnerRef};
    use proxima_storage_pg::SemanticIndexFirst;

    use super::*;
    use crate::company_owner;

    fn lookup<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| {
            pairs
                .iter()
                .find(|(candidate, _)| *candidate == key)
                .map(|(_, value)| (*value).to_string())
        }
    }

    fn owner(id: uuid::Uuid) -> Owner {
        OwnerRef::Group(GroupId::new(id))
    }

    #[derive(Debug)]
    struct TestAuthenticator;

    #[async_trait]
    impl Authenticator for TestAuthenticator {
        async fn authenticate(
            &self,
            _credentials: &Credentials,
        ) -> Result<AuthzContext, AuthError> {
            Err(AuthError::InvalidCredentials)
        }
    }

    fn base_config(mcp: Option<SocketAddr>) -> RuntimeConfig {
        RuntimeConfig {
            database_url: "postgres://localhost/proxima".to_string(),
            s3: None,
            owner: Some(company_owner(uuid::Uuid::now_v7())),
            mcp: mcp.map(|bind| McpSettings { bind }),
            expose_network: false,
            allowed_origins: Vec::new(),
            allowed_hosts: Vec::new(),
            tool_scope: ToolScope::All,
            stream_revalidation: RevalidationConfig::default(),
            insecure_single_owner: false,
            rest_enabled: false,
            skip_migrations: false,
            pg_tuning: PgTuning::default(),
            auth: RuntimeAuthState {
                has_host_authenticator: true,
            },
            resource_metadata: None,
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
    fn runtime_builder_debug_redacts_database_url() {
        let builder = RuntimeBuilder::default().database_url("postgres://user:secret@host/db");
        let debug = format!("{builder:?}");

        assert!(debug.contains("has_database_url"));
        assert!(!debug.contains("secret"));
        assert!(!debug.contains("postgres://user"));
    }

    #[test]
    fn runtime_config_debug_redacts_database_url() {
        let config = base_config(None);
        let debug = format!("{config:?}");

        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("postgres://localhost"));
    }

    #[test]
    fn validate_mcp_requires_authenticator() {
        let mut config = base_config(Some(addr([127, 0, 0, 1])));
        config.auth.has_host_authenticator = false;

        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("host authenticator"));
    }

    #[test]
    fn validate_insecure_single_owner_cannot_serve_mcp() {
        let mut config = base_config(Some(addr([127, 0, 0, 1])));
        config.insecure_single_owner = true;
        config.expose_network = true;
        config.allowed_origins = vec!["https://app.test".to_string()];

        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("cannot serve MCP"));
    }

    #[test]
    fn validate_insecure_single_owner_rejected_even_on_loopback() {
        let mut config = base_config(Some(addr([0, 0, 0, 0])));
        config.insecure_single_owner = true;
        config.auth.has_host_authenticator = false;

        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("cannot serve MCP"));
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
        config.auth.has_host_authenticator = false;

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
        config.auth.has_host_authenticator = false;
        config.expose_network = true;

        config.validate().unwrap();
    }

    #[test]
    fn validate_mcp_rejects_wildcard_allowed_host_on_loopback() {
        // `*` has no rmcp wildcard meaning; it must be rejected loudly,
        // not silently lock out a loopback-bound reverse proxy.
        let mut config = base_config(Some(addr([127, 0, 0, 1])));
        config.allowed_hosts = vec!["*".to_string()];

        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("wildcard allowed hosts"));
    }

    #[test]
    fn validate_exposed_network_requires_resolvable_public_host() {
        // Exposed, origins present (so the empty-origins rule passes) but
        // loopback-only and no public_url ⇒ no resolvable public host, so
        // every real request would 403. Must fail closed at startup.
        let mut config = base_config(Some(addr([127, 0, 0, 1])));
        config.expose_network = true;
        config.allowed_origins = vec!["http://localhost:8080".to_string()];

        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("resolvable public host"));
    }

    #[test]
    fn validate_exposed_network_accepts_public_url_host() {
        let mut config = base_config(Some(addr([127, 0, 0, 1])));
        config.expose_network = true;
        config.allowed_origins = vec!["http://localhost:8080".to_string()];
        config.resource_metadata = Some(ResourceServerMetadata {
            public_url: "https://proxima.aqs-dev.cloud".to_string(),
            authorization_servers: vec!["https://idp.test".to_string()],
        });

        config.validate().unwrap();
    }

    #[test]
    fn host_of_url_extracts_bare_lowercased_host() {
        assert_eq!(
            host_of_url("https://proxima.aqs-dev.cloud").as_deref(),
            Some("proxima.aqs-dev.cloud")
        );
        assert_eq!(
            host_of_url("https://Example.COM:8443/mcp").as_deref(),
            Some("example.com")
        );
        assert_eq!(host_of_url("http://[::1]:8080").as_deref(), Some("::1"));
        assert_eq!(
            host_of_url("https://user@host.test:9000/p?q=1").as_deref(),
            Some("host.test")
        );
        assert_eq!(
            host_of_url("tauri://localhost").as_deref(),
            Some("localhost")
        );
        assert_eq!(host_of_url("   "), None);
    }

    #[test]
    fn is_loopback_host_detects_loopback_forms() {
        assert!(is_loopback_host("localhost"));
        assert!(is_loopback_host("127.0.0.1"));
        assert!(is_loopback_host("::1"));
        assert!(!is_loopback_host("proxima.aqs-dev.cloud"));
        assert!(!is_loopback_host("10.0.0.5"));
    }

    #[test]
    fn public_allowed_hosts_derives_from_public_url_and_origins() {
        let mut config = base_config(Some(addr([127, 0, 0, 1])));
        config.resource_metadata = Some(ResourceServerMetadata {
            public_url: "https://proxima.aqs-dev.cloud".to_string(),
            authorization_servers: vec![],
        });
        config.allowed_origins = vec![
            "https://app.test".to_string(),
            "http://localhost:5173".to_string(),
        ];

        // public_url host first, then non-loopback origin hosts; loopback dropped.
        assert_eq!(
            config.public_allowed_hosts(),
            vec!["proxima.aqs-dev.cloud".to_string(), "app.test".to_string()]
        );
    }

    #[test]
    fn public_allowed_hosts_explicit_overrides_derivation() {
        let mut config = base_config(Some(addr([127, 0, 0, 1])));
        config.resource_metadata = Some(ResourceServerMetadata {
            public_url: "https://derived.test".to_string(),
            authorization_servers: vec![],
        });
        config.allowed_origins = vec!["https://app.test".to_string()];
        config.allowed_hosts = vec!["Proxima.Internal:8443".to_string(), "10.0.0.5".to_string()];

        assert_eq!(
            config.public_allowed_hosts(),
            vec!["proxima.internal:8443".to_string(), "10.0.0.5".to_string()]
        );
    }

    #[test]
    fn allowed_hosts_env_is_split_trimmed_and_lowercased() {
        let builder = RuntimeBuilder::default()
            .apply_lookup(lookup(&[(
                "PROXIMA_ALLOWED_HOSTS",
                " Proxima.Test, ,host:8443 , ",
            )]))
            .unwrap();

        assert_eq!(
            builder.allowed_hosts.unwrap(),
            ["proxima.test".to_string(), "host:8443".to_string()]
        );
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
    fn stream_revalidation_env_parses_integer_seconds() {
        let (config, _) = RuntimeBuilder::default()
            .database_url("postgres://localhost/proxima")
            .owner(owner(uuid::Uuid::now_v7()))
            .tool_scope(ToolScope::All)
            .apply_lookup(lookup(&[
                ("PROXIMA_STREAM_MAX_LIFETIME", "7"),
                ("PROXIMA_STREAM_EPOCH_INTERVAL", "2"),
            ]))
            .unwrap()
            .resolve()
            .unwrap();

        assert_eq!(
            config.stream_revalidation.max_stream_lifetime,
            Duration::from_secs(7)
        );
        assert_eq!(
            config.stream_revalidation.epoch_check_interval,
            Duration::from_secs(2)
        );
    }

    #[test]
    fn stream_revalidation_zero_duration_rejected_at_resolve() {
        let err = RuntimeBuilder::default()
            .database_url("postgres://localhost/proxima")
            .owner(owner(uuid::Uuid::now_v7()))
            .stream_max_lifetime(Duration::ZERO)
            .resolve()
            .unwrap_err();

        assert!(err.to_string().contains("greater than 0"));
    }

    #[test]
    fn stream_revalidation_defaults_when_unset() {
        let (config, _) = RuntimeBuilder::default()
            .database_url("postgres://localhost/proxima")
            .owner(owner(uuid::Uuid::now_v7()))
            .tool_scope(ToolScope::All)
            .resolve()
            .unwrap();

        assert_eq!(config.stream_revalidation, RevalidationConfig::default());
    }

    #[test]
    fn malformed_mcp_bind_errors() {
        let err = RuntimeBuilder::default()
            .apply_lookup(lookup(&[("PROXIMA_MCP_BIND", "not-a-socket")]))
            .unwrap_err();

        assert!(err.to_string().contains("PROXIMA_MCP_BIND"));
    }

    #[test]
    fn pg_tuning_reads_the_env_block() {
        let (config, _) = RuntimeBuilder::default()
            .database_url("postgres://localhost/proxima")
            .owner(owner(uuid::Uuid::now_v7()))
            .tool_scope(ToolScope::All)
            .apply_lookup(lookup(&[
                ("PROXIMA_PG_SEMANTIC_INDEX_FIRST", "pushdown"),
                ("PROXIMA_PG_HNSW_EF_SEARCH", "200"),
            ]))
            .unwrap()
            .resolve()
            .unwrap();

        assert_eq!(
            config.pg_tuning,
            PgTuning {
                semantic_index_first: SemanticIndexFirst::Pushdown,
                hnsw_ef_search: 200,
                ..PgTuning::default()
            }
        );
    }

    /// An untuned environment is silent, not an answer, so it leaves a
    /// programmatically tuned base alone — the same `merge_over` rule the
    /// allowlists follow.
    #[test]
    fn an_untuned_env_does_not_override_configured_tuning() {
        let tuned = PgTuning {
            hnsw_ef_search: 200,
            ..PgTuning::default()
        };
        let from_env = RuntimeBuilder::default()
            .apply_lookup(lookup(&[("PROXIMA_REST_ENABLED", "true")]))
            .expect("an untuned environment is not a malformed one");

        let merged = from_env.merge_over(RuntimeBuilder::default().pg_tuning(tuned));

        assert_eq!(merged.pg_tuning, Some(tuned));
    }

    #[test]
    fn malformed_pg_tuning_errors() {
        let err = RuntimeBuilder::default()
            .apply_lookup(lookup(&[(
                "PROXIMA_PG_SEMANTIC_INDEX_FIRST",
                "index-first",
            )]))
            .unwrap_err();

        assert!(
            err.to_string()
                .contains("PROXIMA_PG_SEMANTIC_INDEX_FIRST=index-first")
        );
    }

    /// An operator who exports a variable without a value has not configured
    /// it. This used to abort boot with `must be a boolean, got ""` for the
    /// three flags, and hand the pool an empty connection string for
    /// `DATABASE_URL`, while `PROXIMA_EMBED_*` in the same process read the
    /// same input as unset.
    #[test]
    fn an_empty_value_is_an_unset_value() {
        let builder = RuntimeBuilder::default()
            .apply_lookup(lookup(&[
                ("DATABASE_URL", ""),
                ("PROXIMA_EXPOSE_NETWORK", ""),
                ("PROXIMA_REST_ENABLED", "   "),
                ("PROXIMA_SKIP_MIGRATIONS", ""),
                ("PROXIMA_MCP_BIND", ""),
                ("PROXIMA_STREAM_MAX_LIFETIME", ""),
            ]))
            .expect("an empty value must not be parsed as a malformed one");

        assert!(builder.database_url.is_none());
        assert!(builder.expose_network.is_none());
        assert!(builder.rest_enabled.is_none());
        assert!(builder.skip_migrations.is_none());
        assert!(builder.mcp_bind.is_none());
        assert!(builder.stream_max_lifetime.is_none());
    }

    /// Trailing newlines survive here-docs and mounted secrets. Trimming is
    /// what lets the same value work here and in `PROXIMA_S3_*`, which has
    /// trimmed since it was written.
    #[test]
    fn surrounding_whitespace_is_trimmed_before_parsing() {
        let builder = RuntimeBuilder::default()
            .apply_lookup(lookup(&[
                ("DATABASE_URL", " postgres://localhost/proxima\n"),
                ("PROXIMA_EXPOSE_NETWORK", " true\n"),
                ("PROXIMA_MCP_BIND", " 127.0.0.1:31415\n"),
            ]))
            .expect("a trailing newline must not be a parse error");

        assert_eq!(
            builder.database_url.as_deref(),
            Some("postgres://localhost/proxima")
        );
        assert_eq!(builder.expose_network, Some(true));
        assert_eq!(
            builder.mcp_bind.map(|bind| bind.to_string()).as_deref(),
            Some("127.0.0.1:31415")
        );
    }

    /// Empty means unset, so it no longer beats a programmatically set base.
    /// `merge_over` layers on `Option`, and an empty variable used to arrive
    /// as `Some(vec![])` and win.
    #[test]
    fn an_empty_allowlist_does_not_override_a_configured_one() {
        let from_env = RuntimeBuilder::default()
            .apply_lookup(lookup(&[("PROXIMA_ALLOWED_ORIGINS", "")]))
            .expect("empty allowlist is unset");
        let base = RuntimeBuilder::default().allowed_origins(vec!["https://app.test".to_string()]);

        let merged = from_env.merge_over(base);

        assert_eq!(
            merged.allowed_origins.as_deref(),
            Some(["https://app.test".to_string()].as_slice())
        );
    }

    #[test]
    fn resolve_without_tool_scope_fails_closed_with_actionable_message() {
        // No embedding host may silently advertise the full tool surface
        // (core_publish/core_membership included) by omitting `.tool_scope(...)`.
        let err = RuntimeBuilder::default()
            .database_url("postgres://localhost/proxima")
            .owner(owner(uuid::Uuid::now_v7()))
            .resolve()
            .unwrap_err();

        let message = err.to_string();
        assert!(message.contains("tool_scope is required"));
        assert!(message.contains("ToolScope::All"));
        assert!(message.contains("ToolScope::Palette"));
    }

    #[test]
    fn resolve_with_explicit_tool_scope_all_builds_as_before() {
        let (config, _) = RuntimeBuilder::default()
            .database_url("postgres://localhost/proxima")
            .owner(owner(uuid::Uuid::now_v7()))
            .tool_scope(ToolScope::All)
            .resolve()
            .unwrap();

        assert_eq!(config.tool_scope, ToolScope::All);
    }

    #[test]
    fn runtime_config_accepts_custom_authenticator_without_separate_owner_access() {
        let (config, _) = RuntimeBuilder::default()
            .database_url("postgres://localhost/proxima")
            .owner(owner(uuid::Uuid::now_v7()))
            .tool_scope(ToolScope::All)
            .with_mcp()
            .authenticator(Arc::new(TestAuthenticator))
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
            .stream_max_lifetime(Duration::from_secs(12))
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
        assert_eq!(merged.stream_max_lifetime, Some(Duration::from_secs(12)));
        assert!(merged.insecure_single_owner);
    }
}

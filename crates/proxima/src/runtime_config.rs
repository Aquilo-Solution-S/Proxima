use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use proxima_blob_s3::S3RuntimeConfig;
use proxima_core::publication::PublicationConfig;
use proxima_core::{
    Authenticator, EmbeddingClient, EmbeddingRouter, EmbeddingRuntimePolicy, FlavorServiceError,
    FlavorServices, Owner, OwnerAccessPort, RequestHeaders, RevalidationConfig, ToolScope,
    is_loopback_host,
};
use proxima_mcp_server::{McpTransportConfig, RequestHeaderAllowlist, ResourceServerMetadata};
use proxima_storage_pg::{PgHostStateParticipant, PgPoolConfig, PgTuning};

use crate::EmbedError;
use crate::config::{
    parse_bool_value, pg_pool_config_from_lookup, pg_tuning_from_lookup,
    publication_config_from_lookup, s3_from_lookup,
};
use crate::owner_access::{ForwarderPolicy, LateOwnerAccess, forwarder_from_lookup};

const DEFAULT_MCP_BIND: &str = "127.0.0.1:31415";

pub(crate) const DUPLICATE_HOST_STATE_PARTICIPANT: &str = "a host-state participant is already registered and a unit of work dispatches to exactly one; register one participant that serves every command type (docs/19)";

/// Runtime configuration builder for host applications.
#[derive(Default)]
pub struct RuntimeBuilder {
    database_url: Option<String>,
    platform_database_url: Option<String>,
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
    pg_pool_config: Option<PgPoolConfig>,
    pg_tuning: Option<PgTuning>,
    authenticator: Option<Arc<dyn Authenticator>>,
    resource_metadata: Option<ResourceServerMetadata>,
    embed_client: Option<Arc<dyn EmbeddingClient>>,
    embedding_router: Option<Arc<dyn EmbeddingRouter>>,
    embedding_runtime_policy: Option<EmbeddingRuntimePolicy>,
    publication: Option<PublicationConfig>,
    /// Horizon after which a DELIVERED outbox record is reclaimed. `None`
    /// keeps published records forever, which is both the default and what
    /// `PROXIMA_OUTBOX_PUBLISHED_RETENTION_SECS=0` spells.
    published_retention: Option<Duration>,
    #[cfg(feature = "outbox-nats")]
    nats: Option<proxima_outbox_nats::NatsPublisherConfig>,
    host_state_participant: Option<Arc<dyn PgHostStateParticipant>>,
    /// A second participant was registered; [`Self::resolve`] refuses.
    duplicate_host_state_participant: bool,
    runtime_grants: Option<bool>,
    request_headers: Option<Vec<String>>,
    owner_access: Option<Arc<dyn OwnerAccessPort>>,
    forwarder: Option<ForwarderPolicy>,
    /// Every host bag handed to [`Self::services`], across layers; merged
    /// with [`FlavorServices::try_extend`] at resolve so a type two layers
    /// both publish is a boot error, not a silent winner.
    services: Vec<FlavorServices>,
    health_endpoints: Option<bool>,
    max_request_body_bytes: Option<usize>,
    mcp_transport: Option<McpTransportConfig>,
    /// The `PROXIMA_OIDC_*` values the environment layer saw, kept so the
    /// authenticator is built at resolve only when no layer set one.
    oidc_env: Option<std::collections::BTreeMap<&'static str, String>>,
}

impl std::fmt::Debug for RuntimeBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeBuilder")
            .field("has_database_url", &self.database_url.is_some())
            .field(
                "has_platform_database_url",
                &self.platform_database_url.is_some(),
            )
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
            .field("pg_pool_config", &self.pg_pool_config)
            .field("pg_tuning", &self.pg_tuning)
            .field("has_authenticator", &self.authenticator.is_some())
            .field("has_resource_metadata", &self.resource_metadata.is_some())
            .field("has_embed_client", &self.embed_client.is_some())
            .field("has_embedding_router", &self.embedding_router.is_some())
            .field("embedding_runtime_policy", &self.embedding_runtime_policy)
            .field("publication", &self.publication)
            .field("published_retention", &self.published_retention)
            .field(
                "has_host_state_participant",
                &self.host_state_participant.is_some(),
            )
            .field(
                "duplicate_host_state_participant",
                &self.duplicate_host_state_participant,
            )
            .field("runtime_grants", &self.runtime_grants)
            .field("request_headers", &self.request_headers)
            .field("has_owner_access", &self.owner_access.is_some())
            .field("forwarder", &self.forwarder)
            .field("services", &self.services)
            .field("health_endpoints", &self.health_endpoints)
            .field("max_request_body_bytes", &self.max_request_body_bytes)
            .field("mcp_transport", &self.mcp_transport)
            .field("has_oidc_env", &self.oidc_env.is_some())
            .finish_non_exhaustive()
    }
}

impl RuntimeBuilder {
    #[must_use]
    pub(crate) fn merge_over(self, base: Self) -> Self {
        Self {
            database_url: self.database_url.or(base.database_url),
            platform_database_url: self.platform_database_url.or(base.platform_database_url),
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
            pg_pool_config: self.pg_pool_config.or(base.pg_pool_config),
            pg_tuning: self.pg_tuning.or(base.pg_tuning),
            authenticator: self.authenticator.or(base.authenticator),
            resource_metadata: self.resource_metadata.or(base.resource_metadata),
            embed_client: self.embed_client.or(base.embed_client),
            embedding_router: self.embedding_router.or(base.embedding_router),
            embedding_runtime_policy: self
                .embedding_runtime_policy
                .or(base.embedding_runtime_policy),
            publication: self.publication.or(base.publication),
            published_retention: self.published_retention.or(base.published_retention),
            #[cfg(feature = "outbox-nats")]
            nats: self.nats.or(base.nats),
            duplicate_host_state_participant: self.duplicate_host_state_participant
                || base.duplicate_host_state_participant
                || (self.host_state_participant.is_some() && base.host_state_participant.is_some()),
            host_state_participant: self.host_state_participant.or(base.host_state_participant),
            runtime_grants: self.runtime_grants.or(base.runtime_grants),
            request_headers: self.request_headers.or(base.request_headers),
            owner_access: self.owner_access.or(base.owner_access),
            forwarder: self.forwarder.or(base.forwarder),
            services: base.services.into_iter().chain(self.services).collect(),
            health_endpoints: self.health_endpoints.or(base.health_endpoints),
            max_request_body_bytes: self.max_request_body_bytes.or(base.max_request_body_bytes),
            mcp_transport: self.mcp_transport.or(base.mcp_transport),
            oidc_env: self.oidc_env.or(base.oidc_env),
        }
    }

    /// Set the Postgres connection string. Env equivalent: `DATABASE_URL`.
    #[must_use]
    pub fn database_url(mut self, database_url: impl Into<String>) -> Self {
        self.database_url = Some(database_url.into());
        self
    }

    /// Set the separate migration/platform connection string. Env equivalent:
    /// `PROXIMA_PLATFORM_DATABASE_URL`.
    #[must_use]
    pub fn platform_database_url(mut self, url: impl Into<String>) -> Self {
        self.platform_database_url = Some(url.into());
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

    /// Bind the deployment's publication source and capture bounds.
    /// Env equivalent: `PROXIMA_PUBLICATION_SOURCE`,
    /// `PROXIMA_OUTBOX_MAX_PENDING`, `PROXIMA_OUTBOX_MAX_PAYLOAD_BYTES`.
    ///
    /// Retention of DELIVERED records is deliberately NOT here. It is host
    /// housekeeping, not engine configuration — the engine never prunes —
    /// so it lives on [`Self::published_retention`] and is read from
    /// `PROXIMA_OUTBOX_PUBLISHED_RETENTION_SECS` outside this block's guard.
    ///
    /// Setting this suppresses the environment read for the whole block —
    /// one parser, as with the S3 and Postgres blocks.
    #[must_use]
    pub fn publication(mut self, publication: PublicationConfig) -> Self {
        self.publication = Some(publication);
        self
    }

    /// How long an ALREADY-PUBLISHED outbox record is kept before the host
    /// reclaims its storage. Env equivalent:
    /// `PROXIMA_OUTBOX_PUBLISHED_RETENTION_SECS`.
    ///
    /// `None` — the default — keeps delivered records forever. The horizon
    /// can never reach a `pending` or `claimed` record whatever its age: an
    /// undelivered event is a promise this deployment has not kept, and no
    /// retention policy may quietly cancel it (doc 18, kernel OB-9).
    ///
    /// Setting this suppresses the environment read, so a host that composes
    /// programmatically is not overridden by a stray variable. A value under
    /// the one-minute floor is refused at `build`, exactly as the
    /// environment path refuses it at boot.
    #[must_use]
    pub const fn published_retention(mut self, horizon: Duration) -> Self {
        self.published_retention = Some(horizon);
        self
    }

    /// Configure the `JetStream` publisher. Env equivalent: the
    /// `PROXIMA_NATS_*` block.
    #[cfg(feature = "outbox-nats")]
    #[must_use]
    pub fn nats(mut self, nats: proxima_outbox_nats::NatsPublisherConfig) -> Self {
        self.nats = Some(nats);
        self
    }

    /// Register a typed host-state participant on the [`crate::UnitOfWork`]
    /// write session. Hosts that register none keep existing Fact/sidecar/lock
    /// behavior with no extra configuration.
    ///
    /// Exactly one: a second registration — here, through
    /// [`crate::Proxima::host_state_participant`], or from another tuple
    /// element's [`crate::FlavorApp::configure`] — keeps the first and makes
    /// [`Self::resolve`] refuse, instead of silently replacing it.
    #[must_use]
    pub fn host_state_participant(mut self, participant: Arc<dyn PgHostStateParticipant>) -> Self {
        if self.host_state_participant.is_some() {
            self.duplicate_host_state_participant = true;
        } else {
            self.host_state_participant = Some(participant);
        }
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

    /// Grant the runtime role its DML privileges at boot, after migrating
    /// and before the runtime pool connects. Opt-in; off by default. Env
    /// equivalent: `PROXIMA_RUNTIME_GRANTS`.
    #[must_use]
    pub const fn runtime_grants(mut self, runtime_grants: bool) -> Self {
        self.runtime_grants = Some(runtime_grants);
        self
    }

    /// Copy these inbound request headers to tools as opaque
    /// [`RequestHeaders`] on the served paths (`/mcp`, `/v1`). Entries are
    /// header names or `prefix*`; credential headers are refused. Empty —
    /// the default — publishes nothing. Env equivalent (comma-separated):
    /// `PROXIMA_REQUEST_HEADERS`.
    #[must_use]
    pub fn request_headers(mut self, request_headers: Vec<String>) -> Self {
        self.request_headers = Some(request_headers);
        self
    }

    /// The owner-access port the served runtime resolves roles through: the
    /// edge's per-Group probe behind `X-Proxima-Owner` and the delegation
    /// service. Defaults to the Postgres resolver over the runtime pool. A
    /// host authenticator that resolves roles through its own port should
    /// pass that same port here, or the two can answer differently for the
    /// same subject.
    #[must_use]
    pub fn owner_access(mut self, owner_access: Arc<dyn OwnerAccessPort>) -> Self {
        self.owner_access = Some(owner_access);
        self
    }

    /// Trusted forwarder subjects and the fixed role each holds in whichever
    /// Group it selects; wraps the owner-access port. Env equivalent:
    /// `PROXIMA_FORWARDER_SUBJECTS` + `PROXIMA_FORWARDER_ROLE`.
    #[must_use]
    pub fn forwarder(mut self, policy: ForwarderPolicy) -> Self {
        self.forwarder = Some(policy);
        self
    }

    /// Host services published beside the flavors' own: visible to
    /// [`crate::FlavorApp::services`] through
    /// [`crate::AppContext::services`], and to every tool, request
    /// behavior, route, and worker through the composed set. Repeatable; a
    /// type published twice — by two calls or by a host and a flavor — is a
    /// boot error.
    #[must_use]
    pub fn services(mut self, services: FlavorServices) -> Self {
        self.services.push(services);
        self
    }

    /// Serve anonymous `GET /healthz` (process up) and `GET /readyz`
    /// (database reachable, not shutting down) on the MCP listener, outside
    /// the Host guard and bearer auth so an orchestrator probe needs
    /// neither. Off by default: a host that mounts its own would otherwise
    /// collide. Env equivalent: `PROXIMA_HEALTH_ENDPOINTS`.
    #[must_use]
    pub const fn health_endpoints(mut self, health_endpoints: bool) -> Self {
        self.health_endpoints = Some(health_endpoints);
        self
    }

    /// Largest accepted request body on the listener, in bytes (default
    /// 4 MiB). Env equivalent: `PROXIMA_MAX_REQUEST_BODY_BYTES`.
    #[must_use]
    pub const fn max_request_body_bytes(mut self, bytes: usize) -> Self {
        self.max_request_body_bytes = Some(bytes);
        self
    }

    /// rmcp Streamable HTTP tuning. Its body cap is replaced by
    /// [`Self::max_request_body_bytes`] when that is set. Env equivalents:
    /// `PROXIMA_MCP_SSE_KEEP_ALIVE_SECS`, `PROXIMA_MCP_SSE_RETRY_SECS`,
    /// `PROXIMA_MCP_SESSIONS`, `PROXIMA_MCP_JSON_RESPONSE`.
    #[must_use]
    pub const fn mcp_transport(mut self, transport: McpTransportConfig) -> Self {
        self.mcp_transport = Some(transport);
        self
    }

    /// Set Postgres pool and per-connection timeout policy.
    ///
    /// Env equivalents are `PROXIMA_PG_MAX_CONNECTIONS`,
    /// `PROXIMA_PG_STATEMENT_TIMEOUT_MS`, `PROXIMA_PG_ACQUIRE_TIMEOUT_SECS`,
    /// `PROXIMA_PG_IDLE_TIMEOUT_SECS`, and `PROXIMA_PG_MAX_LIFETIME_SECS`.
    #[must_use]
    pub fn pg_pool_config(mut self, pg_pool_config: PgPoolConfig) -> Self {
        self.pg_pool_config = Some(pg_pool_config);
        self
    }

    /// Set the Postgres query tuning, bypassing the `PROXIMA_PG_*` search
    /// variables a deployment would otherwise read. Its defaults are this
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

    /// One embedding client for every Owner. Mutually exclusive with
    /// [`Self::embedding_router`].
    #[must_use]
    pub fn embed_client(mut self, client: Arc<dyn EmbeddingClient>) -> Self {
        self.embed_client = Some(client);
        self
    }

    /// Per-Owner embedding routing (`Engine::with_embedding_router`).
    /// Mutually exclusive with [`Self::embed_client`].
    #[must_use]
    pub fn embedding_router(mut self, router: Arc<dyn EmbeddingRouter>) -> Self {
        self.embedding_router = Some(router);
        self
    }

    /// Set provider batching, request timeout, worker cadence, and durable
    /// claim lifecycle as one validated policy block.
    #[must_use]
    pub fn embedding_runtime_policy(mut self, policy: EmbeddingRuntimePolicy) -> Self {
        self.embedding_runtime_policy = Some(policy);
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
    /// Empty or whitespace-only is unset; values are trimmed.
    /// `PROXIMA_ALLOWED_ORIGINS=` therefore does not override a
    /// programmatically set base — it leaves the field untouched.
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
        if self.platform_database_url.is_none() {
            self.platform_database_url = lookup("PROXIMA_PLATFORM_DATABASE_URL");
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
        if self.runtime_grants.is_none() {
            self.runtime_grants = lookup("PROXIMA_RUNTIME_GRANTS")
                .map(|raw| parse_bool_value("PROXIMA_RUNTIME_GRANTS", &raw))
                .transpose()?;
        }
        self.apply_served_path_lookup(&lookup)?;
        if self.pg_pool_config.is_none() {
            self.pg_pool_config = pg_pool_config_from_lookup(&lookup)?;
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
        if self.embedding_runtime_policy.is_none() && embedding_runtime_policy_env_is_set(&lookup) {
            self.embedding_runtime_policy = Some(embedding_runtime_policy_from_lookup(&lookup)?);
        }
        // Unconditional: the block is read (and validated) on every boot,
        // independent of any cargo feature. A malformed bound is a boot
        // error whether or not this binary can talk to a broker.
        if self.publication.is_none() {
            self.publication = Some(publication_config_from_lookup(&lookup)?);
        }
        // Read outside the `publication` guard on purpose: retention is a
        // host-only housekeeping horizon, not part of the engine's
        // publication configuration, so a host that bound the engine block
        // programmatically has said nothing about it.
        if self.published_retention.is_none() {
            self.published_retention = crate::config::published_retention_from_lookup(&lookup)?;
        }
        #[cfg(feature = "outbox-nats")]
        if self.nats.is_none() {
            self.nats = crate::config::nats_from_lookup(&lookup)?;
        }
        Ok(self)
    }

    /// The served-path block of [`Self::apply_lookup`]: request headers,
    /// forwarder policy, health probes, transport, and the OIDC capture.
    fn apply_served_path_lookup(
        &mut self,
        lookup: &impl Fn(&str) -> Option<String>,
    ) -> Result<(), ProximaError> {
        if self.request_headers.is_none() {
            self.request_headers = lookup("PROXIMA_REQUEST_HEADERS")
                .map(|raw| raw.split(',').map(ToOwned::to_owned).collect());
        }
        if self.forwarder.is_none() {
            self.forwarder = forwarder_from_lookup(lookup)?;
        }
        if self.health_endpoints.is_none() {
            self.health_endpoints = lookup("PROXIMA_HEALTH_ENDPOINTS")
                .map(|raw| parse_bool_value("PROXIMA_HEALTH_ENDPOINTS", &raw))
                .transpose()?;
        }
        if self.max_request_body_bytes.is_none() {
            self.max_request_body_bytes = lookup("PROXIMA_MAX_REQUEST_BODY_BYTES")
                .map(|raw| {
                    raw.parse::<usize>().map_err(|_| {
                        ProximaError::Config(format!(
                            "PROXIMA_MAX_REQUEST_BODY_BYTES must be a byte count, got {raw:?}"
                        ))
                    })
                })
                .transpose()?;
        }
        if self.mcp_transport.is_none() {
            self.mcp_transport = mcp_transport_from_lookup(lookup)?;
        }
        if self.oidc_env.is_none() && lookup("PROXIMA_OIDC_ISSUER").is_some() {
            self.oidc_env = Some(
                oidc_env_keys()
                    .iter()
                    .filter_map(|key| lookup(key).map(|value| (*key, value)))
                    .collect(),
            );
        }
        Ok(())
    }

    /// The environment's OIDC authenticator fills in only when no layer set
    /// one in code, and resolves through a port bound at boot — the same one
    /// the edge and the delegation service get.
    #[cfg(feature = "auth-oidc")]
    fn env_authenticator(&mut self) -> Result<Option<LateOwnerAccess>, ProximaError> {
        if self.authenticator.is_some() {
            return Ok(None);
        }
        let Some(oidc_env) = self.oidc_env.take() else {
            return Ok(None);
        };
        let late = LateOwnerAccess::default();
        let lookup = |key: &str| oidc_env.get(key).cloned();
        let Some((authenticator, metadata)) =
            crate::auth::oidc_from_lookup(&lookup, Arc::new(late.clone()))?
        else {
            return Ok(None);
        };
        self.authenticator = Some(authenticator);
        self.resource_metadata.get_or_insert(metadata);
        Ok(Some(late))
    }

    /// Without `auth-oidc` there is no authenticator to build: an issuer the
    /// operator set is a request for authentication this binary cannot
    /// honour, refused rather than ignored.
    #[cfg(not(feature = "auth-oidc"))]
    fn env_authenticator(&mut self) -> Result<Option<LateOwnerAccess>, ProximaError> {
        if self.authenticator.is_none() && self.oidc_env.take().is_some() {
            return Err(ProximaError::Config(
                "PROXIMA_OIDC_ISSUER is set but this binary was built without the `auth-oidc` \
                 cargo feature; enable it or install an authenticator in code"
                    .into(),
            ));
        }
        Ok(None)
    }

    /// The served-path half of [`Self::resolve`]: the environment OIDC
    /// authenticator, the request-header allowlist, the merged host service
    /// bag, and the transport with its one body cap.
    fn take_served_path(&mut self) -> Result<ServedPath, ProximaError> {
        let late_owner_access = self.env_authenticator()?;
        let request_headers =
            RequestHeaderAllowlist::parse(self.request_headers.as_deref().unwrap_or_default())
                .map_err(|err| ProximaError::Config(err.to_string()))?;
        let mut services = FlavorServices::new();
        for bag in std::mem::take(&mut self.services) {
            services.try_extend(bag)?;
        }
        if services.get::<RequestHeaders>().is_some() {
            return Err(ProximaError::Config(
                "RequestHeaders is request-scoped; publish headers with request_headers \
                     (PROXIMA_REQUEST_HEADERS), not as a boot service"
                    .into(),
            ));
        }
        let mut mcp_transport = self.mcp_transport.unwrap_or_default();
        if let Some(bytes) = self.max_request_body_bytes {
            mcp_transport.max_request_body_bytes = bytes;
        }
        if mcp_transport.max_request_body_bytes == 0 {
            return Err(ProximaError::Config(
                "the request body limit must be at least 1 byte".into(),
            ));
        }
        Ok(ServedPath {
            late_owner_access,
            request_headers,
            services,
            mcp_transport,
        })
    }

    /// Resolve the builder into pure config plus host-provided runtime parts.
    ///
    /// # Errors
    ///
    /// Returns `ProximaError::Config` for missing required config and
    /// `ProximaError::Security` for fail-closed validation failures.
    pub fn resolve(mut self) -> Result<(RuntimeConfig, RuntimeParts), ProximaError> {
        if self.duplicate_host_state_participant {
            return Err(ProximaError::Config(
                DUPLICATE_HOST_STATE_PARTICIPANT.into(),
            ));
        }
        let database_url = self
            .database_url
            .take()
            .ok_or_else(|| ProximaError::Config("DATABASE_URL is required".into()))?;
        let ServedPath {
            late_owner_access,
            request_headers,
            services,
            mcp_transport,
        } = self.take_served_path()?;
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
        // One floor, both doors. The environment path refuses a sub-minute
        // horizon while parsing; a host that set it programmatically would
        // otherwise get a prune loop that runs every minute over a horizon
        // shorter than its own interval.
        if let Some(horizon) = self.published_retention
            && horizon < crate::config::MIN_PUBLISHED_RETENTION
        {
            return Err(ProximaError::Config(format!(
                "published retention is {}s, under the {}s floor; leave it unset to \
                 keep published records forever",
                horizon.as_secs(),
                crate::config::MIN_PUBLISHED_RETENTION.as_secs(),
            )));
        }
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
            embedding_router: self.embedding_router,
            host_state_participant: self.host_state_participant,
            owner_access: self.owner_access,
            services,
            late_owner_access,
        };
        let pg_pool_config = self.pg_pool_config.unwrap_or_default();
        let publication = self.publication.unwrap_or_default();
        let config = RuntimeConfig {
            database_url,
            platform_database_url: self.platform_database_url,
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
            runtime_grants: self.runtime_grants.unwrap_or(false),
            request_headers,
            forwarder: self.forwarder,
            health_endpoints: self.health_endpoints.unwrap_or(false),
            mcp_transport,
            pg_pool_config,
            pg_tuning: self.pg_tuning.unwrap_or_default(),
            auth: RuntimeAuthState {
                has_host_authenticator: parts.authenticator.is_some(),
            },
            resource_metadata: self.resource_metadata,
            embedding_runtime_policy: self.embedding_runtime_policy.unwrap_or_default(),
            publication: publication.clone(),
            published_retention: self.published_retention,
            #[cfg(feature = "outbox-nats")]
            nats: self.nats,
        };
        config.validate()?;
        Ok((config, parts))
    }
}

/// What [`RuntimeBuilder::take_served_path`] resolves.
struct ServedPath {
    late_owner_access: Option<LateOwnerAccess>,
    request_headers: RequestHeaderAllowlist,
    services: FlavorServices,
    mcp_transport: McpTransportConfig,
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
    pub platform_database_url: Option<String>,
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
    /// Grant the runtime role its DML privileges at boot
    /// (`PROXIMA_RUNTIME_GRANTS`), after migrating and before the runtime
    /// pool connects. Off by default: a DBA-managed deployment grants
    /// out-of-band (docs/15).
    pub runtime_grants: bool,
    /// Inbound headers copied to tools as `RequestHeaders`
    /// (`PROXIMA_REQUEST_HEADERS`). Empty publishes nothing.
    pub request_headers: RequestHeaderAllowlist,
    /// Trusted forwarder subjects and their fixed per-Group role
    /// (`PROXIMA_FORWARDER_*`), wrapped around the owner-access port.
    pub forwarder: Option<ForwarderPolicy>,
    /// Anonymous `/healthz` and `/readyz` on the MCP listener
    /// (`PROXIMA_HEALTH_ENDPOINTS`).
    pub health_endpoints: bool,
    /// rmcp tuning plus the one body cap both the listener guard and rmcp
    /// enforce (`PROXIMA_MAX_REQUEST_BODY_BYTES`, `PROXIMA_MCP_*`).
    pub mcp_transport: McpTransportConfig,
    /// Postgres pool size and timeout policy. Resolved once before storage
    /// construction; the canonical boot path never re-reads process env.
    pub pg_pool_config: PgPoolConfig,
    /// Postgres query tuning (`PROXIMA_PG_*`). Defaults are this release's
    /// shipped behaviour, so an unset environment is production.
    pub pg_tuning: PgTuning,
    pub auth: RuntimeAuthState,
    pub resource_metadata: Option<ResourceServerMetadata>,
    pub embedding_runtime_policy: EmbeddingRuntimePolicy,
    /// The deployment's `CloudEvents` producer identity and capture bounds
    /// (`PROXIMA_PUBLICATION_SOURCE`, `PROXIMA_OUTBOX_*`; docs/18).
    ///
    /// Always present, never optional: it is handed to the engine builder
    /// on every boot, and its `limits` are the ones enforced at capture —
    /// no backend holds a second copy.
    pub publication: PublicationConfig,
    /// How long a DELIVERED record is kept before the publisher task
    /// reclaims it (`PROXIMA_OUTBOX_PUBLISHED_RETENTION_SECS`; docs/18
    /// §Retention).
    ///
    /// `None` keeps published records forever — the behaviour of every
    /// release before the knob existed, and what `0` spells explicitly.
    /// Host housekeeping, deliberately NOT part of [`Self::publication`]:
    /// the engine never sees it, and the only port that can act on it is
    /// host-held.
    pub published_retention: Option<Duration>,
    /// The broker the captured outbox drains to (`PROXIMA_NATS_*`).
    ///
    /// `None` — the default — leaves the outbox captured and undrained,
    /// which is a safe state: nothing is lost, the backlog is bounded, and
    /// the publisher can be started later against the same records.
    #[cfg(feature = "outbox-nats")]
    pub nats: Option<proxima_outbox_nats::NatsPublisherConfig>,
}

#[derive(Debug, Clone, Copy)]
pub struct RuntimeAuthState {
    pub has_host_authenticator: bool,
}

impl std::fmt::Debug for RuntimeConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeConfig")
            .field("database_url", &"<redacted>")
            .field(
                "platform_database_url",
                &self.platform_database_url.as_ref().map(|_| "<redacted>"),
            )
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
            .field("runtime_grants", &self.runtime_grants)
            .field("request_headers", &self.request_headers)
            .field("forwarder", &self.forwarder)
            .field("health_endpoints", &self.health_endpoints)
            .field("mcp_transport", &self.mcp_transport)
            .field("pg_pool_config", &self.pg_pool_config)
            .field("pg_tuning", &self.pg_tuning)
            .field("auth", &self.auth)
            .field("resource_metadata", &self.resource_metadata)
            .field("embedding_runtime_policy", &self.embedding_runtime_policy)
            .field("publication", &self.publication)
            .field("published_retention", &self.published_retention)
            .finish_non_exhaustive()
    }
}

impl RuntimeConfig {
    /// Validate the fail-closed network/auth matrix.
    ///
    /// # Errors
    ///
    /// Returns `ProximaError::Security` when transport exposure is unsafe.
    pub fn validate(&self) -> Result<(), ProximaError> {
        self.pg_pool_config
            .validate()
            .map_err(|error| ProximaError::Config(error.to_string()))?;
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
    pub embedding_router: Option<Arc<dyn EmbeddingRouter>>,
    pub host_state_participant: Option<Arc<dyn PgHostStateParticipant>>,
    /// The host's owner-access port; `None` uses the runtime-pool resolver.
    pub owner_access: Option<Arc<dyn OwnerAccessPort>>,
    /// The merged host service bag ([`RuntimeBuilder::services`]).
    pub services: FlavorServices,
    /// Bound at boot to the runtime's owner-access port when the
    /// authenticator came from the `PROXIMA_OIDC_*` environment.
    pub(crate) late_owner_access: Option<LateOwnerAccess>,
}

impl std::fmt::Debug for RuntimeParts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeParts")
            .field("has_authenticator", &self.authenticator.is_some())
            .field("has_embed_client", &self.embed_client.is_some())
            .field("has_embedding_router", &self.embedding_router.is_some())
            .field(
                "has_host_state_participant",
                &self.host_state_participant.is_some(),
            )
            .field("has_owner_access", &self.owner_access.is_some())
            .field("services", &self.services)
            .field("env_authenticator", &self.late_owner_access.is_some())
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
    /// The target database does not match this binary's schema and must
    /// be reset. Distinct from [`Self::Storage`] so hosts can match on it.
    #[error(
        "database schema does not match this binary; reset required (see docs/how-to/migrations.md): {details}"
    )]
    SchemaResetRequired { details: String },
}

impl From<EmbedError> for ProximaError {
    fn from(value: EmbedError) -> Self {
        match value {
            EmbedError::Config(err) => Self::Config(err),
            EmbedError::Registry(err) => Self::Registry(err),
            EmbedError::Storage(err) => Self::Storage(err),
            EmbedError::Engine(err) => Self::Engine(err),
            EmbedError::SchemaResetRequired { details } => Self::SchemaResetRequired { details },
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

/// The variables the environment layer captures for the OIDC authenticator
/// built at resolve.
#[cfg(feature = "auth-oidc")]
fn oidc_env_keys() -> &'static [&'static str] {
    &crate::auth::OIDC_ENV_KEYS
}

/// Only the issuer: its presence is what a build without `auth-oidc` refuses.
#[cfg(not(feature = "auth-oidc"))]
fn oidc_env_keys() -> &'static [&'static str] {
    &["PROXIMA_OIDC_ISSUER"]
}

/// The `PROXIMA_MCP_*` transport block: `None` when none is set, else
/// rmcp's defaults with the set values applied. Seconds of `0` disable the
/// SSE ping or retry hint.
fn mcp_transport_from_lookup(
    lookup: &impl Fn(&str) -> Option<String>,
) -> Result<Option<McpTransportConfig>, ProximaError> {
    const KEEP_ALIVE: &str = "PROXIMA_MCP_SSE_KEEP_ALIVE_SECS";
    const RETRY: &str = "PROXIMA_MCP_SSE_RETRY_SECS";
    const SESSIONS: &str = "PROXIMA_MCP_SESSIONS";
    const JSON_RESPONSE: &str = "PROXIMA_MCP_JSON_RESPONSE";
    if [KEEP_ALIVE, RETRY, SESSIONS, JSON_RESPONSE]
        .iter()
        .all(|key| lookup(key).is_none())
    {
        return Ok(None);
    }
    let optional_secs = |key: &str| -> Result<Option<Option<Duration>>, ProximaError> {
        lookup(key)
            .map(|raw| parse_duration_seconds(key, &raw))
            .transpose()
            .map(|parsed| parsed.map(|duration| (!duration.is_zero()).then_some(duration)))
    };
    let mut transport = McpTransportConfig::default();
    if let Some(keep_alive) = optional_secs(KEEP_ALIVE)? {
        transport.sse_keep_alive = keep_alive;
    }
    if let Some(retry) = optional_secs(RETRY)? {
        transport.sse_retry = retry;
    }
    if let Some(raw) = lookup(SESSIONS) {
        transport.legacy_session_mode = parse_bool_value(SESSIONS, &raw)?;
    }
    if let Some(raw) = lookup(JSON_RESPONSE) {
        transport.json_response = parse_bool_value(JSON_RESPONSE, &raw)?;
    }
    Ok(Some(transport))
}

fn embedding_runtime_policy_env_is_set(lookup: &impl Fn(&str) -> Option<String>) -> bool {
    [
        proxima_core::PROXIMA_EMBED_REQUEST_TIMEOUT_SECONDS,
        proxima_core::PROXIMA_EMBED_BATCH_SIZE,
        proxima_core::PROXIMA_EMBED_WORKER_INTERVAL_SECONDS,
        proxima_core::PROXIMA_EMBED_STALE_CLAIM_TIMEOUT_SECONDS,
    ]
    .iter()
    .any(|key| proxima_core::env_value(lookup, key).is_some())
}

/// Parse the canonical generic embedding policy block through an injected
/// lookup. Shared by the facade env layer and host binaries that must build a
/// concrete embedding adapter before calling the facade.
///
/// # Errors
///
/// Rejects malformed, zero, out-of-range, and unsafe policy values.
pub fn embedding_runtime_policy_from_lookup(
    lookup: &impl Fn(&str) -> Option<String>,
) -> Result<EmbeddingRuntimePolicy, ProximaError> {
    EmbeddingRuntimePolicy::from_lookup(lookup).map_err(|err| ProximaError::Config(err.to_string()))
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

    struct NoopParticipant;

    #[async_trait]
    impl PgHostStateParticipant for NoopParticipant {
        fn participant_id(&self) -> proxima_core::storage_ports::HostStateParticipantId {
            proxima_core::storage_ports::HostStateParticipantId::new("noop")
        }

        fn declared_tables(&self) -> &'static [proxima_core::storage_ports::StateSurfaceName] {
            &[]
        }

        async fn apply(
            &self,
            _tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
            _permit: &proxima_core::storage_ports::HostStateWritePermit,
            _request: proxima_core::storage_ports::HostStateRequest,
        ) -> Result<proxima_core::storage_ports::HostStateReply, proxima_core::StorageError>
        {
            unreachable!("registration-only fixture")
        }
    }

    #[test]
    fn a_second_host_state_participant_refuses_instead_of_replacing_the_first() {
        let with =
            |builder: RuntimeBuilder| builder.host_state_participant(Arc::new(NoopParticipant));
        let base = || {
            RuntimeBuilder::default()
                .database_url("postgres://localhost/proxima")
                .tool_scope(ToolScope::All)
        };

        let (_, parts) = with(base()).resolve().expect("one participant resolves");
        assert!(parts.host_state_participant.is_some());

        let twice = with(with(base()))
            .resolve()
            .expect_err("two on one builder");
        assert!(twice.to_string().contains("already registered"), "{twice}");

        // An overlay's participant over a `FlavorApp::configure` one, as a
        // tuple app or `Proxima::host_state_participant` composes them.
        let layered = with(RuntimeBuilder::default())
            .merge_over(with(base()))
            .resolve()
            .expect_err("two across layers");
        assert!(
            layered.to_string().contains("already registered"),
            "{layered}"
        );
    }

    #[tokio::test]
    async fn the_embedded_builder_refuses_a_second_participant_before_connecting() {
        let config = crate::EmbedConfig {
            database_url: "postgres://nobody@127.0.0.1:1/unreachable".into(),
            platform_database_url: None,
            s3: None,
        };
        let refused = crate::ProximaBuilder::new(config, owner(uuid::Uuid::now_v7()))
            .host_state_participant(Arc::new(NoopParticipant))
            .host_state_participant(Arc::new(NoopParticipant))
            .boot()
            .await
            .err()
            .map(|error| error.to_string())
            .expect("two participants refuse");
        assert!(refused.contains("already registered"), "{refused}");
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
            publication: PublicationConfig::default(),
            published_retention: None,
            #[cfg(feature = "outbox-nats")]
            nats: None,
            database_url: "postgres://localhost/proxima".to_string(),
            platform_database_url: None,
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
            runtime_grants: false,
            request_headers: RequestHeaderAllowlist::default(),
            forwarder: None,
            health_endpoints: false,
            mcp_transport: McpTransportConfig::default(),
            pg_pool_config: PgPoolConfig::default(),
            pg_tuning: PgTuning::default(),
            auth: RuntimeAuthState {
                has_host_authenticator: true,
            },
            resource_metadata: None,
            embedding_runtime_policy: EmbeddingRuntimePolicy::default(),
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
            public_url: "https://proxima.example.com".to_string(),
            authorization_servers: vec!["https://idp.test".to_string()],
        });

        config.validate().unwrap();
    }

    #[test]
    fn host_of_url_extracts_bare_lowercased_host() {
        assert_eq!(
            host_of_url("https://proxima.example.com").as_deref(),
            Some("proxima.example.com")
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
        assert!(!is_loopback_host("proxima.example.com"));
        assert!(!is_loopback_host("10.0.0.5"));
    }

    #[test]
    fn public_allowed_hosts_derives_from_public_url_and_origins() {
        let mut config = base_config(Some(addr([127, 0, 0, 1])));
        config.resource_metadata = Some(ResourceServerMetadata {
            public_url: "https://proxima.example.com".to_string(),
            authorization_servers: vec![],
        });
        config.allowed_origins = vec![
            "https://app.test".to_string(),
            "http://localhost:5173".to_string(),
        ];

        // public_url host first, then non-loopback origin hosts; loopback dropped.
        assert_eq!(
            config.public_allowed_hosts(),
            vec!["proxima.example.com".to_string(), "app.test".to_string()]
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
    fn embedding_runtime_policy_env_is_typed_and_programmatic_override_wins() {
        let env_policy = RuntimeBuilder::default()
            .apply_lookup(lookup(&[
                (proxima_core::PROXIMA_EMBED_REQUEST_TIMEOUT_SECONDS, "30"),
                (proxima_core::PROXIMA_EMBED_BATCH_SIZE, "12"),
                (proxima_core::PROXIMA_EMBED_WORKER_INTERVAL_SECONDS, "7"),
                (
                    proxima_core::PROXIMA_EMBED_STALE_CLAIM_TIMEOUT_SECONDS,
                    "90",
                ),
            ]))
            .expect("valid env policy");
        assert_eq!(
            env_policy
                .embedding_runtime_policy
                .expect("env policy present")
                .batch_size(),
            12
        );

        let explicit = EmbeddingRuntimePolicy::new(
            Duration::from_secs(20),
            4,
            Duration::from_secs(2),
            Duration::from_mins(1),
        )
        .expect("valid explicit policy");
        let merged =
            env_policy.merge_over(RuntimeBuilder::default().embedding_runtime_policy(explicit));
        assert_eq!(
            merged.embedding_runtime_policy.expect("merged policy"),
            EmbeddingRuntimePolicy::new(
                Duration::from_secs(30),
                12,
                Duration::from_secs(7),
                Duration::from_secs(90),
            )
            .expect("valid expected policy")
        );

        let resolved = RuntimeBuilder::default()
            .database_url("postgres://localhost/proxima")
            .owner(owner(uuid::Uuid::now_v7()))
            .tool_scope(ToolScope::All)
            .embedding_runtime_policy(explicit)
            .resolve()
            .expect("programmatic policy resolves")
            .0;
        assert_eq!(resolved.embedding_runtime_policy, explicit);
    }

    #[test]
    fn unset_embedding_policy_env_does_not_override_programmatic_base() {
        let explicit = EmbeddingRuntimePolicy::new(
            Duration::from_secs(20),
            4,
            Duration::from_secs(2),
            Duration::from_mins(1),
        )
        .expect("valid explicit policy");
        let env_layer = RuntimeBuilder::default()
            .apply_lookup(lookup(&[]))
            .expect("unset env");
        let merged =
            env_layer.merge_over(RuntimeBuilder::default().embedding_runtime_policy(explicit));
        assert_eq!(merged.embedding_runtime_policy, Some(explicit));
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
            .apply_lookup(lookup(&[("PROXIMA_PG_HNSW_EF_SEARCH", "200")]))
            .unwrap()
            .resolve()
            .unwrap();

        assert_eq!(
            config.pg_tuning,
            PgTuning {
                hnsw_ef_search: 200,
                ..PgTuning::default()
            }
        );
    }

    #[test]
    fn pg_pool_config_reads_the_injected_env_block() {
        let (config, _) = RuntimeBuilder::default()
            .database_url("postgres://localhost/proxima")
            .owner(owner(uuid::Uuid::now_v7()))
            .tool_scope(ToolScope::All)
            .apply_lookup(lookup(&[
                ("PROXIMA_PG_MAX_CONNECTIONS", "4"),
                ("PROXIMA_PG_ACQUIRE_TIMEOUT_SECS", "9"),
            ]))
            .unwrap()
            .resolve()
            .unwrap();

        assert_eq!(config.pg_pool_config.max_connections, 4);
        assert_eq!(
            config.pg_pool_config.acquire_timeout,
            Duration::from_secs(9)
        );
    }

    #[test]
    fn programmatic_pg_pool_config_reaches_resolved_config() {
        let configured = PgPoolConfig {
            max_connections: 3,
            statement_timeout: Duration::from_secs(41),
            acquire_timeout: Duration::from_secs(2),
            idle_timeout: Duration::from_secs(17),
            max_lifetime: Duration::from_secs(29),
        };
        let (config, _) = RuntimeBuilder::default()
            .database_url("postgres://localhost/proxima")
            .owner(owner(uuid::Uuid::now_v7()))
            .tool_scope(ToolScope::All)
            .pg_pool_config(configured)
            .apply_lookup(lookup(&[("PROXIMA_PG_MAX_CONNECTIONS", "9")]))
            .expect("explicit pool policy outranks the injected lookup")
            .resolve()
            .unwrap();

        assert_eq!(config.pg_pool_config, configured);
    }

    #[test]
    fn invalid_pg_pool_config_fails_at_resolution() {
        let lookup_error = RuntimeBuilder::default()
            .apply_lookup(lookup(&[("PROXIMA_PG_MAX_CONNECTIONS", "many")]))
            .expect_err("malformed injected pool config must fail");
        assert!(
            lookup_error
                .to_string()
                .contains("PROXIMA_PG_MAX_CONNECTIONS=many")
        );

        let programmatic_error = RuntimeBuilder::default()
            .database_url("postgres://localhost/proxima")
            .owner(owner(uuid::Uuid::now_v7()))
            .tool_scope(ToolScope::All)
            .pg_pool_config(PgPoolConfig {
                max_connections: 0,
                ..PgPoolConfig::default()
            })
            .resolve()
            .expect_err("zero programmatic pool size must fail resolution");
        assert!(
            programmatic_error
                .to_string()
                .contains("at least one connection")
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
    fn a_silent_lookup_does_not_override_configured_pool_policy() {
        let configured = PgPoolConfig {
            max_connections: 3,
            ..PgPoolConfig::default()
        };
        let injected = RuntimeBuilder::default()
            .apply_lookup(lookup(&[("PROXIMA_REST_ENABLED", "true")]))
            .expect("silent pool lookup");

        let merged = injected.merge_over(RuntimeBuilder::default().pg_pool_config(configured));

        assert_eq!(merged.pg_pool_config, Some(configured));
    }

    #[test]
    fn an_explicit_default_pool_env_overrides_configured_pool_policy() {
        let configured = PgPoolConfig {
            max_connections: 3,
            ..PgPoolConfig::default()
        };
        let injected = RuntimeBuilder::default()
            .apply_lookup(lookup(&[("PROXIMA_PG_MAX_CONNECTIONS", "10")]))
            .expect("explicit default-valued pool env");
        let configured_base = RuntimeBuilder::default()
            .database_url("postgres://localhost/proxima")
            .owner(owner(uuid::Uuid::now_v7()))
            .tool_scope(ToolScope::All)
            .pg_pool_config(configured);

        let (config, _) = injected
            .merge_over(configured_base)
            .resolve()
            .expect("resolved pool policy");

        assert_eq!(config.pg_pool_config.max_connections, 10);
    }

    #[test]
    fn malformed_pg_tuning_errors() {
        let err = RuntimeBuilder::default()
            .apply_lookup(lookup(&[("PROXIMA_PG_HNSW_ITERATIVE_SCAN", "relaxed")]))
            .unwrap_err();

        assert!(
            err.to_string()
                .contains("PROXIMA_PG_HNSW_ITERATIVE_SCAN=relaxed")
        );
    }

    /// Empty or whitespace-only env is unset, not a parse error.
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

    /// Empty means unset, so it does not override a programmatically set base.
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
        // (core_transfer/core_membership included) by omitting `.tool_scope(...)`.
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

    /// The one-minute floor guards BOTH doors. The environment path refuses
    /// a sub-minute horizon while parsing; a host that composes
    /// programmatically must hit the same wall, or the knob would mean two
    /// different things depending on how it was set.
    #[test]
    fn a_programmatic_retention_horizon_is_floored_like_the_env_one() {
        let refused = RuntimeBuilder::default()
            .database_url("postgres://localhost/proxima")
            .owner(owner(uuid::Uuid::now_v7()))
            .tool_scope(ToolScope::All)
            .published_retention(Duration::from_secs(59))
            .resolve()
            .expect_err("a sub-minute horizon must be refused");
        assert!(
            matches!(refused, ProximaError::Config(ref message) if message.contains("floor")),
            "{refused}"
        );

        let (config, _) = RuntimeBuilder::default()
            .database_url("postgres://localhost/proxima")
            .owner(owner(uuid::Uuid::now_v7()))
            .tool_scope(ToolScope::All)
            .published_retention(Duration::from_mins(1))
            .resolve()
            .expect("the floor itself is acceptable");
        assert_eq!(config.published_retention, Some(Duration::from_mins(1)));

        let (default_config, _) = RuntimeBuilder::default()
            .database_url("postgres://localhost/proxima")
            .owner(owner(uuid::Uuid::now_v7()))
            .tool_scope(ToolScope::All)
            .resolve()
            .expect("no horizon is the default");
        assert_eq!(
            default_config.published_retention, None,
            "unset keeps delivered records forever"
        );
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

    fn served_builder() -> RuntimeBuilder {
        RuntimeBuilder::default()
            .database_url("postgres://localhost/proxima")
            .tool_scope(ToolScope::All)
    }

    #[test]
    fn served_path_env_resolves_into_config() {
        let forwarder = uuid::Uuid::now_v7().to_string();
        let (config, _) = served_builder()
            .apply_lookup(lookup(&[
                ("PROXIMA_REQUEST_HEADERS", "x-pack-ticket, X-Piy-Env-*"),
                ("PROXIMA_FORWARDER_SUBJECTS", &forwarder),
                ("PROXIMA_FORWARDER_ROLE", "ingest"),
                ("PROXIMA_HEALTH_ENDPOINTS", "true"),
                ("PROXIMA_MAX_REQUEST_BODY_BYTES", "8388608"),
                ("PROXIMA_MCP_SSE_KEEP_ALIVE_SECS", "0"),
                ("PROXIMA_MCP_SESSIONS", "false"),
            ]))
            .expect("valid env")
            .resolve()
            .expect("resolves");

        assert!(
            config
                .request_headers
                .allows(&http::HeaderName::from_static("x-piy-env-forgejo"))
        );
        assert_eq!(
            config.forwarder.as_ref().map(ForwarderPolicy::role),
            Some(proxima_core::Role::ingest())
        );
        assert!(config.health_endpoints);
        assert_eq!(config.mcp_transport.max_request_body_bytes, 8 * 1024 * 1024);
        assert_eq!(config.mcp_transport.sse_keep_alive, None);
        assert!(!config.mcp_transport.legacy_session_mode);
        assert_eq!(
            config.mcp_transport.sse_retry,
            McpTransportConfig::default().sse_retry,
            "an unset transport field keeps rmcp's default"
        );
    }

    #[test]
    fn served_path_defaults_change_nothing() {
        let (config, parts) = served_builder()
            .apply_lookup(lookup(&[]))
            .unwrap()
            .resolve()
            .unwrap();
        assert!(config.request_headers.is_empty());
        assert!(config.forwarder.is_none());
        assert!(!config.health_endpoints);
        assert_eq!(config.mcp_transport, McpTransportConfig::default());
        assert!(parts.late_owner_access.is_none());
        assert!(parts.authenticator.is_none());
    }

    #[test]
    fn malformed_served_path_values_are_refused() {
        for pairs in [
            [("PROXIMA_REQUEST_HEADERS", "authorization")],
            [("PROXIMA_MAX_REQUEST_BODY_BYTES", "8MiB")],
            [("PROXIMA_MAX_REQUEST_BODY_BYTES", "0")],
            [("PROXIMA_MCP_SSE_RETRY_SECS", "soon")],
            [("PROXIMA_HEALTH_ENDPOINTS", "maybe")],
        ] {
            let resolved = served_builder()
                .apply_lookup(lookup(&pairs))
                .and_then(RuntimeBuilder::resolve);
            assert!(resolved.is_err(), "{pairs:?} must be refused");
        }
    }

    #[test]
    fn host_service_bags_merge_across_layers_and_refuse_duplicates() {
        #[derive(Debug)]
        struct Budget;
        #[derive(Debug)]
        struct Ticketing;

        let base = RuntimeBuilder::default().services(FlavorServices::with(Budget));
        let (_, parts) = served_builder()
            .services(FlavorServices::with(Ticketing))
            .merge_over(base)
            .resolve()
            .expect("disjoint bags merge");
        assert!(parts.services.get::<Budget>().is_some());
        assert!(parts.services.get::<Ticketing>().is_some());

        let duplicate = served_builder()
            .services(FlavorServices::with(Budget))
            .merge_over(RuntimeBuilder::default().services(FlavorServices::with(Budget)))
            .resolve();
        assert!(matches!(duplicate, Err(ProximaError::FlavorServices(_))));

        let forged = served_builder()
            .services(FlavorServices::with(RequestHeaders::default()))
            .resolve();
        assert!(
            matches!(forged, Err(ProximaError::Config(message)) if message.contains("request-scoped"))
        );
    }

    #[cfg(feature = "auth-oidc")]
    #[tokio::test]
    async fn the_env_oidc_authenticator_fills_in_only_without_one_in_code() {
        let map = format!("sub:{}", uuid::Uuid::now_v7());
        let oidc = [
            ("PROXIMA_OIDC_ISSUER", "https://idp.test"),
            ("PROXIMA_OIDC_AUDIENCE", "proxima"),
            ("PROXIMA_PUBLIC_URL", "https://mcp.test"),
            ("PROXIMA_OIDC_SUBJECT_MAP", map.as_str()),
        ];
        let (config, parts) = served_builder()
            .apply_lookup(lookup(&oidc))
            .unwrap()
            .resolve()
            .expect("env OIDC resolves");
        assert!(parts.authenticator.is_some());
        assert!(parts.late_owner_access.is_some());
        assert!(config.auth.has_host_authenticator);
        assert_eq!(
            config.resource_metadata.map(|md| md.public_url),
            Some("https://mcp.test".to_owned())
        );

        let (_, parts) = served_builder()
            .authenticator(Arc::new(TestAuthenticator))
            .apply_lookup(lookup(&oidc))
            .unwrap()
            .resolve()
            .unwrap();
        assert!(
            parts.late_owner_access.is_none(),
            "a code authenticator wins; the env one is never built"
        );
    }
}

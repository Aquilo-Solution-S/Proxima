use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use proxima_blob_s3::S3RuntimeConfig;
use proxima_core::publication::PublicationConfig;
use proxima_core::publication::intake::DurableIntake;
use proxima_core::storage_ports::OwnerDropProofHandle;
use proxima_core::storage_ports::publication::ConsumerName;
use proxima_core::{
    Authenticator, EmbeddingClient, EmbeddingRouter, EmbeddingRuntimePolicy, FlavorServiceError,
    FlavorServices, OwnerAccessPort, OwnerId, RequestHeaders, RevalidationConfig, ServedSurface,
    is_loopback_host,
};
use proxima_mcp_server::{
    McpHostTools, McpTransportConfig, RequestHeaderAllowlist, ResourceServerMetadata,
};
use proxima_storage_pg::{PgHostStateParticipant, PgPlatformScope, PgPoolConfig, PgTuning};

use crate::MissingMigrationPolicy;
use crate::config::{
    parse_bool_value, pg_pool_config_from_lookup, pg_tuning_from_lookup,
    publication_config_from_lookup, s3_from_lookup,
};
use crate::owner_access::{ForwarderPolicy, LateOwnerAccess, forwarder_from_lookup};

/// What an authenticator built at boot receives
/// ([`RuntimeBuilder::authenticator_with_platform_scope`]).
#[derive(Clone)]
pub struct PlatformAuthContext {
    /// The runtime's platform scope, validated at boot
    /// ([`PgPlatformScope::new`]: platform role, ownership, `FORCE RLS`
    /// census) — the one way to run platform-scoped queries.
    pub platform_scope: PgPlatformScope,
    /// The runtime's owner-access port (host port or the Postgres resolver,
    /// under the forwarder policy when one is configured).
    pub owner_access: Arc<dyn OwnerAccessPort>,
}

impl std::fmt::Debug for PlatformAuthContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlatformAuthContext")
            .finish_non_exhaustive()
    }
}

/// Builds the host authenticator once the platform scope exists.
pub type PlatformAuthenticatorFactory =
    Arc<dyn Fn(PlatformAuthContext) -> Result<Arc<dyn Authenticator>, ProximaError> + Send + Sync>;

pub(crate) const DUPLICATE_HOST_STATE_PARTICIPANT: &str = "a host-state participant is already registered and a unit of work dispatches to exactly one; register one participant that serves every command type (docs/host-integration.md)";

/// Runtime configuration builder for host applications.
#[derive(Default)]
pub struct RuntimeBuilder {
    database_url: Option<String>,
    store_number: Option<u16>,
    platform_database_url: Option<String>,
    migration_database_url: Option<String>,
    s3: Option<S3RuntimeConfig>,
    owner: Option<OwnerId>,
    mcp_bind: Option<SocketAddr>,
    expose_network: Option<bool>,
    allowed_origins: Option<Vec<String>>,
    allowed_hosts: Option<Vec<String>>,
    served_surface: Option<ServedSurface>,
    stream_max_lifetime: Option<Duration>,
    epoch_check_interval: Option<Duration>,
    insecure_single_owner: bool,
    rest_enabled: Option<bool>,
    skip_migrations: Option<bool>,
    missing_migration_policy: Option<MissingMigrationPolicy>,
    pg_pool_config: Option<PgPoolConfig>,
    pg_tuning: Option<PgTuning>,
    authenticator: Option<Arc<dyn Authenticator>>,
    platform_authenticator: Option<PlatformAuthenticatorFactory>,
    host_tools: Option<Arc<dyn McpHostTools>>,
    vector_index: Option<Arc<dyn proxima_core::storage_ports::VectorIndex>>,
    resource_metadata: Option<ResourceServerMetadata>,
    embed_client: Option<Arc<dyn EmbeddingClient>>,
    embedding_router: Option<Arc<dyn EmbeddingRouter>>,
    embedding_runtime_policy: Option<EmbeddingRuntimePolicy>,
    publication: Option<PublicationConfig>,
    /// Horizon after which a DELIVERED outbox record is reclaimed. `None`
    /// leaves the default; zero keeps published records forever, as
    /// `PROXIMA_OUTBOX_PUBLISHED_RETENTION_SECS=0` spells.
    published_retention: Option<Duration>,
    /// How often the runtime sweeps expired uploads and drains object purge
    /// debt. `None` leaves the default; zero turns it off.
    object_purge_interval: Option<Duration>,
    outbox_consumers: OutboxConsumers,
    /// The built-in HTTP sink `PROXIMA_OUTBOX_HTTP_URL` configures; added
    /// as the consumer `http` at [`Self::resolve`].
    outbox_http: Option<Arc<crate::outbox_http::HttpIntake>>,
    host_state_participant: Option<Arc<dyn PgHostStateParticipant>>,
    /// A second participant was registered; [`Self::resolve`] refuses.
    duplicate_host_state_participant: bool,
    runtime_grants: Option<bool>,
    request_headers: Option<Vec<String>>,
    owner_access: Option<Arc<dyn OwnerAccessPort>>,
    owner_drop_proof: Option<OwnerDropProofHandle>,
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
            .field("store_number", &self.store_number)
            .field(
                "has_platform_database_url",
                &self.platform_database_url.is_some(),
            )
            .field(
                "has_migration_database_url",
                &self.migration_database_url.is_some(),
            )
            .field("s3", &self.s3)
            .field("owner", &self.owner)
            .field("mcp_bind", &self.mcp_bind)
            .field("expose_network", &self.expose_network)
            .field("allowed_origins", &self.allowed_origins)
            .field("allowed_hosts", &self.allowed_hosts)
            .field("served_surface", &self.served_surface)
            .field("stream_max_lifetime", &self.stream_max_lifetime)
            .field("epoch_check_interval", &self.epoch_check_interval)
            .field("insecure_single_owner", &self.insecure_single_owner)
            .field("rest_enabled", &self.rest_enabled)
            .field("skip_migrations", &self.skip_migrations)
            .field("missing_migration_policy", &self.missing_migration_policy)
            .field("pg_pool_config", &self.pg_pool_config)
            .field("pg_tuning", &self.pg_tuning)
            .field("has_authenticator", &self.authenticator.is_some())
            .field(
                "has_platform_authenticator",
                &self.platform_authenticator.is_some(),
            )
            .field("host_tools", &self.host_tools)
            .field("has_vector_index", &self.vector_index.is_some())
            .field("has_resource_metadata", &self.resource_metadata.is_some())
            .field("has_embed_client", &self.embed_client.is_some())
            .field("has_embedding_router", &self.embedding_router.is_some())
            .field("embedding_runtime_policy", &self.embedding_runtime_policy)
            .field("publication", &self.publication)
            .field("published_retention", &self.published_retention)
            .field("object_purge_interval", &self.object_purge_interval)
            .field("outbox_http", &self.outbox_http)
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
            .field("has_owner_drop_proof", &self.owner_drop_proof.is_some())
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
        // `authenticator` and `authenticator_with_platform_scope` are one
        // slot: whichever this layer set replaces whichever the base set.
        let (authenticator, platform_authenticator) =
            if self.authenticator.is_some() || self.platform_authenticator.is_some() {
                (self.authenticator, self.platform_authenticator)
            } else {
                (base.authenticator, base.platform_authenticator)
            };
        Self {
            database_url: self.database_url.or(base.database_url),
            store_number: self.store_number.or(base.store_number),
            platform_database_url: self.platform_database_url.or(base.platform_database_url),
            migration_database_url: self.migration_database_url.or(base.migration_database_url),
            s3: self.s3.or(base.s3),
            owner: self.owner.or(base.owner),
            mcp_bind: self.mcp_bind.or(base.mcp_bind),
            expose_network: self.expose_network.or(base.expose_network),
            allowed_origins: self.allowed_origins.or(base.allowed_origins),
            allowed_hosts: self.allowed_hosts.or(base.allowed_hosts),
            served_surface: self.served_surface.or(base.served_surface),
            stream_max_lifetime: self.stream_max_lifetime.or(base.stream_max_lifetime),
            epoch_check_interval: self.epoch_check_interval.or(base.epoch_check_interval),
            insecure_single_owner: self.insecure_single_owner || base.insecure_single_owner,
            rest_enabled: self.rest_enabled.or(base.rest_enabled),
            skip_migrations: self.skip_migrations.or(base.skip_migrations),
            missing_migration_policy: self
                .missing_migration_policy
                .or(base.missing_migration_policy),
            pg_pool_config: self.pg_pool_config.or(base.pg_pool_config),
            pg_tuning: self.pg_tuning.or(base.pg_tuning),
            authenticator,
            platform_authenticator,
            host_tools: self.host_tools.or(base.host_tools),
            vector_index: self.vector_index.or(base.vector_index),
            resource_metadata: self.resource_metadata.or(base.resource_metadata),
            embed_client: self.embed_client.or(base.embed_client),
            embedding_router: self.embedding_router.or(base.embedding_router),
            embedding_runtime_policy: self
                .embedding_runtime_policy
                .or(base.embedding_runtime_policy),
            publication: self.publication.or(base.publication),
            published_retention: self.published_retention.or(base.published_retention),
            object_purge_interval: self.object_purge_interval.or(base.object_purge_interval),
            outbox_consumers: base.outbox_consumers.overlaid_by(self.outbox_consumers),
            outbox_http: self.outbox_http.or(base.outbox_http),
            duplicate_host_state_participant: self.duplicate_host_state_participant
                || base.duplicate_host_state_participant
                || (self.host_state_participant.is_some() && base.host_state_participant.is_some()),
            host_state_participant: self.host_state_participant.or(base.host_state_participant),
            runtime_grants: self.runtime_grants.or(base.runtime_grants),
            request_headers: self.request_headers.or(base.request_headers),
            owner_access: self.owner_access.or(base.owner_access),
            owner_drop_proof: self.owner_drop_proof.or(base.owner_drop_proof),
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

    /// Assign this store its host-selected number. Required; the first boot
    /// persists it and every later boot must supply the same value.
    /// Environment equivalent: `PROXIMA_STORE_NUMBER`.
    #[must_use]
    pub const fn store_number(mut self, number: u16) -> Self {
        self.store_number = Some(number);
        self
    }

    /// Set the separate migration/platform connection string.
    /// Environment equivalent: `PROXIMA_PLATFORM_DATABASE_URL`.
    #[must_use]
    pub fn platform_database_url(mut self, url: impl Into<String>) -> Self {
        self.platform_database_url = Some(url.into());
        self
    }

    /// Set the connection boot migrates (or, with [`Self::skip_migrations`],
    /// runs its preflight) on, when it must differ from
    /// [`Self::platform_database_url`]: the platform URL goes through a
    /// transaction-mode pooler, this one reaches Postgres directly or
    /// through a session-mode pool. Migrations hold a session advisory lock
    /// and session settings for the whole run, which a transaction-mode
    /// pooler cannot carry. Same role and database as the platform URL.
    /// Unset: boot migrates on the platform URL. Environment equivalent:
    /// `PROXIMA_MIGRATION_DATABASE_URL`.
    #[must_use]
    pub fn migration_database_url(mut self, url: impl Into<String>) -> Self {
        self.migration_database_url = Some(url.into());
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
    pub fn owner(mut self, owner: OwnerId) -> Self {
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
    /// The default is seven days; `Duration::ZERO` keeps delivered records
    /// forever. The horizon can never reach a `pending` or `claimed` record whatever its age: an
    /// undelivered event is a promise this deployment has not kept, and no
    /// retention policy may quietly cancel it (docs/host-integration.md, kernel OB-9).
    ///
    /// Setting this suppresses the environment read, so a host that composes
    /// programmatically is not overridden by a stray variable. A non-zero
    /// value under the one-minute floor is refused at `build`, exactly as
    /// the environment path refuses it at boot.
    #[must_use]
    pub const fn published_retention(mut self, horizon: Duration) -> Self {
        self.published_retention = Some(horizon);
        self
    }

    /// How often the runtime sweeps expired uploads and drains object purge
    /// debt while an object store is configured. Env equivalent:
    /// `PROXIMA_OBJECT_PURGE_INTERVAL_SECS`; default 60 s.
    ///
    /// `Duration::ZERO` turns the in-process drain off, for a deployment
    /// that runs `proxima-mcp maintain-storage` on its own schedule instead.
    /// Setting this suppresses the environment read.
    #[must_use]
    pub const fn object_purge_interval(mut self, interval: Duration) -> Self {
        self.object_purge_interval = Some(interval);
        self
    }

    /// Deliver the captured outbox to `intake`, in this process: the
    /// one-consumer case of [`Self::outbox_consumer`], registered as the
    /// consumer `default` ([`ConsumerName::DEFAULT`]). Set, the outbox
    /// publisher starts and calls [`DurableIntake::accept`] once per captured
    /// event (docs/host-integration.md §Delivery). With no consumer configured nothing drains
    /// the outbox: Facts are still captured, the backlog is bounded by
    /// `max_pending`, and a later boot with an intake delivers it.
    ///
    /// Delivery is at-least-once: a crash between `accept` and the outbox
    /// recording it delivers the event again with a higher
    /// [`ReceivedEvent::delivered_count`], so the intake deduplicates on
    /// [`ReceivedEvent::id`]. `Ok` (`Accepted` or `Rejected`) records the
    /// event as published. An [`IntakeError`], or an `accept` that does not
    /// return within five seconds, releases it to be tried again after the
    /// drain's backoff (the poll interval, doubling up to 30 s while a pass
    /// delivers nothing). The runtime keeps no per-event state of its own.
    ///
    /// No environment variables: the drain's batch (64), lease (30 s) and poll
    /// interval (500 ms) are fixed. A later call replaces an earlier one.
    /// Published-record retention and the feature's quiesce/resume
    /// ([`crate::Feature::OutboxPublisher`]) apply to the drain.
    ///
    /// [`ReceivedEvent::delivered_count`]: proxima_core::publication::intake::ReceivedEvent::delivered_count
    /// [`ReceivedEvent::id`]: proxima_core::publication::intake::ReceivedEvent::id
    /// [`IntakeError`]: proxima_core::publication::intake::IntakeError
    #[must_use]
    pub fn outbox_intake(self, intake: Arc<dyn DurableIntake>) -> Self {
        self.outbox_consumer(ConsumerName::default(), intake)
    }

    /// Deliver the captured outbox to `intake` as the consumer `name`, in
    /// this process (docs/host-integration.md §Consumers).
    ///
    /// Each configured consumer gets its own drain and its own delivery lane
    /// per record: every listenable Fact captured after the consumer was first
    /// registered reaches it at least once, whatever the other consumers do.
    /// One that fails or lags holds back only its own lanes. Boot registers
    /// every configured name; a name stays registered — and keeps being owed
    /// every new record — until a host retires it
    /// ([`crate::BuiltProxima::outbox_consumers`]). A later call for the same
    /// name replaces the earlier intake.
    ///
    /// The delivery contract is [`Self::outbox_intake`]'s, per consumer.
    #[must_use]
    pub fn outbox_consumer(mut self, name: ConsumerName, intake: Arc<dyn DurableIntake>) -> Self {
        self.outbox_consumers.0.insert(name, intake);
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

    /// The MCP listener's bind address: set, MCP starts — `run()` listens
    /// on it, `build()` hands the router to the host. Unset, MCP is off.
    /// Env equivalent: `PROXIMA_MCP_BIND`. A flavor that serves MCP by
    /// default sets it in [`crate::FlavorApp::configure`].
    #[must_use]
    pub fn mcp_bind(mut self, bind: SocketAddr) -> Self {
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

    /// Set the deployment's served commands. Declared reads remain served;
    /// caller authorization independently requires each exact scope key.
    #[must_use]
    pub fn served_surface(mut self, served_surface: ServedSurface) -> Self {
        self.served_surface = Some(served_surface);
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
    /// (see docs/protocol.md). Off by default, and only effective when the crate is
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

    /// Choose whether migration ledgers may contain versions absent from the
    /// composed binary. The default is [`MissingMigrationPolicy::Reject`].
    /// Explicitly choose [`MissingMigrationPolicy::Allow`] when releases
    /// share a database during a rolling deployment. Checksums, dirty rows,
    /// and divergent migration lanes are still checked. Environment equivalent:
    /// `PROXIMA_MISSING_MIGRATION_POLICY` (`reject` or `allow`).
    #[must_use]
    pub fn missing_migration_policy(mut self, policy: MissingMigrationPolicy) -> Self {
        self.missing_migration_policy = Some(policy);
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

    /// The host's proof that a person was dropped
    /// ([`proxima_core::OwnerDropProofPort`]), which `erase_personal_owner` and
    /// `erase_personal_source_scope` require before they erase anything. With
    /// none installed both refuse with `DropProofPortUnavailable`.
    #[must_use]
    pub fn owner_drop_proof(mut self, owner_drop_proof: OwnerDropProofHandle) -> Self {
        self.owner_drop_proof = Some(owner_drop_proof);
        self
    }

    /// Trusted forwarder subjects and their fixed role for a verified,
    /// registered owner; wraps the owner-access port. Env equivalent:
    /// `PROXIMA_FORWARDER_SUBJECTS` + `PROXIMA_FORWARDER_ROLE`.
    #[must_use]
    pub fn forwarder(mut self, policy: ForwarderPolicy) -> Self {
        self.forwarder = Some(policy);
        self
    }

    /// Host services published beside the flavors' own: visible to
    /// [`crate::FlavorApp::services`] through
    /// [`crate::ProximaHost::services`], and to every tool, request
    /// behavior, route, and worker through the composed set. Repeatable; a
    /// type published twice — by two calls or by a host and a flavor — is a
    /// boot error.
    #[must_use]
    pub fn services(mut self, services: FlavorServices) -> Self {
        self.services.push(services);
        self
    }

    /// Read memory stores beside this one through `port` (issue #462):
    /// `core_search_memories` fans out to them, `core_derive` accepts
    /// bare UUID sources from another store, and `proxima://memory/<uuid>`
    /// reads through it using the number embedded in that UUID. Each store gets
    /// the default 2 s bound; publish an
    /// [`OtherStoresService`] with `with_timeout` through [`Self::services`]
    /// for another. Without a port every surface behaves as before.
    ///
    /// [`OtherStoresService`]: proxima_core::storage_ports::other_stores::OtherStoresService
    #[must_use]
    pub fn other_stores(
        self,
        port: Arc<dyn proxima_core::storage_ports::other_stores::OtherStores>,
    ) -> Self {
        self.services(FlavorServices::with(
            proxima_core::storage_ports::other_stores::OtherStoresService::new(port),
        ))
    }

    /// Serve anonymous `GET /healthz` (process up) and `GET /readyz`
    /// (database reachable and not shutting down) on the MCP listener.
    /// Observed embedding configuration faults keep readiness at HTTP 200
    /// with a sanitized degraded body; database failure or shutdown returns 503.
    /// Both probes sit outside the Host guard and bearer auth so an orchestrator
    /// probe needs neither. Off by default: a host that mounts its own would
    /// otherwise collide. Env equivalent: `PROXIMA_HEALTH_ENDPOINTS`.
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
    /// `PROXIMA_MCP_SSE_KEEP_ALIVE_SECS`, `PROXIMA_MCP_SESSION_IDLE_SECS`,
    /// `PROXIMA_MCP_SSE_RETRY_SECS`, `PROXIMA_MCP_SESSIONS`,
    /// `PROXIMA_MCP_JSON_RESPONSE`.
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

    /// Build the host authenticator at boot, after the runtime validated
    /// its platform scope, from that scope and the runtime's owner-access
    /// port — for an authenticator whose resolver runs platform-scoped
    /// queries. Requires `platform_database_url`; exclusive with
    /// [`Self::authenticator`] and the `PROXIMA_OIDC_*` environment.
    #[must_use]
    pub fn authenticator_with_platform_scope<F>(mut self, factory: F) -> Self
    where
        F: Fn(PlatformAuthContext) -> Result<Arc<dyn Authenticator>, ProximaError>
            + Send
            + Sync
            + 'static,
    {
        self.platform_authenticator = Some(Arc::new(factory));
        self
    }

    /// Serve `tools` on `/mcp` beside the registry's tools, through the same
    /// scope gate and request behaviors ([`McpHostTools`]).
    #[must_use]
    pub fn host_tools(mut self, tools: Arc<dyn McpHostTools>) -> Self {
        self.host_tools = Some(tools);
        self
    }

    /// Keep the store's chunk vectors in `index` instead of its own database
    /// (#519). The store keeps each memory's head and pays index debt as
    /// [`proxima_core::storage_ports::VectorIndex`] describes; the network
    /// form is [`crate::vector_index_http`]. Without it the store's pgvector
    /// index serves.
    ///
    /// The setting also picks the schema the boot installs
    /// ([`crate::host::VectorStorage::HostIndex`]): the store's database
    /// needs no `vector` extension. The kind is chosen when the store is
    /// installed.
    #[must_use]
    pub fn vector_index(
        mut self,
        index: Arc<dyn proxima_core::storage_ports::VectorIndex>,
    ) -> Self {
        self.vector_index = Some(index);
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
        if self.store_number.is_none() {
            self.store_number = lookup("PROXIMA_STORE_NUMBER")
                .map(|value| parse_store_number(&value))
                .transpose()?;
        }
        if self.database_url.is_none() {
            self.database_url = lookup("DATABASE_URL");
        }
        if self.platform_database_url.is_none() {
            self.platform_database_url = lookup("PROXIMA_PLATFORM_DATABASE_URL");
        }
        if self.migration_database_url.is_none() {
            self.migration_database_url = lookup("PROXIMA_MIGRATION_DATABASE_URL");
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
        if self.missing_migration_policy.is_none() {
            self.missing_migration_policy = missing_migration_policy_from_lookup(&lookup)?;
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
        // Unconditional: the block is read (and validated) on every boot.
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
        if self.object_purge_interval.is_none() {
            self.object_purge_interval = crate::config::object_purge_interval_from_lookup(&lookup)?;
        }
        if self.outbox_http.is_none() {
            self.outbox_http = crate::outbox_http::HttpIntake::from_lookup(&lookup)?.map(Arc::new);
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
        if self.authenticator.is_some() || self.platform_authenticator.is_some() {
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
        if self.authenticator.is_none()
            && self.platform_authenticator.is_none()
            && self.oidc_env.take().is_some()
        {
            return Err(ProximaError::Config(
                "PROXIMA_OIDC_ISSUER is set but this binary was built without the `auth-oidc` \
                 cargo feature; enable it or install an authenticator in code"
                    .into(),
            ));
        }
        Ok(None)
    }

    /// [`Self::authenticator_with_platform_scope`] runs at boot against the
    /// platform scope; refuse before storage what could never reach it.
    fn check_platform_authenticator(&self) -> Result<(), ProximaError> {
        if self.platform_authenticator.is_none() {
            return Ok(());
        }
        if self.authenticator.is_some() {
            return Err(ProximaError::Config(
                "set either authenticator or authenticator_with_platform_scope, not both".into(),
            ));
        }
        if self.platform_database_url.is_none() {
            return Err(ProximaError::Config(
                "authenticator_with_platform_scope needs the platform scope: set \
                 platform_database_url (PROXIMA_PLATFORM_DATABASE_URL)"
                    .into(),
            ));
        }
        Ok(())
    }

    /// The platform URL [`Self::resolve`] requires, and the migration URL
    /// checked against it.
    fn take_platform_urls(&mut self) -> Result<(String, Option<String>), ProximaError> {
        let platform = self.platform_database_url.take().ok_or_else(|| {
            ProximaError::Config(
                "PROXIMA_PLATFORM_DATABASE_URL is required for owner-RLS boot".into(),
            )
        })?;
        let migration = self.migration_database_url.take();
        if let Some(migration) = &migration {
            check_migration_database_url(migration, &platform)?;
        }
        Ok((platform, migration))
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

    /// The built-in HTTP sink joins as the consumer `http`. A host that also
    /// registered `http` itself is told so rather than one of the two
    /// silently winning.
    fn register_outbox_http(&mut self) -> Result<(), ProximaError> {
        let Some(http) = self.outbox_http.take() else {
            return Ok(());
        };
        let name = crate::outbox_http::consumer_name();
        if self.outbox_consumers.0.contains_key(&name) {
            return Err(ProximaError::Config(format!(
                "{} is set and the host also registered an outbox consumer named \
                 `http`; rename the host consumer or unset the variable",
                crate::outbox_http::ENV_URL
            )));
        }
        self.outbox_consumers.0.insert(name, http);
        Ok(())
    }

    /// One floor, both doors. The environment path refuses a sub-minute
    /// horizon while parsing; a host that set it programmatically would
    /// otherwise get a prune loop that runs every minute over a horizon
    /// shorter than its own interval.
    fn check_published_retention(&self) -> Result<(), ProximaError> {
        if let Some(horizon) = self.published_retention
            && !horizon.is_zero()
            && horizon < crate::config::MIN_PUBLISHED_RETENTION
        {
            return Err(ProximaError::Config(format!(
                "published retention is {}s, under the {}s floor; zero keeps \
                 published records forever",
                horizon.as_secs(),
                crate::config::MIN_PUBLISHED_RETENTION.as_secs(),
            )));
        }
        Ok(())
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
        self.check_platform_authenticator()?;
        let (platform_database_url, migration_database_url) = self.take_platform_urls()?;
        self.register_outbox_http()?;
        let ServedPath {
            late_owner_access,
            request_headers,
            services,
            mcp_transport,
        } = self.take_served_path()?;
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
        self.check_published_retention()?;
        let served_surface = self.served_surface.ok_or_else(|| {
            ProximaError::Config(
                "served_surface is required: pass ServedSurface::All to serve all registered commands \
                 or ServedSurface::Palette([...]) to restrict configured commands; declared reads \
                 remain served and each caller needs its exact scope keys"
                    .into(),
            )
        })?;
        let parts = RuntimeParts {
            authenticator: self.authenticator,
            platform_authenticator: self.platform_authenticator,
            host_tools: self.host_tools,
            vector_index: self.vector_index,
            embed_client: self.embed_client,
            embedding_router: self.embedding_router,
            host_state_participant: self.host_state_participant,
            owner_access: self.owner_access,
            owner_drop_proof: self.owner_drop_proof,
            services,
            late_owner_access,
        };
        let config = RuntimeConfig {
            database_url,
            store_number: self.store_number.ok_or_else(|| {
                ProximaError::Config(
                    "PROXIMA_STORE_NUMBER is required: assign this store a number".into(),
                )
            })?,
            platform_database_url,
            migration_database_url,
            s3: self.s3,
            owner: self.owner,
            mcp: self.mcp_bind.map(|bind| McpSettings { bind }),
            expose_network: self.expose_network.unwrap_or(false),
            allowed_origins: self.allowed_origins.unwrap_or_default(),
            allowed_hosts: self.allowed_hosts.unwrap_or_default(),
            served_surface,
            stream_revalidation,
            insecure_single_owner: self.insecure_single_owner,
            rest_enabled: self.rest_enabled.unwrap_or(false),
            skip_migrations: self.skip_migrations.unwrap_or(false),
            missing_migration_policy: self.missing_migration_policy.unwrap_or_default(),
            runtime_grants: self.runtime_grants.unwrap_or(false),
            request_headers,
            forwarder: self.forwarder,
            health_endpoints: self.health_endpoints.unwrap_or(false),
            mcp_transport,
            pg_pool_config: self.pg_pool_config.unwrap_or_default(),
            pg_tuning: self.pg_tuning.unwrap_or_default(),
            auth: RuntimeAuthState {
                has_host_authenticator: parts.has_authenticator(),
            },
            resource_metadata: self.resource_metadata,
            embedding_runtime_policy: self.embedding_runtime_policy.unwrap_or_default(),
            publication: self.publication.unwrap_or_default(),
            published_retention: Some(
                self.published_retention
                    .unwrap_or(crate::config::DEFAULT_PUBLISHED_RETENTION),
            )
            .filter(|horizon| !horizon.is_zero()),
            object_purge_interval: Some(
                self.object_purge_interval
                    .unwrap_or(crate::config::DEFAULT_OBJECT_PURGE_INTERVAL),
            )
            .filter(|interval| !interval.is_zero()),
            outbox_consumers: self.outbox_consumers,
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
    /// Host-assigned immutable store identity, persisted on first boot.
    pub store_number: u16,
    pub platform_database_url: String,
    /// The connection boot migrates and runs its preflight on
    /// (`PROXIMA_MIGRATION_DATABASE_URL`); `None` uses
    /// [`Self::platform_database_url`]. See [`Self::migrations_url`].
    pub migration_database_url: Option<String>,
    pub s3: Option<S3RuntimeConfig>,
    pub owner: Option<OwnerId>,
    pub mcp: Option<McpSettings>,
    pub expose_network: bool,
    pub allowed_origins: Vec<String>,
    /// Explicit inbound `Host` allowlist (`PROXIMA_ALLOWED_HOSTS`). Empty
    /// ⇒ derive from `resource_metadata.public_url` + `allowed_origins`.
    /// Bare hostnames or `host:port`; the listener always adds loopback and
    /// shares the resulting non-empty allowlist with rmcp's inner guard.
    pub allowed_hosts: Vec<String>,
    pub served_surface: ServedSurface,
    pub stream_revalidation: RevalidationConfig,
    pub insecure_single_owner: bool,
    /// Serve `/v1` beside `/mcp` on the same listener, inside the same Host,
    /// auth, and body-limit layers (docs/protocol.md). Two gates, both required: the
    /// `rest` cargo feature compiles the module, this flag serves it. A second
    /// transport projection is opt-in twice on purpose — compiling it is a
    /// build decision, exposing it is a deployment one.
    pub rest_enabled: bool,
    /// Boot without applying migrations (preflight only) — schema is migrated
    /// out-of-band under a DDL role in split-role `GitOps` deploys.
    pub skip_migrations: bool,
    /// Missing-version handling for normal boot and read-only preflight.
    pub missing_migration_policy: MissingMigrationPolicy,
    /// Grant the runtime role its DML privileges at boot
    /// (`PROXIMA_RUNTIME_GRANTS`), after migrating and before the runtime
    /// pool connects. Off by default: a DBA-managed deployment grants
    /// out-of-band (docs/operate.md).
    pub runtime_grants: bool,
    /// Inbound headers copied to tools as `RequestHeaders`
    /// (`PROXIMA_REQUEST_HEADERS`). Empty publishes nothing.
    pub request_headers: RequestHeaderAllowlist,
    /// Trusted forwarder subjects and their fixed verified-owner role
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
    /// (`PROXIMA_PUBLICATION_SOURCE`, `PROXIMA_OUTBOX_*`; docs/host-integration.md).
    ///
    /// Always present, never optional: it is handed to the engine builder
    /// on every boot, and its `limits` are the ones enforced at capture —
    /// no backend holds a second copy.
    pub publication: PublicationConfig,
    /// How long a DELIVERED record is kept before the publisher task
    /// reclaims it (`PROXIMA_OUTBOX_PUBLISHED_RETENTION_SECS`; docs/host-integration.md
    /// §Retention).
    ///
    /// Seven days unless set; `None` keeps published records forever, which
    /// `0` spells explicitly. Host housekeeping, deliberately NOT part of [`Self::publication`]:
    /// the engine never sees it, and the only port that can act on it is
    /// host-held.
    pub published_retention: Option<Duration>,
    /// How often the runtime sweeps expired uploads and drains object purge
    /// debt while an object store is configured
    /// (`PROXIMA_OBJECT_PURGE_INTERVAL_SECS`, default 60 s). `None` when
    /// turned off with `0`.
    pub object_purge_interval: Option<Duration>,
    /// The host intakes the captured outbox is delivered to, in this
    /// process, by consumer name ([`RuntimeBuilder::outbox_consumer`],
    /// [`RuntimeBuilder::outbox_intake`]).
    ///
    /// Empty — the default — leaves the outbox captured and undrained,
    /// which is a safe state: nothing is lost, the backlog is bounded, and
    /// a later boot with an intake delivers the same records.
    pub outbox_consumers: OutboxConsumers,
}

/// The configured outbox consumers, by name, in name order.
#[derive(Clone, Default)]
pub struct OutboxConsumers(std::collections::BTreeMap<ConsumerName, Arc<dyn DurableIntake>>);

impl OutboxConsumers {
    /// Whether any consumer is configured: the outbox publisher's start
    /// rule, and what makes `core/goal-change-v1` listenable.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Every configured consumer and its intake, in name order.
    pub fn iter(&self) -> impl Iterator<Item = (&ConsumerName, &Arc<dyn DurableIntake>)> {
        self.0.iter()
    }

    /// The configured names, in order.
    pub fn names(&self) -> impl Iterator<Item = &ConsumerName> {
        self.0.keys()
    }

    /// `self`, with every name `overlay` configures replaced by its intake.
    fn overlaid_by(mut self, overlay: Self) -> Self {
        self.0.extend(overlay.0);
        self
    }
}

impl std::fmt::Debug for OutboxConsumers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.0.keys()).finish()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RuntimeAuthState {
    pub has_host_authenticator: bool,
}

impl std::fmt::Debug for RuntimeConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeConfig")
            .field("database_url", &"<redacted>")
            .field("store_number", &self.store_number)
            .field("platform_database_url", &"<redacted>")
            .field(
                "migration_database_url",
                &self.migration_database_url.as_ref().map(|_| "<redacted>"),
            )
            .field("s3", &self.s3)
            .field("owner", &self.owner)
            .field("mcp", &self.mcp)
            .field("expose_network", &self.expose_network)
            .field("allowed_origins", &self.allowed_origins)
            .field("allowed_hosts", &self.allowed_hosts)
            .field("served_surface", &self.served_surface)
            .field("stream_revalidation", &self.stream_revalidation)
            .field("insecure_single_owner", &self.insecure_single_owner)
            .field("rest_enabled", &self.rest_enabled)
            .field("skip_migrations", &self.skip_migrations)
            .field("missing_migration_policy", &self.missing_migration_policy)
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
            .field("object_purge_interval", &self.object_purge_interval)
            .field("outbox_consumers", &self.outbox_consumers)
            .finish_non_exhaustive()
    }
}

impl RuntimeConfig {
    /// The URL boot migrates and runs its preflight on:
    /// [`Self::migration_database_url`] when set, otherwise
    /// [`Self::platform_database_url`].
    #[must_use]
    pub fn migrations_url(&self) -> &str {
        self.migration_database_url
            .as_deref()
            .unwrap_or(&self.platform_database_url)
    }

    /// Validate the fail-closed network/auth matrix.
    ///
    /// # Errors
    ///
    /// Returns `ProximaError::Security` when transport exposure is unsafe,
    /// and `ProximaError::Config` for an invalid pool configuration or a
    /// migration URL that does not name the platform URL's role and database.
    pub fn validate(&self) -> Result<(), ProximaError> {
        self.pg_pool_config
            .validate()
            .map_err(|error| ProximaError::Config(error.to_string()))?;
        if let Some(migration) = &self.migration_database_url {
            check_migration_database_url(migration, &self.platform_database_url)?;
        }
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
    /// Builds [`Self::authenticator`] at boot from the platform scope.
    pub platform_authenticator: Option<PlatformAuthenticatorFactory>,
    /// Host tools served on `/mcp` ([`RuntimeBuilder::host_tools`]).
    pub host_tools: Option<Arc<dyn McpHostTools>>,
    /// The store's host vector index ([`RuntimeBuilder::vector_index`]).
    pub vector_index: Option<Arc<dyn proxima_core::storage_ports::VectorIndex>>,
    pub embed_client: Option<Arc<dyn EmbeddingClient>>,
    pub embedding_router: Option<Arc<dyn EmbeddingRouter>>,
    pub host_state_participant: Option<Arc<dyn PgHostStateParticipant>>,
    /// The host's owner-access port; `None` uses the runtime-pool resolver.
    pub owner_access: Option<Arc<dyn OwnerAccessPort>>,
    /// The host's personal-owner drop proof ([`RuntimeBuilder::owner_drop_proof`]).
    pub owner_drop_proof: Option<OwnerDropProofHandle>,
    /// The merged host service bag ([`RuntimeBuilder::services`]).
    pub services: FlavorServices,
    /// Bound at boot to the runtime's owner-access port when the
    /// authenticator came from the `PROXIMA_OIDC_*` environment.
    pub(crate) late_owner_access: Option<LateOwnerAccess>,
}

impl RuntimeParts {
    /// An authenticator is installed, or built at boot from the platform
    /// scope.
    #[must_use]
    pub fn has_authenticator(&self) -> bool {
        self.authenticator.is_some() || self.platform_authenticator.is_some()
    }
}

impl std::fmt::Debug for RuntimeParts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeParts")
            .field("has_authenticator", &self.authenticator.is_some())
            .field(
                "has_platform_authenticator",
                &self.platform_authenticator.is_some(),
            )
            .field("host_tools", &self.host_tools)
            .field("has_vector_index", &self.vector_index.is_some())
            .field("has_embed_client", &self.embed_client.is_some())
            .field("has_embedding_router", &self.embedding_router.is_some())
            .field(
                "has_host_state_participant",
                &self.host_state_participant.is_some(),
            )
            .field("has_owner_access", &self.owner_access.is_some())
            .field("has_owner_drop_proof", &self.owner_drop_proof.is_some())
            .field("services", &self.services)
            .field("env_authenticator", &self.late_owner_access.is_some())
            .finish()
    }
}

/// The error a facade failure wraps. A message alone converts with
/// `.into()`.
pub type Cause = Box<dyn std::error::Error + Send + Sync>;

/// Name a failure in the facade's words and keep the failure as the source.
pub fn cause_with(
    message: impl Into<String>,
    source: impl std::error::Error + Send + Sync + 'static,
) -> Cause {
    #[derive(Debug)]
    struct Named {
        message: String,
        source: Cause,
    }
    impl std::fmt::Display for Named {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(&self.message)
        }
    }
    impl std::error::Error for Named {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(self.source.as_ref())
        }
    }
    Box::new(Named {
        message: message.into(),
        source: Box::new(source),
    })
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
    /// Carries the error the storage layer returned, so a host can walk
    /// [`std::error::Error::source`]; the message reads as that error does.
    #[error("storage: {0}")]
    Storage(#[source] Cause),
    #[error("engine: {0}")]
    Engine(#[source] Cause),
    #[error("security: {0}")]
    Security(String),
    #[error("mcp: {0}")]
    Mcp(#[source] Cause),
    /// The target database does not match this binary's schema and must
    /// be reset. Distinct from [`Self::Storage`] so hosts can match on it.
    #[error(
        "database schema does not match this binary; reset required (see docs/contributing.md#migrations): {details}"
    )]
    SchemaResetRequired { details: String },
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

/// Read `PROXIMA_MIGRATION_DATABASE_URL` through `lookup` and check it
/// against `platform_database_url` the way boot does: same role, same
/// database. `None` when unset; the caller then migrates on the platform URL.
/// For entry points that migrate without the facade (a maintenance command
/// that opens its own migration pool).
///
/// # Errors
///
/// [`ProximaError::Config`] when either URL does not parse or the migration
/// URL names another role or database. Neither URL is echoed.
pub fn migration_database_url_from_lookup(
    lookup: &impl Fn(&str) -> Option<String>,
    platform_database_url: &str,
) -> Result<Option<String>, ProximaError> {
    let Some(migration) = proxima_core::env_value(lookup, "PROXIMA_MIGRATION_DATABASE_URL") else {
        return Ok(None);
    };
    check_migration_database_url(&migration, platform_database_url)?;
    Ok(Some(migration))
}

/// Parse an explicitly configured missing-version policy.
///
/// `PROXIMA_MISSING_MIGRATION_POLICY` accepts `reject` or `allow`. An unset
/// value returns `None` so configuration layers preserve the host's choice;
/// callers without layers use [`MissingMigrationPolicy::default`].
///
/// # Errors
///
/// Returns [`ProximaError::Config`] for any other nonempty value.
pub fn missing_migration_policy_from_lookup(
    lookup: &impl Fn(&str) -> Option<String>,
) -> Result<Option<MissingMigrationPolicy>, ProximaError> {
    proxima_core::env_value(lookup, "PROXIMA_MISSING_MIGRATION_POLICY")
        .map(|raw| match raw.as_str() {
            "reject" => Ok(MissingMigrationPolicy::Reject),
            "allow" => Ok(MissingMigrationPolicy::Allow),
            _ => Err(ProximaError::Config(
                "PROXIMA_MISSING_MIGRATION_POLICY must be reject or allow".into(),
            )),
        })
        .transpose()
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

pub(crate) fn parse_duration_seconds(key: &str, raw: &str) -> Result<Duration, ProximaError> {
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
/// SSE ping, the session idle timeout, or the retry hint.
fn mcp_transport_from_lookup(
    lookup: &impl Fn(&str) -> Option<String>,
) -> Result<Option<McpTransportConfig>, ProximaError> {
    const KEEP_ALIVE: &str = "PROXIMA_MCP_SSE_KEEP_ALIVE_SECS";
    const SESSION_IDLE: &str = "PROXIMA_MCP_SESSION_IDLE_SECS";
    const RETRY: &str = "PROXIMA_MCP_SSE_RETRY_SECS";
    const SESSIONS: &str = "PROXIMA_MCP_SESSIONS";
    const JSON_RESPONSE: &str = "PROXIMA_MCP_JSON_RESPONSE";
    if [KEEP_ALIVE, SESSION_IDLE, RETRY, SESSIONS, JSON_RESPONSE]
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
    if let Some(idle) = optional_secs(SESSION_IDLE)? {
        transport.session_idle_timeout = idle;
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

/// `PROXIMA_MIGRATION_DATABASE_URL` is another way to reach the platform
/// scope, not another scope: migrations create objects owned by the role
/// they run as, and the platform role owns the schema. Refuse a URL that
/// names another role or database; host, port and connection parameters
/// may differ. Neither URL is echoed: it could carry a password.
fn check_migration_database_url(migration: &str, platform: &str) -> Result<(), ProximaError> {
    use std::str::FromStr as _;

    let parse = |url: &str, variable: &str| {
        sqlx::postgres::PgConnectOptions::from_str(url)
            .map_err(|_| ProximaError::Config(format!("{variable} is not a valid Postgres URL")))
    };
    let migration = parse(migration, "PROXIMA_MIGRATION_DATABASE_URL")?;
    let platform = parse(platform, "PROXIMA_PLATFORM_DATABASE_URL")?;
    if migration.get_username() != platform.get_username() {
        return Err(ProximaError::Config(
            "PROXIMA_MIGRATION_DATABASE_URL must connect as the role of \
             PROXIMA_PLATFORM_DATABASE_URL (the schema owner)"
                .into(),
        ));
    }
    if migration.get_database() != platform.get_database() {
        return Err(ProximaError::Config(
            "PROXIMA_MIGRATION_DATABASE_URL must name the database of \
             PROXIMA_PLATFORM_DATABASE_URL"
                .into(),
        ));
    }
    Ok(())
}

/// Parse the host-assigned store number at the configuration boundary.
///
/// # Errors
///
/// Returns `ProximaError::Config` for a non-integer or a value outside `u16`.
pub fn parse_store_number(raw: &str) -> Result<u16, ProximaError> {
    raw.trim().parse().map_err(|_| {
        ProximaError::Config("PROXIMA_STORE_NUMBER must be an integer from 0 to 65535".into())
    })
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
    use proxima_core::env_from_pairs as lookup;
    use proxima_core::{AuthError, AuthzContext, Credentials, GroupId, OwnerId};

    use super::*;

    #[test]
    fn a_wrapped_failure_keeps_its_message_and_its_source() {
        let io = std::io::Error::other("disk gone");
        let error = ProximaError::Storage(cause_with("reading the ledger", io));
        assert_eq!(error.to_string(), "storage: reading the ledger");
        let cause = std::error::Error::source(&error).expect("the cause is the source");
        assert_eq!(
            std::error::Error::source(cause)
                .expect("the failure")
                .to_string(),
            "disk gone"
        );
        let plain = ProximaError::Mcp("listener disabled".into());
        assert_eq!(plain.to_string(), "mcp: listener disabled");
    }

    #[test]
    fn missing_migrations_require_an_explicit_host_policy() {
        let resolve = |builder: RuntimeBuilder| {
            builder
                .database_url("postgres://runtime/proxima")
                .store_number(0)
                .platform_database_url("postgres://platform/proxima")
                .served_surface(ServedSurface::All)
                .resolve()
                .expect("host config")
                .0
        };
        assert_eq!(
            resolve(RuntimeBuilder::default()).missing_migration_policy,
            MissingMigrationPolicy::Reject
        );
        let rolling =
            || RuntimeBuilder::default().missing_migration_policy(MissingMigrationPolicy::Allow);
        assert_eq!(
            resolve(rolling()).missing_migration_policy,
            MissingMigrationPolicy::Allow
        );
        assert_eq!(
            resolve(RuntimeBuilder::default().merge_over(rolling())).missing_migration_policy,
            MissingMigrationPolicy::Allow
        );
        assert_eq!(
            resolve(
                RuntimeBuilder::default()
                    .missing_migration_policy(MissingMigrationPolicy::Reject)
                    .merge_over(rolling())
            )
            .missing_migration_policy,
            MissingMigrationPolicy::Reject
        );
    }

    #[test]
    fn missing_migration_policy_is_parsed_once_at_the_configuration_boundary() {
        for (raw, expected) in [
            ("allow", Some(MissingMigrationPolicy::Allow)),
            (" reject ", Some(MissingMigrationPolicy::Reject)),
            (" ", None),
        ] {
            let configured = [("PROXIMA_MISSING_MIGRATION_POLICY", raw)];
            let from = lookup(&configured);
            assert_eq!(
                missing_migration_policy_from_lookup(&from).expect("valid policy"),
                expected
            );
            assert_eq!(
                RuntimeBuilder::default()
                    .apply_lookup(&from)
                    .expect("valid configuration")
                    .missing_migration_policy,
                expected
            );
        }
        let invalid = lookup(&[("PROXIMA_MISSING_MIGRATION_POLICY", "sometimes")]);
        assert!(missing_migration_policy_from_lookup(&invalid).is_err());
        assert!(RuntimeBuilder::default().apply_lookup(&invalid).is_err());
        assert_eq!(
            RuntimeBuilder::default()
                .missing_migration_policy(MissingMigrationPolicy::Reject)
                .apply_lookup(&invalid)
                .expect("explicit host choice wins")
                .missing_migration_policy,
            Some(MissingMigrationPolicy::Reject)
        );
    }

    #[test]
    fn store_number_is_required_and_parsed_at_configuration() {
        let base = || {
            RuntimeBuilder::default()
                .database_url("postgres://unused/db")
                .platform_database_url("postgres://unused/platform")
                .served_surface(ServedSurface::All)
        };
        let missing = base().resolve().expect_err("a store needs a number");
        assert!(missing.to_string().contains("PROXIMA_STORE_NUMBER"));
        for raw in ["-1", "65536", "three", "1.5"] {
            let error = base()
                .apply_lookup(|key| (key == "PROXIMA_STORE_NUMBER").then(|| raw.to_owned()))
                .expect_err("invalid outside store number");
            assert!(
                error.to_string().contains("PROXIMA_STORE_NUMBER"),
                "{error}"
            );
        }
        for number in [0, 3, u16::MAX] {
            let (config, _) = base()
                .apply_lookup(|key| (key == "PROXIMA_STORE_NUMBER").then(|| number.to_string()))
                .expect("valid store number")
                .resolve()
                .expect("number supplied");
            assert_eq!(config.store_number, number);
        }
        let (config, _) = base()
            .store_number(3)
            .apply_lookup(|key| (key == "PROXIMA_STORE_NUMBER").then(|| "4".into()))
            .expect("explicit value takes precedence")
            .resolve()
            .expect("complete config");
        assert_eq!(config.store_number, 3);
    }

    #[test]
    fn platform_database_url_is_required_at_resolve() {
        let error = RuntimeBuilder::default()
            .database_url("postgres://runtime/proxima")
            .served_surface(ServedSurface::All)
            .resolve()
            .expect_err("owner-RLS boot must require the platform URL");
        assert!(
            error.to_string().contains("PROXIMA_PLATFORM_DATABASE_URL"),
            "{error}"
        );
    }
    fn with_platform(builder: RuntimeBuilder) -> RuntimeBuilder {
        builder
            .database_url("postgres://runtime@db/proxima")
            .platform_database_url("postgres://platform@pooler:6432/proxima")
            .store_number(0)
            .served_surface(ServedSurface::All)
    }

    #[test]
    fn migrations_use_the_platform_url_unless_a_migration_url_is_set() {
        let (config, _) = with_platform(RuntimeBuilder::default())
            .resolve()
            .expect("resolves");
        assert_eq!(config.migration_database_url, None);
        assert_eq!(
            config.migrations_url(),
            "postgres://platform@pooler:6432/proxima"
        );

        let (config, _) = with_platform(
            RuntimeBuilder::default()
                .apply_lookup(lookup(&[(
                    "PROXIMA_MIGRATION_DATABASE_URL",
                    "postgres://platform@db:5432/proxima",
                )]))
                .expect("env layer"),
        )
        .resolve()
        .expect("a direct URL for the same role and database resolves");
        assert_eq!(
            config.migrations_url(),
            "postgres://platform@db:5432/proxima"
        );
        assert_eq!(
            config.platform_database_url,
            "postgres://platform@pooler:6432/proxima"
        );
        assert!(
            !format!("{config:?}").contains("db:5432"),
            "Debug redacts the migration URL"
        );
    }

    #[test]
    fn a_migration_url_must_name_the_platform_role_and_database() {
        for (url, names) in [
            ("postgres://admin@db:5432/proxima", "role"),
            ("postgres://platform@db:5432/other", "database"),
            ("not a url", "valid Postgres URL"),
        ] {
            let error = with_platform(RuntimeBuilder::default().migration_database_url(url))
                .resolve()
                .expect_err("refused");
            let message = error.to_string();
            assert!(
                message.contains("PROXIMA_MIGRATION_DATABASE_URL") && message.contains(names),
                "{url}: {message}"
            );
            assert!(
                !message.contains("db:5432"),
                "the URL is not echoed: {message}"
            );
        }
    }

    use crate::company_owner;

    fn owner(id: uuid::Uuid) -> OwnerId {
        OwnerId::from(GroupId::new(id))
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
            _session: &mut proxima_storage_pg::PgHostStateSession<'_, '_>,
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
                .store_number(0)
                .platform_database_url("postgres://localhost/platform")
                .served_surface(ServedSurface::All)
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

    /// The built-in HTTP sink (#462): registered as `http` only when its URL
    /// is set; a bad URL fails boot; a host consumer named `http` clashes.
    #[test]
    fn the_http_outbox_sink_registers_only_when_its_url_is_set() {
        let base = || {
            RuntimeBuilder::default()
                .database_url("postgres://localhost/proxima")
                .store_number(0)
                .platform_database_url("postgres://localhost/platform")
                .served_surface(ServedSurface::All)
        };
        let names = |config: &RuntimeConfig| {
            config
                .outbox_consumers
                .names()
                .map(|name| name.as_str().to_owned())
                .collect::<Vec<_>>()
        };

        let (unset, _) = base()
            .apply_lookup(lookup(&[]))
            .expect("no outbox env parses")
            .resolve()
            .expect("resolves");
        assert!(names(&unset).is_empty(), "URL unset: no consumer");

        let (set, _) = base()
            .apply_lookup(lookup(&[(
                "PROXIMA_OUTBOX_HTTP_URL",
                "https://receiver.example/events",
            )]))
            .expect("a URL parses")
            .resolve()
            .expect("resolves");
        assert_eq!(names(&set), ["http"]);

        for bad in ["not a url", "ftp://receiver.example/", "https://"] {
            base()
                .apply_lookup(lookup(&[("PROXIMA_OUTBOX_HTTP_URL", bad)]))
                .expect_err(bad);
        }
        let clear_text_token = base()
            .apply_lookup(lookup(&[
                ("PROXIMA_OUTBOX_HTTP_URL", "http://receiver.example/"),
                ("PROXIMA_OUTBOX_HTTP_TOKEN_FILE", "/nonexistent"),
            ]))
            .expect_err("a token is never sent over http://");
        assert!(
            clear_text_token.to_string().contains("https"),
            "{clear_text_token}"
        );
        let (plaintext, _) = base()
            .apply_lookup(lookup(&[
                ("PROXIMA_OUTBOX_HTTP_URL", "http://receiver.example/"),
                ("PROXIMA_OUTBOX_HTTP_TOKEN_FILE", "/nonexistent"),
                ("PROXIMA_PLAINTEXT_HOSTS", "receiver.example"),
            ]))
            .expect("a named host takes a token over http://")
            .resolve()
            .expect("resolves");
        assert_eq!(names(&plaintext), ["http"]);
        for hosts in ["*", "10.0.0.0/8"] {
            base()
                .apply_lookup(lookup(&[("PROXIMA_PLAINTEXT_HOSTS", hosts)]))
                .expect_err("a bad plaintext host fails boot without a sink");
        }
        base()
            .apply_lookup(lookup(&[
                ("PROXIMA_OUTBOX_HTTP_URL", "https://receiver.example/"),
                ("PROXIMA_OUTBOX_HTTP_CA_FILE", "/nonexistent/ca.pem"),
            ]))
            .expect_err("an unreadable CA fails boot");

        let clash = base()
            .outbox_consumer(
                crate::outbox_http::consumer_name(),
                Arc::new(crate::outbox_http::tests::Never),
            )
            .apply_lookup(lookup(&[(
                "PROXIMA_OUTBOX_HTTP_URL",
                "https://receiver.example/events",
            )]))
            .expect("parses")
            .resolve()
            .expect_err("a host consumer named http clashes");
        assert!(clash.to_string().contains("`http`"), "{clash}");
    }

    /// MCP's start rule at resolve: a bind address is the whole switch.
    #[test]
    fn mcp_is_configured_exactly_when_a_bind_address_is() {
        let base = || {
            RuntimeBuilder::default()
                .database_url("postgres://localhost/proxima")
                .store_number(0)
                .platform_database_url("postgres://localhost/platform")
                .served_surface(ServedSurface::All)
                .authenticator(Arc::new(TestAuthenticator))
        };
        let (off, _) = base().resolve().expect("no bind resolves");
        assert!(off.mcp.is_none(), "no bind address means MCP is off");

        let (from_env, _) = base()
            .apply_lookup(lookup(&[("PROXIMA_MCP_BIND", "127.0.0.1:0")]))
            .expect("bind env parses")
            .resolve()
            .expect("env bind resolves");
        assert_eq!(
            from_env.mcp.map(|mcp| mcp.bind),
            Some(addr_port([127, 0, 0, 1], 0))
        );

        // Code wins over the environment.
        let (code, _) = base()
            .mcp_bind(addr_port([127, 0, 0, 1], 7))
            .apply_lookup(lookup(&[("PROXIMA_MCP_BIND", "127.0.0.1:0")]))
            .expect("bind env parses")
            .resolve()
            .expect("code bind resolves");
        assert_eq!(
            code.mcp.map(|mcp| mcp.bind),
            Some(addr_port([127, 0, 0, 1], 7))
        );
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
            object_purge_interval: Some(crate::config::DEFAULT_OBJECT_PURGE_INTERVAL),
            outbox_consumers: OutboxConsumers::default(),
            database_url: "postgres://localhost/proxima".to_string(),
            store_number: 0,
            platform_database_url: "postgres://unused/platform".into(),
            migration_database_url: None,
            s3: None,
            owner: Some(company_owner(uuid::Uuid::now_v7())),
            mcp: mcp.map(|bind| McpSettings { bind }),
            expose_network: false,
            allowed_origins: Vec::new(),
            allowed_hosts: Vec::new(),
            served_surface: ServedSurface::All,
            stream_revalidation: RevalidationConfig::default(),
            insecure_single_owner: false,
            rest_enabled: false,
            skip_migrations: false,
            missing_migration_policy: MissingMigrationPolicy::default(),
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
        addr_port(ip, 31415)
    }

    fn addr_port(ip: [u8; 4], port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::from(ip)), port)
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
            additional_scopes: Vec::new(),
        });

        config.validate().unwrap();
    }

    #[test]
    fn public_allowed_hosts_derives_from_public_url_and_origins() {
        let mut config = base_config(Some(addr([127, 0, 0, 1])));
        config.resource_metadata = Some(ResourceServerMetadata {
            public_url: "https://proxima.example.com".to_string(),
            authorization_servers: vec![],
            additional_scopes: Vec::new(),
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
            .store_number(0)
            .platform_database_url("postgres://localhost/platform")
            .owner(owner(uuid::Uuid::now_v7()))
            .served_surface(ServedSurface::All)
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
            .store_number(0)
            .platform_database_url("postgres://localhost/platform")
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
            .store_number(0)
            .platform_database_url("postgres://localhost/platform")
            .owner(owner(uuid::Uuid::now_v7()))
            .served_surface(ServedSurface::All)
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
            .store_number(0)
            .platform_database_url("postgres://localhost/platform")
            .owner(owner(uuid::Uuid::now_v7()))
            .served_surface(ServedSurface::All)
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
    fn pg_pool_config_reads_the_injected_env_block() {
        let (config, _) = RuntimeBuilder::default()
            .database_url("postgres://localhost/proxima")
            .store_number(0)
            .platform_database_url("postgres://localhost/platform")
            .owner(owner(uuid::Uuid::now_v7()))
            .served_surface(ServedSurface::All)
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
            .store_number(0)
            .platform_database_url("postgres://localhost/platform")
            .owner(owner(uuid::Uuid::now_v7()))
            .served_surface(ServedSurface::All)
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
            .apply_lookup(lookup(&[("PROXIMA_PG_MAX_CONNECTIONS", "5")]))
            .expect("explicit default-valued pool env");
        let configured_base = RuntimeBuilder::default()
            .database_url("postgres://localhost/proxima")
            .store_number(0)
            .platform_database_url("postgres://localhost/platform")
            .owner(owner(uuid::Uuid::now_v7()))
            .served_surface(ServedSurface::All)
            .pg_pool_config(configured);

        let (config, _) = injected
            .merge_over(configured_base)
            .resolve()
            .expect("resolved pool policy");

        assert_eq!(config.pg_pool_config.max_connections, 5);
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
    fn resolve_without_served_surface_fails_closed_with_actionable_message() {
        // No embedding host may silently advertise the full tool surface
        // (core_transfer/core_membership included) by omitting `.served_surface(...)`.
        let err = RuntimeBuilder::default()
            .database_url("postgres://localhost/proxima")
            .store_number(0)
            .platform_database_url("postgres://localhost/platform")
            .owner(owner(uuid::Uuid::now_v7()))
            .resolve()
            .unwrap_err();

        let message = err.to_string();
        assert!(message.contains("served_surface is required"));
        assert!(message.contains("ServedSurface::All"));
        assert!(message.contains("ServedSurface::Palette"));
    }

    #[test]
    fn resolve_with_explicit_served_surface_all_builds_as_before() {
        let (config, _) = RuntimeBuilder::default()
            .database_url("postgres://localhost/proxima")
            .store_number(0)
            .platform_database_url("postgres://localhost/platform")
            .owner(owner(uuid::Uuid::now_v7()))
            .served_surface(ServedSurface::All)
            .resolve()
            .unwrap();

        assert_eq!(config.served_surface, ServedSurface::All);
    }

    /// The one-minute floor guards BOTH doors. The environment path refuses
    /// a sub-minute horizon while parsing; a host that composes
    /// programmatically must hit the same wall, or the knob would mean two
    /// different things depending on how it was set.
    #[test]
    fn a_programmatic_retention_horizon_is_floored_like_the_env_one() {
        let refused = RuntimeBuilder::default()
            .database_url("postgres://localhost/proxima")
            .store_number(0)
            .platform_database_url("postgres://localhost/platform")
            .owner(owner(uuid::Uuid::now_v7()))
            .served_surface(ServedSurface::All)
            .published_retention(Duration::from_secs(59))
            .resolve()
            .expect_err("a sub-minute horizon must be refused");
        assert!(
            matches!(refused, ProximaError::Config(ref message) if message.contains("floor")),
            "{refused}"
        );

        let (config, _) = RuntimeBuilder::default()
            .database_url("postgres://localhost/proxima")
            .store_number(0)
            .platform_database_url("postgres://localhost/platform")
            .owner(owner(uuid::Uuid::now_v7()))
            .served_surface(ServedSurface::All)
            .published_retention(Duration::from_mins(1))
            .resolve()
            .expect("the floor itself is acceptable");
        assert_eq!(config.published_retention, Some(Duration::from_mins(1)));

        let (default_config, _) = RuntimeBuilder::default()
            .database_url("postgres://localhost/proxima")
            .store_number(0)
            .platform_database_url("postgres://localhost/platform")
            .owner(owner(uuid::Uuid::now_v7()))
            .served_surface(ServedSurface::All)
            .resolve()
            .expect("no horizon is the default");
        assert_eq!(
            default_config.published_retention,
            Some(crate::config::DEFAULT_PUBLISHED_RETENTION),
            "unset reclaims delivered records after the default horizon"
        );

        let (keep_forever, _) = RuntimeBuilder::default()
            .database_url("postgres://localhost/proxima")
            .store_number(0)
            .platform_database_url("postgres://localhost/platform")
            .owner(owner(uuid::Uuid::now_v7()))
            .served_surface(ServedSurface::All)
            .published_retention(Duration::ZERO)
            .resolve()
            .expect("zero keeps delivered records forever");
        assert_eq!(keep_forever.published_retention, None);
    }

    #[test]
    fn runtime_config_accepts_custom_authenticator_without_separate_owner_access() {
        let (config, _) = RuntimeBuilder::default()
            .database_url("postgres://localhost/proxima")
            .store_number(0)
            .platform_database_url("postgres://localhost/platform")
            .owner(owner(uuid::Uuid::now_v7()))
            .served_surface(ServedSurface::All)
            .mcp_bind(addr([127, 0, 0, 1]))
            .authenticator(Arc::new(TestAuthenticator))
            .resolve()
            .unwrap();

        assert_eq!(config.mcp.unwrap().bind, addr([127, 0, 0, 1]));
    }

    /// The two ways to install an authenticator are one slot: an overlay
    /// setting either replaces the base's either, instead of both surviving
    /// the merge and refusing boot as "not both".
    #[test]
    fn an_overlay_authenticator_replaces_a_base_platform_authenticator() {
        let platform = || {
            RuntimeBuilder::default()
                .authenticator_with_platform_scope(|_| Ok(Arc::new(TestAuthenticator)))
        };
        let merged = RuntimeBuilder::default()
            .authenticator(Arc::new(TestAuthenticator))
            .merge_over(platform());
        assert!(merged.authenticator.is_some() && merged.platform_authenticator.is_none());

        let merged = platform()
            .merge_over(RuntimeBuilder::default().authenticator(Arc::new(TestAuthenticator)));
        assert!(merged.authenticator.is_none() && merged.platform_authenticator.is_some());

        // One layer naming both is still ambiguous.
        let refused = platform()
            .authenticator(Arc::new(TestAuthenticator))
            .database_url("postgres://unused/proxima")
            .store_number(0)
            .platform_database_url("postgres://unused/platform")
            .served_surface(ServedSurface::All)
            .resolve()
            .expect_err("both in one layer");
        assert!(refused.to_string().contains("not both"), "{refused}");
    }

    fn served_builder() -> RuntimeBuilder {
        RuntimeBuilder::default()
            .database_url("postgres://localhost/proxima")
            .store_number(0)
            .platform_database_url("postgres://localhost/platform")
            .served_surface(ServedSurface::All)
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
    fn session_idle_env_sets_the_timeout_and_zero_disables_it() {
        let transport = |value: &str| {
            served_builder()
                .apply_lookup(lookup(&[("PROXIMA_MCP_SESSION_IDLE_SECS", value)]))
                .and_then(RuntimeBuilder::resolve)
                .map(|(config, _)| config.mcp_transport)
        };
        let set = transport("40").expect("40 s resolves");
        assert_eq!(set.session_idle_timeout, Some(Duration::from_secs(40)));
        assert_eq!(set.progress_heartbeat(), Duration::from_secs(20));
        let never = transport("0").expect("0 resolves");
        assert_eq!(never.session_idle_timeout, None);
        assert_eq!(
            never.progress_heartbeat(),
            proxima_mcp_server::DEFAULT_PROGRESS_HEARTBEAT
        );
        assert!(
            transport("soon").is_err(),
            "non-integer seconds are refused"
        );
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
        let map = format!(
            r#"[{{"iss":"https://idp.test","sub":"subject","user_id":"{}"}}]"#,
            uuid::Uuid::now_v7()
        );
        let oidc = [
            ("PROXIMA_OIDC_ISSUER", "https://idp.test"),
            ("PROXIMA_OIDC_AUDIENCE", "proxima"),
            ("PROXIMA_PUBLIC_URL", "https://mcp.test"),
            ("PROXIMA_OIDC_SUBJECT_MAP_JSON", map.as_str()),
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

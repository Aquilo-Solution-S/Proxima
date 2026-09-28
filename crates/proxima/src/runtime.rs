use std::convert::Infallible;
use std::marker::PhantomData;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::Request;
use axum::response::IntoResponse;
use proxima_blob_s3::{CitedBlobStore, S3RuntimeConfig};
use proxima_core::authz::SystemAuthority;
use proxima_core::storage_ports::{
    CitedBlobOwnerReconcileService, CitedBlobReadService, CitedBlobService,
    DelegatedAuthorityService,
};
use proxima_core::{
    AuthPath, Authenticator, AuthzContext, DelegationRuntimeAuthority, EmbeddingClient,
    EmbeddingRouter, FlavorRegistryFrozen, FlavorServices, OwnerAccessPort, RevalidationConfig,
    ToolScope,
};
use proxima_core::{EngineHandle, Owner, OwnerRef, Role, UserId};
use proxima_mcp_server::{
    HostAllowlist, McpEdgeAuth, McpToolHost, McpTransportConfig, OriginAllowlist, assert_loopback,
    body_limit_layer, cors_layer, default_allowlist, host_guard_layer,
};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tower::Service;

use crate::boot::Booted;
use crate::features::{BootReport, FeatureDecision, FeatureInputs, Features};
use crate::owner_access::ForwarderPolicy;
use crate::{AppContext, FlavorApp, ProximaError, ProximaHost, RuntimeBuilder};
use proxima_storage_pg::{PgDelegationStore, PgOwnerAccessResolver};

/// Application runtime facade.
///
/// Configuration layers, lowest first: [`FlavorApp::configure`] (the app's
/// defaults), the environment ([`Self::from_env`] / [`Self::from_lookup`]),
/// then this value's own builder calls — code wins. Every feature starts
/// when its config is present (docs/10 §Runtime features); [`Self::build`]
/// and [`Self::run`] start the same ones.
pub struct Proxima<A: FlavorApp> {
    overlay: RuntimeBuilder,
    use_env: bool,
    injected_env: Option<RuntimeBuilder>,
    _app: PhantomData<A>,
}

impl<A: FlavorApp> std::fmt::Debug for Proxima<A> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Proxima")
            .field("overlay", &self.overlay)
            .field("use_env", &self.use_env)
            .field("has_injected_env", &self.injected_env.is_some())
            .finish()
    }
}

impl<A: FlavorApp + 'static> Proxima<A> {
    #[must_use]
    pub fn app() -> Self {
        Self {
            overlay: RuntimeBuilder::default(),
            use_env: false,
            injected_env: None,
            _app: PhantomData,
        }
    }

    #[must_use]
    pub fn from_env(mut self) -> Self {
        self.use_env = true;
        self.injected_env = None;
        self
    }

    /// Resolve the environment layer through an injected lookup.
    ///
    /// This is the process-env-equivalent path for hosts whose configuration
    /// source is not global process state. Resolution happens once; storage
    /// boot consumes the resulting `RuntimeConfig` without another env read.
    ///
    /// # Errors
    ///
    /// Returns [`ProximaError::Config`] when a supplied value is malformed.
    pub fn from_lookup(
        mut self,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, ProximaError> {
        self.injected_env = Some(RuntimeBuilder::default().apply_lookup(lookup)?);
        self.use_env = false;
        Ok(self)
    }

    #[must_use]
    pub fn database_url(mut self, database_url: impl Into<String>) -> Self {
        self.overlay = self.overlay.database_url(database_url);
        self
    }

    /// Set the separate migration/platform database URL.
    #[must_use]
    pub fn platform_database_url(mut self, url: impl Into<String>) -> Self {
        self.overlay = self.overlay.platform_database_url(url);
        self
    }

    #[must_use]
    pub fn s3(mut self, s3: S3RuntimeConfig) -> Self {
        self.overlay = self.overlay.s3(s3);
        self
    }

    /// Set Postgres pool and per-connection timeout policy.
    #[must_use]
    pub fn pg_pool_config(mut self, config: proxima_storage_pg::PgPoolConfig) -> Self {
        self.overlay = self.overlay.pg_pool_config(config);
        self
    }

    /// [`RuntimeBuilder::pg_tuning`].
    #[must_use]
    pub fn pg_tuning(mut self, tuning: proxima_storage_pg::PgTuning) -> Self {
        self.overlay = self.overlay.pg_tuning(tuning);
        self
    }

    #[must_use]
    pub fn owner(mut self, owner: Owner) -> Self {
        self.overlay = self.overlay.owner(owner);
        self
    }

    #[must_use]
    pub fn authenticator(mut self, authenticator: Arc<dyn Authenticator>) -> Self {
        self.overlay = self.overlay.authenticator(authenticator);
        self
    }

    /// [`RuntimeBuilder::authenticator_with_platform_scope`].
    #[must_use]
    pub fn authenticator_with_platform_scope<F>(mut self, factory: F) -> Self
    where
        F: Fn(crate::PlatformAuthContext) -> Result<Arc<dyn Authenticator>, ProximaError>
            + Send
            + Sync
            + 'static,
    {
        self.overlay = self.overlay.authenticator_with_platform_scope(factory);
        self
    }

    /// [`RuntimeBuilder::host_tools`].
    #[must_use]
    pub fn host_tools(mut self, tools: Arc<dyn proxima_mcp_server::McpHostTools>) -> Self {
        self.overlay = self.overlay.host_tools(tools);
        self
    }

    /// [`RuntimeBuilder::record_mcp_calls`].
    #[must_use]
    pub fn record_mcp_calls(mut self, record: bool) -> Self {
        self.overlay = self.overlay.record_mcp_calls(record);
        self
    }

    #[must_use]
    pub fn resource_metadata(
        mut self,
        metadata: proxima_mcp_server::ResourceServerMetadata,
    ) -> Self {
        self.overlay = self.overlay.resource_metadata(metadata);
        self
    }

    /// [`RuntimeBuilder::mcp_bind`]: set, MCP starts.
    #[must_use]
    pub fn mcp_bind(mut self, bind: SocketAddr) -> Self {
        self.overlay = self.overlay.mcp_bind(bind);
        self
    }

    #[must_use]
    pub fn expose_network(mut self, expose_network: bool) -> Self {
        self.overlay = self.overlay.expose_network(expose_network);
        self
    }

    #[must_use]
    pub fn allowed_origins(mut self, allowed_origins: Vec<String>) -> Self {
        self.overlay = self.overlay.allowed_origins(allowed_origins);
        self
    }

    #[must_use]
    pub fn allowed_hosts(mut self, allowed_hosts: Vec<String>) -> Self {
        self.overlay = self.overlay.allowed_hosts(allowed_hosts);
        self
    }

    #[must_use]
    pub fn tool_scope(mut self, tool_scope: ToolScope) -> Self {
        self.overlay = self.overlay.tool_scope(tool_scope);
        self
    }

    #[must_use]
    pub fn stream_max_lifetime(mut self, duration: std::time::Duration) -> Self {
        self.overlay = self.overlay.stream_max_lifetime(duration);
        self
    }

    #[must_use]
    pub fn epoch_check_interval(mut self, duration: std::time::Duration) -> Self {
        self.overlay = self.overlay.epoch_check_interval(duration);
        self
    }

    #[must_use]
    pub fn allow_insecure_single_owner(mut self) -> Self {
        self.overlay = self.overlay.allow_insecure_single_owner();
        self
    }

    #[must_use]
    pub fn skip_migrations(mut self) -> Self {
        self.overlay = self.overlay.skip_migrations();
        self
    }

    /// Grant the runtime role its DML privileges at boot. Env equivalent:
    /// `PROXIMA_RUNTIME_GRANTS`.
    #[must_use]
    pub fn runtime_grants(mut self, runtime_grants: bool) -> Self {
        self.overlay = self.overlay.runtime_grants(runtime_grants);
        self
    }

    /// See [`RuntimeBuilder::request_headers`].
    #[must_use]
    pub fn request_headers(mut self, request_headers: Vec<String>) -> Self {
        self.overlay = self.overlay.request_headers(request_headers);
        self
    }

    /// See [`RuntimeBuilder::owner_access`].
    #[must_use]
    pub fn owner_access(mut self, owner_access: Arc<dyn OwnerAccessPort>) -> Self {
        self.overlay = self.overlay.owner_access(owner_access);
        self
    }

    /// See [`RuntimeBuilder::forwarder`].
    #[must_use]
    pub fn forwarder(mut self, policy: ForwarderPolicy) -> Self {
        self.overlay = self.overlay.forwarder(policy);
        self
    }

    /// See [`RuntimeBuilder::services`].
    #[must_use]
    pub fn services(mut self, services: FlavorServices) -> Self {
        self.overlay = self.overlay.services(services);
        self
    }

    /// See [`RuntimeBuilder::health_endpoints`].
    #[must_use]
    pub fn health_endpoints(mut self, health_endpoints: bool) -> Self {
        self.overlay = self.overlay.health_endpoints(health_endpoints);
        self
    }

    /// See [`RuntimeBuilder::max_request_body_bytes`].
    #[must_use]
    pub fn max_request_body_bytes(mut self, bytes: usize) -> Self {
        self.overlay = self.overlay.max_request_body_bytes(bytes);
        self
    }

    /// See [`RuntimeBuilder::mcp_transport`].
    #[must_use]
    pub fn mcp_transport(mut self, transport: McpTransportConfig) -> Self {
        self.overlay = self.overlay.mcp_transport(transport);
        self
    }

    /// Serve `/v1` beside `/mcp`. Env equivalent: `PROXIMA_REST_ENABLED`.
    #[must_use]
    pub fn rest_enabled(mut self, rest_enabled: bool) -> Self {
        self.overlay = self.overlay.rest_enabled(rest_enabled);
        self
    }

    #[must_use]
    pub fn embed_client(mut self, client: Arc<dyn EmbeddingClient>) -> Self {
        self.overlay = self.overlay.embed_client(client);
        self
    }

    #[must_use]
    pub fn embedding_router(mut self, router: Arc<dyn EmbeddingRouter>) -> Self {
        self.overlay = self.overlay.embedding_router(router);
        self
    }

    #[must_use]
    pub fn embedding_runtime_policy(
        mut self,
        policy: proxima_core::EmbeddingRuntimePolicy,
    ) -> Self {
        self.overlay = self.overlay.embedding_runtime_policy(policy);
        self
    }

    /// Bind the deployment's publication source and capture bounds
    /// (docs/18). Env equivalent: `PROXIMA_PUBLICATION_SOURCE`,
    /// `PROXIMA_OUTBOX_MAX_PENDING`, `PROXIMA_OUTBOX_MAX_PAYLOAD_BYTES`.
    #[must_use]
    pub fn publication(
        mut self,
        publication: proxima_core::publication::PublicationConfig,
    ) -> Self {
        self.overlay = self.overlay.publication(publication);
        self
    }

    /// Register a typed host-state participant on the [`crate::UnitOfWork`]
    /// write session. Hosts that register none keep existing Fact/sidecar/lock
    /// behavior with no extra configuration.
    #[must_use]
    pub fn host_state_participant(
        mut self,
        participant: Arc<dyn proxima_storage_pg::PgHostStateParticipant>,
    ) -> Self {
        self.overlay = self.overlay.host_state_participant(participant);
        self
    }

    /// [`RuntimeBuilder::published_retention`]: with the publisher running,
    /// the prune starts.
    #[must_use]
    pub fn published_retention(mut self, horizon: std::time::Duration) -> Self {
        self.overlay = self.overlay.published_retention(horizon);
        self
    }

    /// [`RuntimeBuilder::nats`]: set, the outbox publisher starts.
    #[cfg(feature = "outbox-nats")]
    #[must_use]
    pub fn nats(mut self, nats: proxima_outbox_nats::NatsPublisherConfig) -> Self {
        self.overlay = self.overlay.nats(nats);
        self
    }

    /// [`RuntimeBuilder::copy_cleaner`]: set, the retained-copy cleaner
    /// starts.
    #[cfg(feature = "outbox-nats")]
    #[must_use]
    pub fn copy_cleaner(
        mut self,
        cleaner: proxima_outbox_nats::JetStreamCopyCleanerConfig,
    ) -> Self {
        self.overlay = self.overlay.copy_cleaner(cleaner);
        self
    }

    /// Resolve, validate, boot, and start every configured feature, without
    /// binding a listener: with a bind address configured, MCP is assembled
    /// and handed to the host as [`BuiltProxima::service`] /
    /// [`BuiltProxima::mcp_edge`].
    ///
    /// # Errors
    ///
    /// Returns config, security, storage, engine, or MCP assembly errors.
    pub async fn build(self) -> Result<BuiltProxima, ProximaError> {
        let BootedRuntime {
            config,
            parts,
            allowlist,
            booted,
            cancel,
            app_ctx,
            owner_access,
        } = self.boot_common().await?;

        let (service, mcp_edge, mcp) = match (config.mcp, allowlist) {
            (Some(mcp), Some(allowlist)) => {
                let edge =
                    resolve_mcp_edge(&app_ctx, &parts, owner_access, allowlist, &cancel, &config);
                let service = build_router::<A>(app_ctx.clone(), &edge, &cancel, &config);
                (
                    Some(service),
                    Some(edge),
                    FeatureDecision::mcp_router_handed(mcp.bind),
                )
            }
            _ => (None, None, FeatureDecision::mcp_off()),
        };
        let runtime = Runtime::start::<A>(booted, app_ctx.host, cancel, &config, mcp);
        Ok(BuiltProxima {
            runtime,
            service,
            mcp_edge,
        })
    }

    /// Resolve, validate, boot, start every configured feature, and listen
    /// when a bind address is configured.
    ///
    /// # Errors
    ///
    /// Returns config, security, storage, engine, bind, or MCP serving errors.
    pub async fn run(self) -> Result<RunningProxima, ProximaError> {
        let BootedRuntime {
            config,
            parts,
            allowlist,
            booted,
            cancel,
            app_ctx,
            owner_access,
        } = self.boot_common().await?;

        let (mcp_addr, server, mcp) = if let (Some(mcp), Some(allowlist)) = (config.mcp, allowlist)
        {
            if !config.expose_network {
                assert_loopback(&mcp.bind)
                    .map_err(|err| ProximaError::Security(err.to_string()))?;
            }
            let edge =
                resolve_mcp_edge(&app_ctx, &parts, owner_access, allowlist, &cancel, &config);
            let app = build_router::<A>(app_ctx.clone(), &edge, &cancel, &config);
            let listener = tokio::net::TcpListener::bind(mcp.bind)
                .await
                .map_err(|err| ProximaError::Mcp(err.to_string()))?;
            let bound = listener
                .local_addr()
                .map_err(|err| ProximaError::Mcp(err.to_string()))?;
            let shutdown = cancel.clone();
            let server = tokio::spawn(async move {
                if let Err(err) = axum::serve(listener, app)
                    .with_graceful_shutdown(async move {
                        shutdown.cancelled_owned().await;
                    })
                    .await
                {
                    tracing::warn!(error = %err, "proxima facade server failed");
                }
            });
            booted
                .engine
                .set_mcp_url(format!("http://{bound}/mcp"))
                .await;
            (
                Some(bound),
                Some(server),
                FeatureDecision::mcp_listening(bound),
            )
        } else {
            (None, None, FeatureDecision::mcp_off())
        };

        let runtime = Runtime::start::<A>(booted, app_ctx.host, cancel, &config, mcp);
        Ok(RunningProxima {
            runtime,
            mcp_addr,
            server,
        })
    }

    /// The boot prelude both entry points share, in the order boot has to
    /// happen: resolve configuration, resolve the origin allowlist, then
    /// touch storage.
    ///
    /// That order is the point of having one copy. `resolve_allowlist`
    /// runs before `boot_app`, so a deployment naming an unparseable
    /// origin is refused before a connection is opened. The
    /// `build_refuses_*_before_storage` tests do not reach this: they pin
    /// refusals `resolve` itself raises, which precede storage whatever
    /// this function does. `run_refuses_an_unparseable_origin_before_storage`
    /// is what pins the ordering here, for both entry points at once.
    async fn boot_common(self) -> Result<BootedRuntime, ProximaError> {
        let (config, mut parts) = self.resolve()?;
        let allowlist = if config.mcp.is_some() {
            Some(resolve_allowlist(&config)?)
        } else {
            None
        };
        let booted = boot_app::<A>(&config, &parts).await?;
        let cancel = CancellationToken::new();
        let mut app_ctx = AppContext {
            host: ProximaHost {
                engine: booted.engine.clone(),
                registry: booted.registry.clone(),
                pool: booted.pool.clone(),
                platform_scope: booted.platform_scope.clone(),
                pg_tuning: config.pg_tuning,
                pg_sidecars: booted.pg_sidecars.clone(),
                erase_context: booted.erase_context.clone(),
                origin_scope: booted.origin_scope,
                publication_origin_eligibility: booted.publication_origin_eligibility.clone(),
                blobs: booted.blobs.clone(),
                owner: config.owner,
                services: parts.services.clone(),
            },
        };
        let owner_access = runtime_owner_access(
            &app_ctx,
            parts.owner_access.clone(),
            config.forwarder.clone(),
        );
        if let Some(late) = &parts.late_owner_access {
            late.bind(owner_access.clone());
        }
        if let Some(factory) = parts.platform_authenticator.take() {
            // `resolve` refused a missing platform URL, and boot built the
            // scope from it; absent here is a boot that skipped the census.
            let platform_scope = app_ctx.host.platform_scope.clone().ok_or_else(|| {
                ProximaError::Config(
                    "authenticator_with_platform_scope: the runtime has no platform scope".into(),
                )
            })?;
            parts.authenticator = Some(factory(crate::PlatformAuthContext {
                platform_scope,
                owner_access: owner_access.clone(),
            })?);
        }
        let services = assemble_services::<A>(
            &app_ctx,
            &booted.registry,
            &config.tool_scope,
            parts.authenticator.as_ref(),
            &owner_access,
            &booted.delegation_runtime_authority,
        )?;
        app_ctx.host.services = services;
        Ok(BootedRuntime {
            config,
            parts,
            allowlist,
            booted,
            cancel,
            app_ctx,
            owner_access,
        })
    }

    fn resolve(self) -> Result<(crate::RuntimeConfig, crate::RuntimeParts), ProximaError> {
        let base = A::configure(RuntimeBuilder::default());
        let env_layer = if let Some(injected) = self.injected_env {
            injected
        } else if self.use_env {
            RuntimeBuilder::default().apply_env()?
        } else {
            RuntimeBuilder::default()
        };
        let merged = self.overlay.merge_over(env_layer.merge_over(base));
        merged.resolve()
    }
}

/// What `BuiltProxima` and `RunningProxima` share: the booted engine, its
/// authorities, and the features the runtime started and owns.
struct Runtime {
    host: ProximaHost,
    system_authority: SystemAuthority,
    host_state_maintenance_authority: Option<proxima_core::engine::HostStateMaintenanceAuthority>,
    handle: EngineHandle,
    /// The runtime's own token. Every task it started observes a child of
    /// it; [`Self::shutdown`] cancels it, and so does dropping the runtime
    /// (`_cancel_on_drop`), so no feature outlives its last handle.
    cancel: CancellationToken,
    _cancel_on_drop: tokio_util::sync::DropGuard,
    insecure_single_owner: bool,
    features: Features,
    /// The host-only outbox drain, kept for unit tests that drive a
    /// publisher over a wrapped port. Production code never holds it
    /// outside the publisher task.
    #[cfg(all(test, feature = "outbox-nats"))]
    outbox_for_tests: Arc<dyn proxima_core::storage_ports::publication::PublicationOutboxPort>,
}

impl Runtime {
    /// Start every configured feature over a booted engine. Infallible: it
    /// runs after the last fallible boot step.
    fn start<A: FlavorApp>(
        booted: Booted,
        host: ProximaHost,
        cancel: CancellationToken,
        config: &crate::RuntimeConfig,
        mcp: FeatureDecision,
    ) -> Self {
        let features = crate::features::start::<A>(
            mcp,
            &FeatureInputs {
                engine: &host.engine,
                services: &host.services,
                cancel: &cancel,
                config,
                #[cfg(feature = "outbox-nats")]
                publication: crate::features::PublicationHandles {
                    outbox: booted.outbox.clone(),
                    retention: booted.outbox_retention,
                    eligibility: host.publication_origin_eligibility.clone(),
                    origin_scope: host.origin_scope,
                },
            },
        );
        Self {
            host,
            system_authority: booted.system_authority,
            host_state_maintenance_authority: booted.host_state_maintenance_authority,
            handle: booted.handle,
            _cancel_on_drop: cancel.clone().drop_guard(),
            cancel,
            insecure_single_owner: config.insecure_single_owner,
            features,
            #[cfg(all(test, feature = "outbox-nats"))]
            outbox_for_tests: booted.outbox,
        }
    }

    fn single_owner_authz(&self) -> Option<AuthzContext> {
        self.insecure_single_owner
            .then_some(self.host.owner.as_ref())
            .flatten()
            .map(|owner| insecure_single_owner_authz(owner, AuthPath::HostBearer))
    }

    /// Cancel every started feature, join it, stop the engine.
    async fn shutdown(self) {
        self.cancel.cancel();
        self.features.join().await;
        self.host.engine.stop(self.handle);
    }
}

impl std::fmt::Debug for Runtime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Runtime")
            .field("host", &self.host)
            .field("insecure_single_owner", &self.insecure_single_owner)
            .field("features", &self.features)
            .finish_non_exhaustive()
    }
}

/// The handle methods `BuiltProxima` and `RunningProxima` share, spelled
/// once.
macro_rules! runtime_handle_methods {
    () => {
        /// The booted runtime's host accessors — the same [`ProximaHost`]
        /// [`crate::AppContext::host`] returns.
        #[must_use]
        pub const fn host(&self) -> &ProximaHost {
            &self.runtime.host
        }

        #[must_use]
        pub const fn system_authority(&self) -> &SystemAuthority {
            &self.runtime.system_authority
        }

        /// Boot-held authority for the registered host-state participant.
        /// Absent when the runtime booted without such a participant.
        #[must_use]
        pub const fn host_state_maintenance_authority(
            &self,
        ) -> Option<&proxima_core::engine::HostStateMaintenanceAuthority> {
            self.runtime.host_state_maintenance_authority.as_ref()
        }

        /// The single owner's context in insecure single-owner mode; `None`
        /// otherwise.
        #[must_use]
        pub fn single_owner_authz(&self) -> Option<AuthzContext> {
            self.runtime.single_owner_authz()
        }

        /// Every feature's start decision for this boot, as logged.
        #[must_use]
        pub const fn boot_report(&self) -> &BootReport {
            &self.runtime.features.report
        }

        /// The running outbox publisher's health; `None` when it is off.
        #[cfg(feature = "outbox-nats")]
        #[must_use]
        pub fn publisher_health(&self) -> Option<proxima_outbox_nats::PublisherHealthReader> {
            self.runtime.features.publisher_health()
        }

        /// The running retained-copy cleaner's health; `None` when it is off.
        #[cfg(feature = "outbox-nats")]
        #[must_use]
        pub fn copy_cleaner_health(&self) -> Option<proxima_outbox_nats::CopyCleanerHealthReader> {
            self.runtime.features.copy_cleaner_health()
        }
    };
}

/// A booted app whose features run, without a bound listener.
///
/// Dropping it cancels every started feature; [`Self::shutdown`] also joins
/// them and stops the engine.
pub struct BuiltProxima {
    runtime: Runtime,
    service: Option<Router>,
    /// What [`Self::service`] was layered with; see [`Self::mcp_edge`].
    mcp_edge: Option<crate::McpEdge>,
}

impl BuiltProxima {
    runtime_handle_methods!();

    /// The MCP router — `/mcp`, `/v1` and the health probes as configured —
    /// for the host to serve; `None` without a bind address.
    #[must_use]
    pub const fn service(&self) -> Option<&Router> {
        self.service.as_ref()
    }

    /// The resolved MCP edge [`Self::service`] was built from, `None`
    /// without MCP: a host composing its own router takes the tool host,
    /// allowlists, revalidation and resource metadata from here, or
    /// [`McpEdge::router`](crate::McpEdge::router) for `/mcp` behind bearer
    /// auth beside its own unauthenticated routes.
    #[must_use]
    pub const fn mcp_edge(&self) -> Option<&crate::McpEdge> {
        self.mcp_edge.as_ref()
    }

    /// Stop and join every started feature, then stop the engine.
    pub async fn shutdown(self) {
        self.runtime.shutdown().await;
    }

    /// The host-only outbox drain, for unit tests that wrap it.
    #[cfg(all(test, feature = "outbox-nats"))]
    pub(crate) fn outbox_for_tests(
        &self,
    ) -> Arc<dyn proxima_core::storage_ports::publication::PublicationOutboxPort> {
        self.runtime.outbox_for_tests.clone()
    }
}

impl std::fmt::Debug for BuiltProxima {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BuiltProxima")
            .field("has_service", &self.service.is_some())
            .field("runtime", &self.runtime)
            .finish_non_exhaustive()
    }
}

/// A booted app whose features run, listening when a bind is configured.
///
/// Dropping it cancels the listener and every started feature;
/// [`Self::shutdown`] also joins them and stops the engine.
pub struct RunningProxima {
    runtime: Runtime,
    mcp_addr: Option<SocketAddr>,
    server: Option<JoinHandle<()>>,
}

impl RunningProxima {
    runtime_handle_methods!();

    /// The bound MCP address; `None` without a bind address.
    #[must_use]
    pub const fn mcp_addr(&self) -> Option<SocketAddr> {
        self.mcp_addr
    }

    /// Serve until SIGTERM or SIGINT (Ctrl-C off Unix), then drain as
    /// [`Self::shutdown`] does: readiness turns unavailable, the listener
    /// stops accepting, in-flight requests and streams finish, every feature
    /// joins.
    ///
    /// Installs the process's signal handlers, so it belongs in a binary's
    /// `main`, never in a library.
    ///
    /// # Errors
    ///
    /// [`ProximaError::Mcp`] when the handlers cannot be installed, or when
    /// the listener stopped on its own before any signal — a process whose
    /// server died must not keep running as if healthy. The runtime is shut
    /// down on both paths.
    pub async fn until_shutdown_signal(self) -> Result<(), ProximaError> {
        self.until(shutdown_signal()).await
    }

    /// [`Self::until_shutdown_signal`] with the trigger supplied: shut down
    /// when `signal` resolves.
    ///
    /// # Errors
    ///
    /// As [`Self::until_shutdown_signal`], with `signal`'s own error.
    pub async fn until<F>(mut self, signal: F) -> Result<(), ProximaError>
    where
        F: std::future::Future<Output = Result<(), ProximaError>>,
    {
        let mut listener_stopped = false;
        let outcome = match self.server.as_mut() {
            Some(server) => tokio::select! {
                received = signal => received,
                joined = server => {
                    listener_stopped = true;
                    Err(ProximaError::Mcp(match joined {
                        Ok(()) => "the listener stopped before a shutdown signal".to_owned(),
                        Err(err) => format!("the listener task failed: {err}"),
                    }))
                }
            },
            None => signal.await,
        };
        if listener_stopped {
            self.server = None;
        }
        if outcome.is_ok() {
            tracing::info!("shutdown signal received; draining");
        }
        self.shutdown().await;
        outcome
    }

    /// Stop the listener, stop and join every started feature, then stop
    /// the engine.
    pub async fn shutdown(self) {
        self.runtime.cancel.cancel();
        if let Some(server) = self.server
            && let Err(err) = server.await
        {
            tracing::warn!(error = %err, "proxima facade server join failed");
        }
        self.runtime.shutdown().await;
    }
}

impl std::fmt::Debug for RunningProxima {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunningProxima")
            .field("mcp_addr", &self.mcp_addr)
            .field("has_server", &self.server.is_some())
            .field("runtime", &self.runtime)
            .finish_non_exhaustive()
    }
}

type InsecureAuthz = AuthzContext;

fn insecure_single_owner_authz(owner: &Owner, auth_path: AuthPath) -> InsecureAuthz {
    match *owner {
        OwnerRef::Personal(subject) => AuthzContext::for_subject(subject, auth_path)
            .narrowed_to_owner(*owner)
            .expect("personal owner is self-accessible"),
        OwnerRef::Group(group) => AuthzContext::for_subject_with_role(
            UserId::new(group.into_inner()),
            [(*owner, Role::admin())],
            auth_path,
        )
        .narrowed_to_owner(*owner)
        .expect("group owner role is self-accessible"),
    }
}

/// Run the configured app from process environment.
///
/// # Errors
///
/// Returns config, security, storage, engine, bind, or MCP serving errors.
pub async fn run<A: FlavorApp + 'static>() -> Result<RunningProxima, ProximaError> {
    Proxima::<A>::app().from_env().run().await
}

/// [`run`], then serve until SIGTERM or SIGINT and drain — the whole `main`
/// of a stock host:
///
/// ```no_run
/// // `main` of a host composing `TheFlavor`: `serve::<(TheFlavor,)>()`.
/// async fn main_for<TheFlavor: proxima::FlavorApp + 'static>()
/// -> Result<(), proxima::ProximaError> {
///     proxima::serve::<(TheFlavor,)>().await
/// }
/// ```
///
/// # Errors
///
/// As [`run`] and [`RunningProxima::until_shutdown_signal`].
pub async fn serve<A: FlavorApp + 'static>() -> Result<(), ProximaError> {
    run::<A>().await?.until_shutdown_signal().await
}

/// SIGTERM (what an orchestrator sends) or SIGINT; Ctrl-C off Unix.
async fn shutdown_signal() -> Result<(), ProximaError> {
    let install = |err: std::io::Error| {
        ProximaError::Mcp(format!("installing the shutdown signal handler: {err}"))
    };
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate = signal(SignalKind::terminate()).map_err(install)?;
        let mut interrupt = signal(SignalKind::interrupt()).map_err(install)?;
        tokio::select! {
            _ = terminate.recv() => {}
            _ = interrupt.recv() => {}
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await.map_err(install)
    }
}

/// Compose rmcp and host-mounted routes behind one body, Host, and auth policy.
///
/// `host_allowlist` must also be passed to
/// [`streamable_http_service`](proxima_mcp_server::streamable_http_service) so the
/// listener guard and rmcp's inner `/mcp` guard enforce the same authorities.
pub fn layered_router<S>(
    mcp_service: S,
    app_router: Router,
    edge_auth: Arc<McpEdgeAuth>,
    allowlist: OriginAllowlist,
    host_allowlist: HostAllowlist,
) -> Router
where
    S: Service<Request<Body>, Error = Infallible> + Clone + Send + Sync + 'static,
    S::Response: IntoResponse,
    S::Future: Send + 'static,
{
    layered_router_with_revalidation(
        mcp_service,
        app_router,
        edge_auth,
        allowlist,
        host_allowlist,
        RevalidationConfig::default(),
    )
}

/// Compose rmcp and host-mounted routes with explicit stream revalidation.
///
/// Layer order is body limit, listener-wide Host validation, browser CORS,
/// then bearer auth.
pub fn layered_router_with_revalidation<S>(
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
    Router::new()
        .nest_service(proxima_mcp_server::MCP_PATH, mcp_service)
        .merge(app_router)
        .layer(proxima_mcp_server::mcp_auth_layer_with_config(
            edge_auth,
            revalidation,
        ))
        .layer(cors_layer(allowlist))
        .layer(host_guard_layer(host_allowlist))
        .layer(axum::middleware::from_fn(
            proxima_mcp_server::enforce_body_limit,
        ))
}

/// Erase the concrete store once, then expose its disjoint capabilities over
/// one shared allocation. MCP, REST, and workers receive clones of the same
/// immutable [`FlavorServices`] set; none can recover the concrete backend.
fn cited_blob_services(
    blobs: Option<&CitedBlobStore>,
    runtime_authority: &DelegationRuntimeAuthority,
) -> Option<(
    CitedBlobService,
    CitedBlobReadService,
    CitedBlobOwnerReconcileService,
)> {
    blobs.map(|store| {
        let store = Arc::new(store.clone());
        (
            CitedBlobService::new_runtime(store.clone(), runtime_authority),
            CitedBlobReadService::new_runtime(store.clone(), runtime_authority),
            CitedBlobOwnerReconcileService::new(store),
        )
    })
}

/// Everything [`Proxima::boot_common`] produced, for the two tails that
/// diverge after it: `build` hands the router to the host, `run` binds a
/// listener and spawns the server. Both then start the same features.
///
/// A named struct rather than a tuple because the tail reads seven fields
/// of four visually similar types, and a 7-tuple is both unreadable at the
/// destructuring site and `clippy::type_complexity` on the signature.
struct BootedRuntime {
    config: crate::RuntimeConfig,
    parts: crate::RuntimeParts,
    /// `Some` exactly when MCP is configured — the two are resolved
    /// together so a tail can never serve MCP without an allowlist.
    allowlist: Option<OriginAllowlist>,
    booted: Booted,
    cancel: CancellationToken,
    /// Its host carries the composed service set.
    app_ctx: AppContext,
    /// The one port the edge, the delegation service, and an environment
    /// OIDC authenticator resolve roles through.
    owner_access: Arc<dyn OwnerAccessPort>,
}

impl std::fmt::Debug for BootedRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BootedRuntime")
            .field("config", &self.config)
            .field("parts", &self.parts)
            .field("booted", &self.booted)
            .field("app_ctx", &self.app_ctx)
            .finish_non_exhaustive()
    }
}

/// The host's owner-access port, else the Postgres resolver over the
/// runtime pool, under the forwarder policy when one is configured.
fn runtime_owner_access(
    app_ctx: &AppContext,
    host: Option<Arc<dyn OwnerAccessPort>>,
    forwarder: Option<ForwarderPolicy>,
) -> Arc<dyn OwnerAccessPort> {
    let port = host.unwrap_or_else(|| {
        let host = app_ctx.host();
        Arc::new(match &host.platform_scope {
            Some(scope) => {
                PgOwnerAccessResolver::new(host.pool.clone()).with_platform_scope(scope.clone())
            }
            None => PgOwnerAccessResolver::new(host.pool.clone()),
        })
    });
    match forwarder {
        Some(policy) => policy.wrap(port),
        None => port,
    }
}

/// Flavor services plus the substrate-owned services every composed
/// binary gets for free. When S3 is configured, the store is published as
/// [`CitedBlobService`] for presigned upload/read, [`CitedBlobReadService`]
/// for bounded verified bytes, and the separately authorized
/// [`CitedBlobOwnerReconcileService`] for redacted owner reports. Tools,
/// REST, and flavor workers receive the same immutable service set.
///
/// The delegation service is published only with an authenticator, and
/// redeems through `owner_access`.
fn assemble_services<A: FlavorApp>(
    app_ctx: &AppContext,
    registry: &Arc<FlavorRegistryFrozen>,
    deployment_tool_scope: &ToolScope,
    authenticator: Option<&Arc<dyn Authenticator>>,
    owner_access: &Arc<dyn OwnerAccessPort>,
    runtime_authority: &DelegationRuntimeAuthority,
) -> Result<FlavorServices, ProximaError> {
    let host = app_ctx.host();
    let mut services = host.services.clone();
    services.try_extend(A::services(app_ctx)?)?;
    debug_assert!(
        services
            .get::<proxima_core::engine::HostStateMaintenanceAuthority>()
            .is_none(),
        "host-state maintenance authority must remain outside FlavorServices"
    );
    if let Some((transfer, verified_read, owner_reconcile)) =
        cited_blob_services(host.blobs.as_ref(), runtime_authority)
    {
        services.try_insert(transfer)?;
        services.try_insert(verified_read)?;
        services.try_insert(owner_reconcile)?;
    }
    if services.get::<proxima_core::RequestHeaders>().is_some() {
        return Err(ProximaError::Config(
            "a flavor published RequestHeaders as a boot service; it is request-scoped and \
             only the header allowlist may produce it"
                .into(),
        ));
    }
    if let Some(authenticator) = authenticator {
        let store = Arc::new(match &host.platform_scope {
            Some(scope) => {
                PgDelegationStore::new(host.pool.clone()).with_platform_scope(scope.clone())
            }
            None => PgDelegationStore::new(host.pool.clone()),
        });
        services.try_insert(DelegatedAuthorityService::new(
            store,
            owner_access.clone(),
            authenticator.clone(),
            registry.clone(),
            deployment_tool_scope.clone(),
            runtime_authority,
        ))?;
    }
    Ok(services)
}

async fn boot_app<A: FlavorApp + 'static>(
    config: &crate::RuntimeConfig,
    parts: &crate::RuntimeParts,
) -> Result<Booted, ProximaError> {
    // Boot holds migration and role-census state. Keep that one-time future
    // out of every caller's runtime construction state machine.
    Box::pin(crate::boot::boot::<A>(config, parts)).await
}

/// Resolve the MCP edge both entry points serve: bearer auth over the
/// runtime's owner-access port, the tool host (engine, request headers,
/// host tools, call recording), the Host allowlist and the REST routes.
fn resolve_mcp_edge(
    app_ctx: &AppContext,
    parts: &crate::RuntimeParts,
    owner_access: Arc<dyn OwnerAccessPort>,
    allowlist: OriginAllowlist,
    cancel: &CancellationToken,
    config: &crate::RuntimeConfig,
) -> crate::McpEdge {
    let engine = app_ctx.host().engine.clone();
    let services = app_ctx.host().services.clone();
    let mut edge_auth = McpEdgeAuth::headless().with_tool_scope(config.tool_scope.clone());
    if let Some(authenticator) = parts.authenticator.clone() {
        // The runtime's one owner-access port, so a Group owner the eager
        // role map does not carry can still be resolved one owner per
        // request. A host serving many parties through one forwarder subject
        // cannot enumerate them eagerly; without this the edge would refuse
        // every such owner.
        edge_auth = edge_auth
            .with_host(authenticator)
            .with_owner_access(owner_access);
    }
    let mut tool_host = McpToolHost::from_parts(Arc::new(engine.registry().clone()), services)
        .with_engine(engine)
        .with_request_headers(config.request_headers.clone())
        .with_call_recording(config.record_mcp_calls);
    if let Some(host_tools) = parts.host_tools.clone() {
        tool_host = tool_host.with_host_tools(host_tools);
    }
    let rest_router = rest_router(&tool_host, config);
    crate::McpEdge {
        tool_host,
        edge_auth: Arc::new(edge_auth),
        origin_allowlist: allowlist,
        host_allowlist: resolve_host_allowlist(config),
        revalidation: config.stream_revalidation,
        resource_metadata: config.resource_metadata.clone(),
        transport: config.mcp_transport,
        rest_router,
        cancel: cancel.clone(),
    }
}

fn build_router<A: FlavorApp>(
    app_ctx: AppContext,
    edge: &crate::McpEdge,
    cancel: &CancellationToken,
    config: &crate::RuntimeConfig,
) -> Router {
    let health_router = if config.health_endpoints {
        crate::health::router(app_ctx.host().pool.clone(), cancel.clone())
    } else {
        Router::new()
    };
    let app_router = A::mount_http(Router::new(), app_ctx);
    let auth_layer = proxima_mcp_server::mcp_auth_layer_with_metadata(
        Arc::clone(&edge.edge_auth),
        edge.revalidation,
        edge.resource_metadata.as_ref(),
    );
    let protected_router = Router::new()
        .nest_service(proxima_mcp_server::MCP_PATH, edge.mcp_service())
        .merge(edge.rest_router.clone())
        .merge(app_router)
        .layer(auth_layer);
    let mut router = protected_router;
    if let Some(md) = &edge.resource_metadata {
        router = router.merge(proxima_mcp_server::protected_resource_router(md));
    }
    // Apply listener-wide layers only after anonymous OAuth metadata has been
    // merged. Body-size rejection remains outermost; Host validation then runs
    // before CORS and bearer auth. CORS covers public metadata and preflights;
    // bearer auth remains inside it on protected routes. Health probes sit
    // outside the Host guard: an orchestrator probes the pod address, not a
    // public host, and the probes disclose nothing a rebinding page could use.
    // Everything else, the fallback included, falls through to the guarded
    // router; a `merge` here would replace its guarded fallback with an
    // unguarded one.
    let guarded = router
        .layer(cors_layer(edge.origin_allowlist.clone()))
        .layer(host_guard_layer(edge.host_allowlist.clone()));
    health_router
        .fallback_service(guarded)
        .layer(body_limit_layer(
            config.mcp_transport.max_request_body_bytes,
        ))
}

/// The `/v1` REST surface, merged inside the shared auth, Host, and body-limit
/// layers. Its routes already carry the `/v1` prefix, so this is a `merge`, not
/// a `nest`: nesting would rewrite the inner request URI and strip the prefix
/// off every problem document's `instance`.
#[cfg(feature = "rest")]
fn rest_router(host: &McpToolHost, config: &crate::RuntimeConfig) -> Router {
    if !config.rest_enabled {
        return Router::new();
    }
    proxima_mcp_server::rest::router(
        host.clone(),
        config
            .resource_metadata
            .as_ref()
            .map(|md| md.public_url.clone()),
    )
}

/// Feature-off shape. `PROXIMA_REST_ENABLED` in a binary built without the
/// `rest` feature is an operator asking for a surface that is not in the
/// build; say so once at boot rather than serving 404s that look like a
/// routing bug.
#[cfg(not(feature = "rest"))]
fn rest_router(host: &McpToolHost, config: &crate::RuntimeConfig) -> Router {
    let _ = host;
    if config.rest_enabled {
        tracing::warn!(
            "PROXIMA_REST_ENABLED is set but this binary was built without the `rest` \
             cargo feature; /v1 is not served"
        );
    }
    Router::new()
}

fn resolve_allowlist(config: &crate::RuntimeConfig) -> Result<OriginAllowlist, ProximaError> {
    if config.allowed_origins.is_empty() {
        return Ok(default_allowlist());
    }
    OriginAllowlist::parse(&config.allowed_origins)
        .map_err(|err| ProximaError::Security(err.to_string()))
}

/// Inbound `Host` allowlist shared by the whole listener and rmcp.
///
/// [`HostAllowlist`] always adds loopback (gateway rewrites and port-forwards
/// keep working). Configured or derived public hosts are honored independently
/// of bind address so a loopback listener behind a reverse proxy can preserve
/// its public `Host`. Network exposure separately requires that public set to
/// be non-empty in `RuntimeConfig::validate`.
fn resolve_host_allowlist(config: &crate::RuntimeConfig) -> HostAllowlist {
    HostAllowlist::new(config.public_allowed_hosts())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use proxima_core::{
        AuthError, AuthPath, Authenticator, AuthzContext, Credentials, FlavorRegistry,
        FlavorRegistryError, Owner,
    };
    use proxima_storage_pg::PgPoolConfig;
    use uuid::Uuid;

    use super::*;
    use crate::bundle::FlavorBundle;
    use crate::{AppInfo, RuntimeBuilder, company_owner};

    mod alpha {
        proxima_core::proxima_flavor! {
            name = "proxima-runtime-alpha",
            fact_schemas = [],
            abstraction_schemas = [],
            perspective_schemas = [],
            goal_schemas = [],
            mcp_tools = [],
        }
    }

    struct AlphaApp;

    impl FlavorBundle for AlphaApp {
        fn register(registry: &mut FlavorRegistry) -> Result<(), FlavorRegistryError> {
            alpha::register(registry)
        }

        fn migrators() -> Vec<crate::NamedMigrator> {
            Vec::new()
        }
    }

    impl FlavorApp for AlphaApp {
        fn app_info() -> AppInfo {
            AppInfo {
                id: "alpha",
                title: "Alpha",
                version: "1",
            }
        }

        fn configure(builder: RuntimeBuilder) -> RuntimeBuilder {
            builder
                .database_url("postgres://alpha/proxima")
                .pg_pool_config(PgPoolConfig {
                    max_connections: 3,
                    ..PgPoolConfig::default()
                })
        }
    }

    #[tokio::test]
    async fn extension_assembly_publishes_owner_blob_reconcile_separately() {
        let pool = sqlx::PgPool::connect_lazy_with(sqlx::postgres::PgConnectOptions::new());
        let store = CitedBlobStore::new(
            pool,
            S3RuntimeConfig {
                bucket: "test-bucket".to_string(),
                region: "eu-central-1".to_string(),
                endpoint_url: None,
                force_path_style: false,
                upload_ttl_seconds: 900,
                read_ttl_seconds: 300,
                max_blob_bytes: None,
            },
        )
        .expect("test store config");
        let (engine, _system, delegation_runtime) =
            proxima_core::Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests())
                .into_runtime_authorities();
        let registry = Arc::new(engine.registry().clone());
        let mut host = ProximaHost::for_tests(Arc::new(engine));
        host.blobs = Some(store);
        let app_ctx = AppContext { host };

        let services = assemble_services::<AlphaApp>(
            &app_ctx,
            &registry,
            &ToolScope::All,
            None,
            &runtime_owner_access(&app_ctx, None, None),
            &delegation_runtime,
        )
        .expect("service assembly");

        assert!(services.get::<CitedBlobService>().is_some());
        assert!(
            services.get::<CitedBlobReadService>().is_some(),
            "S3 assembly must publish the bounded verified-read lane"
        );
        assert!(
            services.get::<CitedBlobOwnerReconcileService>().is_some(),
            "S3 assembly must publish the redacted owner reconcile lane"
        );
        assert!(
            services.get::<SystemAuthority>().is_none(),
            "global operator authority must never enter the flavor service set"
        );
        assert!(
            services
                .get::<proxima_core::engine::HostStateMaintenanceAuthority>()
                .is_none(),
            "host-state maintenance authority must remain outside FlavorServices"
        );
        assert!(
            services.get::<DelegatedAuthorityService>().is_none(),
            "delegation service requires a real authenticator"
        );
    }

    #[tokio::test]
    async fn delegation_service_is_authenticator_gated_and_shared_by_identity() {
        let (engine, _system, delegation_runtime) =
            proxima_core::Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests())
                .into_runtime_authorities();
        let engine = Arc::new(engine);
        let registry = Arc::new(engine.registry().clone());
        let app_ctx = AppContext {
            host: ProximaHost::for_tests(engine.clone()),
        };
        let authenticator: Arc<dyn Authenticator> = Arc::new(StubAuth { owner: owner() });
        let services = assemble_services::<AlphaApp>(
            &app_ctx,
            &registry,
            &ToolScope::All,
            Some(&authenticator),
            &runtime_owner_access(&app_ctx, None, None),
            &delegation_runtime,
        )
        .expect("service assembly");
        let service = services
            .get::<DelegatedAuthorityService>()
            .expect("authenticated runtime publishes delegation service");
        let cloned = services.clone();
        let cloned_service = cloned
            .get::<DelegatedAuthorityService>()
            .expect("cloned services retain delegation service");
        assert!(Arc::ptr_eq(&service, &cloned_service));

        let worker =
            crate::workers::FlavorWorkerContext::new_for_tests(engine, CancellationToken::new())
                .with_services(cloned);
        let worker_service = worker
            .service::<DelegatedAuthorityService>()
            .expect("worker sees composed delegation service");
        assert!(Arc::ptr_eq(&service, &worker_service));
        assert!(services.get::<SystemAuthority>().is_none());
        assert!(services.get::<DelegationRuntimeAuthority>().is_none());
    }

    struct StubAuth {
        owner: Owner,
    }

    #[async_trait]
    impl Authenticator for StubAuth {
        async fn authenticate(&self, _creds: &Credentials) -> Result<AuthzContext, AuthError> {
            Ok(insecure_single_owner_authz(
                &self.owner,
                AuthPath::HostBearer,
            ))
        }
    }

    fn owner() -> Owner {
        company_owner(Uuid::now_v7())
    }

    // --- Host-allowlist policy invariants (DNS-rebinding guard) ---
    //
    // rmcp's raw empty-list state is allow-all. The shared HostAllowlist type
    // makes that state unconstructible by adding loopback before either guard
    // sees the policy.

    #[test]
    fn resolve_host_allowlist_is_exactly_loopback_for_loopback_bind() {
        let owner = owner();
        let (config, _) = RuntimeBuilder::default()
            .database_url("postgres://unused/db")
            .owner(owner)
            .tool_scope(ToolScope::All)
            .mcp_bind("127.0.0.1:31415".parse().unwrap())
            .authenticator(Arc::new(StubAuth { owner }))
            .resolve()
            .unwrap();

        assert!(!config.expose_network);
        assert_eq!(
            resolve_host_allowlist(&config).hosts(),
            ["localhost", "127.0.0.1", "::1"]
        );
    }

    #[test]
    fn resolve_host_allowlist_honors_explicit_host_on_loopback_bind() {
        let owner = owner();
        let (config, _) = RuntimeBuilder::default()
            .database_url("postgres://unused/db")
            .owner(owner)
            .tool_scope(ToolScope::All)
            .mcp_bind("127.0.0.1:31415".parse().unwrap())
            .allowed_hosts(vec!["proxy.example.com:8443".to_string()])
            .authenticator(Arc::new(StubAuth { owner }))
            .resolve()
            .unwrap();

        assert!(!config.expose_network);
        assert!(
            resolve_host_allowlist(&config)
                .hosts()
                .iter()
                .any(|host| host == "proxy.example.com:8443")
        );
    }

    #[test]
    fn resolve_host_allowlist_derives_public_hosts_on_loopback_bind() {
        let owner = owner();
        let (config, _) = RuntimeBuilder::default()
            .database_url("postgres://unused/db")
            .owner(owner)
            .tool_scope(ToolScope::All)
            .mcp_bind("127.0.0.1:31415".parse().unwrap())
            .allowed_origins(vec!["https://app.example.com".to_string()])
            .resource_metadata(proxima_mcp_server::ResourceServerMetadata {
                public_url: "https://proxy.example.com".to_string(),
                authorization_servers: vec!["https://idp.test".to_string()],
            })
            .authenticator(Arc::new(StubAuth { owner }))
            .resolve()
            .unwrap();

        assert!(!config.expose_network);
        let allowlist = resolve_host_allowlist(&config);
        assert!(
            allowlist
                .hosts()
                .iter()
                .any(|host| host == "proxy.example.com")
        );
        assert!(
            allowlist
                .hosts()
                .iter()
                .any(|host| host == "app.example.com")
        );
    }

    #[test]
    fn resolve_host_allowlist_exposed_is_loopback_plus_public_and_never_empty() {
        let owner = owner();
        let (config, _) = RuntimeBuilder::default()
            .database_url("postgres://unused/db")
            .owner(owner)
            .tool_scope(ToolScope::All)
            .mcp_bind("0.0.0.0:8080".parse().unwrap())
            .expose_network(true)
            .allowed_origins(vec!["https://app.example.com".to_string()])
            .authenticator(Arc::new(StubAuth { owner }))
            .resolve()
            .unwrap();

        let allowlist = resolve_host_allowlist(&config);
        let hosts = allowlist.hosts();
        // Loopback stays (gateway Host-rewrite + port-forward keep working)…
        assert!(hosts.iter().any(|host| host == "localhost"));
        assert!(hosts.iter().any(|host| host == "127.0.0.1"));
        assert!(hosts.iter().any(|host| host == "::1"));
        // …and the public host is present, but the list is NEVER empty —
        // so an exposed bind always hands rmcp a non-empty allowlist and
        // can never trip its allow-all state.
        assert!(hosts.iter().any(|host| host == "app.example.com"));
        assert!(!hosts.is_empty());
    }

    #[test]
    fn resolve_host_allowlist_includes_public_url_host_distinct_from_origins() {
        // Split deployment: browser app origin differs from the MCP host.
        // Deriving from origins alone would miss the real Host; the
        // public_url host must be in the allowlist.
        let owner = owner();
        let (config, _) = RuntimeBuilder::default()
            .database_url("postgres://unused/db")
            .owner(owner)
            .tool_scope(ToolScope::All)
            .mcp_bind("0.0.0.0:8080".parse().unwrap())
            .expose_network(true)
            .allowed_origins(vec!["https://app.example.com".to_string()])
            .resource_metadata(proxima_mcp_server::ResourceServerMetadata {
                public_url: "https://proxima.example.com".to_string(),
                authorization_servers: vec!["https://idp.test".to_string()],
            })
            .authenticator(Arc::new(StubAuth { owner }))
            .resolve()
            .unwrap();

        let allowlist = resolve_host_allowlist(&config);
        let hosts = allowlist.hosts();
        assert!(hosts.iter().any(|host| host == "proxima.example.com"));
        assert!(hosts.iter().any(|host| host == "app.example.com"));
    }

    #[test]
    fn injected_lookup_replaces_the_ambient_process_env_source() {
        let (config, _) = Proxima::<AlphaApp>::app()
            .from_env()
            .from_lookup(|key| (key == "PROXIMA_PG_MAX_CONNECTIONS").then(|| "4".to_string()))
            .expect("injected pool lookup")
            .tool_scope(ToolScope::All)
            .resolve()
            .expect("resolved runtime config");

        assert_eq!(config.pg_pool_config.max_connections, 4);
    }

    #[tokio::test]
    async fn build_refuses_mcp_without_authenticator_or_insecure_mode_before_storage() {
        let err = Proxima::<AlphaApp>::app()
            .database_url("postgres://unused:5432/unused")
            .owner(owner())
            .tool_scope(ToolScope::All)
            .mcp_bind("127.0.0.1:31415".parse().unwrap())
            .build()
            .await
            .unwrap_err();

        assert!(matches!(err, ProximaError::Security(_)));
    }

    #[tokio::test]
    async fn build_refuses_insecure_exposed_network_before_storage() {
        let err = Proxima::<AlphaApp>::app()
            .database_url("postgres://unused:5432/unused")
            .owner(owner())
            .tool_scope(ToolScope::All)
            .mcp_bind("127.0.0.1:31415".parse().unwrap())
            .allow_insecure_single_owner()
            .expose_network(true)
            .allowed_origins(vec!["https://app.test".to_string()])
            .build()
            .await
            .unwrap_err();

        assert!(matches!(err, ProximaError::Security(_)));
    }

    #[tokio::test]
    async fn build_refuses_non_loopback_insecure_mcp_before_storage() {
        let err = Proxima::<AlphaApp>::app()
            .database_url("postgres://unused:5432/unused")
            .owner(owner())
            .tool_scope(ToolScope::All)
            .mcp_bind("0.0.0.0:31415".parse().unwrap())
            .allow_insecure_single_owner()
            .build()
            .await
            .unwrap_err();

        assert!(matches!(err, ProximaError::Security(_)));
    }

    #[tokio::test]
    async fn build_refuses_exposed_network_without_origins_before_storage() {
        let owner = owner();
        let err = Proxima::<AlphaApp>::app()
            .database_url("postgres://unused:5432/unused")
            .owner(owner)
            .tool_scope(ToolScope::All)
            .mcp_bind("127.0.0.1:31415".parse().unwrap())
            .expose_network(true)
            .authenticator(Arc::new(StubAuth { owner }))
            .build()
            .await
            .unwrap_err();

        assert!(matches!(err, ProximaError::Security(_)));
    }

    /// Every refusal above enters through `build`. The same fail-closed
    /// ordering — resolve the origin allowlist, and only then open a
    /// connection — has to hold for `run`, which is the entry point a
    /// deployment actually uses.
    ///
    /// Both now share `boot_common`, so the ordering is one statement
    /// rather than two copies that have to stay identical. This pins the
    /// claim from the other side: the database URL points nowhere, so a
    /// storage error here would mean the allowlist was resolved too late.
    #[tokio::test]
    async fn run_refuses_an_unparseable_origin_before_storage() {
        let owner = owner();
        let err = Proxima::<AlphaApp>::app()
            .database_url("postgres://unused:5432/unused")
            .owner(owner)
            .tool_scope(ToolScope::All)
            .mcp_bind("127.0.0.1:31415".parse().unwrap())
            .allowed_origins(vec!["*".to_string()])
            .authenticator(Arc::new(StubAuth { owner }))
            .run()
            .await
            .unwrap_err();

        assert!(
            matches!(err, ProximaError::Security(_)),
            "a wildcard origin must be refused before storage is touched, got {err:?}"
        );
    }

    #[tokio::test]
    async fn build_refuses_missing_tool_scope_with_config_error() {
        // Fail-closed default: an embedding host that never calls
        // `.tool_scope(...)` gets a config error instead of silently
        // advertising `ToolScope::All`.
        let err = Proxima::<AlphaApp>::app()
            .database_url("postgres://unused:5432/unused")
            .owner(owner())
            .build()
            .await
            .unwrap_err();

        assert!(matches!(err, ProximaError::Config(_)));
        assert!(err.to_string().contains("tool_scope is required"));
    }
}

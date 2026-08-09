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
use proxima_core::mcp::McpToolExtensions;
use proxima_core::storage_ports::CitedBlobService;
use proxima_core::{
    AnthropicClient, AuthPath, Authenticator, AuthzContext, EmbeddingClient, FlavorRegistryFrozen,
    RevalidationConfig, ToolScope,
};
use proxima_core::{Engine, EngineHandle, Owner, OwnerRef, Role, UserId};
use proxima_mcp_server::{
    HostAllowlist, McpEdgeAuth, McpToolHost, OriginAllowlist, assert_loopback, cors_layer,
    default_allowlist, host_guard_layer, streamable_http_service,
};
use sqlx::PgPool;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tower::Service;

use crate::workers::{FlavorWorker, FlavorWorkerContext};
use crate::{
    AppContext, CoreMcpTools, EmbedConfig, FlavorApp, ProximaBuilder, ProximaError, RuntimeBuilder,
};
use proxima_storage_pg::PgSidecarRegistryFrozen;

const EMBEDDING_WORKER_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

/// Application runtime facade.
pub struct Proxima<A: FlavorApp> {
    overlay: RuntimeBuilder,
    use_env: bool,
    _app: PhantomData<A>,
}

impl<A: FlavorApp> std::fmt::Debug for Proxima<A> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Proxima")
            .field("overlay", &self.overlay)
            .field("use_env", &self.use_env)
            .finish()
    }
}

impl<A: FlavorApp + 'static> Proxima<A> {
    #[must_use]
    pub fn app() -> Self {
        Self {
            overlay: RuntimeBuilder::default(),
            use_env: false,
            _app: PhantomData,
        }
    }

    #[must_use]
    pub fn from_env(mut self) -> Self {
        self.use_env = true;
        self
    }

    #[must_use]
    pub fn database_url(mut self, database_url: impl Into<String>) -> Self {
        self.overlay = self.overlay.database_url(database_url);
        self
    }

    #[must_use]
    pub fn s3(mut self, s3: S3RuntimeConfig) -> Self {
        self.overlay = self.overlay.s3(s3);
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

    #[must_use]
    pub fn resource_metadata(
        mut self,
        metadata: proxima_mcp_server::ResourceServerMetadata,
    ) -> Self {
        self.overlay = self.overlay.resource_metadata(metadata);
        self
    }

    #[must_use]
    pub fn with_mcp(mut self) -> Self {
        self.overlay = self.overlay.with_mcp();
        self
    }

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

    #[must_use]
    pub fn embed_client(mut self, client: Arc<dyn EmbeddingClient>) -> Self {
        self.overlay = self.overlay.embed_client(client);
        self
    }

    #[must_use]
    pub fn anthropic(mut self, client: Arc<dyn AnthropicClient>) -> Self {
        self.overlay = self.overlay.anthropic(client);
        self
    }

    /// Resolve, validate, boot, and return an in-process service.
    ///
    /// # Errors
    ///
    /// Returns config, security, storage, engine, or MCP assembly errors.
    pub async fn build(self) -> Result<BuiltProxima, ProximaError> {
        let (config, parts) = self.resolve()?;
        let allowlist = if config.mcp.is_some() {
            Some(resolve_allowlist(&config)?)
        } else {
            None
        };
        let booted = boot_app::<A>(&config, &parts).await?;
        let cancel = CancellationToken::new();
        let app_ctx = AppContext {
            engine: booted.engine.clone(),
            pool: booted.pool.clone(),
            blobs: booted.blobs.clone(),
            owner: booted.owner,
        };
        let tool_extensions = assemble_tool_extensions::<A>(&app_ctx);

        let service = if let Some(allowlist) = allowlist {
            Some(build_router::<A>(
                app_ctx.clone(),
                tool_extensions.clone(),
                parts.authenticator,
                allowlist,
                &cancel,
                &config,
            ))
        } else {
            None
        };

        Ok(BuiltProxima {
            service,
            engine: booted.engine,
            system_authority: booted.system_authority,
            handle: booted.handle,
            pool: booted.pool,
            registry: booted.registry,
            pg_sidecars: booted.pg_sidecars,
            blobs: booted.blobs,
            owner: booted.owner,
            cancel,
            insecure_single_owner: config.insecure_single_owner,
            tool_extensions,
        })
    }

    /// Resolve, validate, boot, and serve the app/MCP facade when enabled.
    ///
    /// # Errors
    ///
    /// Returns config, security, storage, engine, bind, or MCP serving errors.
    pub async fn run(self) -> Result<RunningProxima, ProximaError> {
        let (config, parts) = self.resolve()?;
        let allowlist = if config.mcp.is_some() {
            Some(resolve_allowlist(&config)?)
        } else {
            None
        };
        let booted = boot_app::<A>(&config, &parts).await?;
        let cancel = CancellationToken::new();
        let app_ctx = AppContext {
            engine: booted.engine.clone(),
            pool: booted.pool.clone(),
            blobs: booted.blobs.clone(),
            owner: booted.owner,
        };
        let tool_extensions = assemble_tool_extensions::<A>(&app_ctx);

        let (mcp_addr, server) = if let (Some(mcp), Some(allowlist)) = (config.mcp, allowlist) {
            if !config.expose_network {
                assert_loopback(&mcp.bind)
                    .map_err(|err| ProximaError::Security(err.to_string()))?;
            }
            let app = build_router::<A>(
                app_ctx,
                tool_extensions.clone(),
                parts.authenticator,
                allowlist,
                &cancel,
                &config,
            );
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
            (Some(bound), Some(server))
        } else {
            (None, None)
        };

        // Workers are spawned only after the last fallible step: an early
        // `?` return would drop their join handles and the uncancelled
        // token, leaving the tasks running detached. The child token lets
        // workers observe the runtime's shutdown without being able to
        // trigger it.
        let worker_ctx = FlavorWorkerContext {
            engine: booted.engine.clone(),
            pool: booted.pool.clone(),
            cancel: cancel.child_token(),
            blobs: cited_blob_service(booted.blobs.as_ref()),
        };
        let workers = A::spawn_workers(&worker_ctx);

        Ok(RunningProxima {
            engine: booted.engine,
            system_authority: booted.system_authority,
            handle: booted.handle,
            pool: booted.pool,
            registry: booted.registry,
            pg_sidecars: booted.pg_sidecars,
            blobs: booted.blobs,
            owner: booted.owner,
            mcp_addr,
            server,
            cancel,
            insecure_single_owner: config.insecure_single_owner,
            tool_extensions,
            workers,
        })
    }

    fn resolve(self) -> Result<(crate::RuntimeConfig, crate::RuntimeParts), ProximaError> {
        let base = A::configure(RuntimeBuilder::default());
        let env_layer = if self.use_env {
            RuntimeBuilder::default().apply_env()?
        } else {
            RuntimeBuilder::default()
        };
        let merged = self.overlay.merge_over(env_layer.merge_over(base));
        merged.resolve()
    }
}

/// Booted app without a bound listener.
pub struct BuiltProxima {
    pub service: Option<Router>,
    pub engine: Arc<Engine>,
    pub system_authority: SystemAuthority,
    pub handle: EngineHandle,
    pool: PgPool,
    pub registry: Arc<FlavorRegistryFrozen>,
    pub pg_sidecars: Arc<PgSidecarRegistryFrozen>,
    pub blobs: Option<CitedBlobStore>,
    pub owner: Option<Owner>,
    pub cancel: CancellationToken,
    pub insecure_single_owner: bool,
    tool_extensions: McpToolExtensions,
}

impl BuiltProxima {
    pub fn shutdown(self) {
        self.cancel.cancel();
        self.engine.stop(self.handle);
    }

    #[must_use]
    pub fn spawn_embedding_worker(&self, cancel: CancellationToken) -> JoinHandle<()> {
        spawn_embedding_worker(self.engine.clone(), cancel)
    }

    #[must_use]
    pub fn single_owner_authz(&self) -> Option<AuthzContext> {
        self.insecure_single_owner
            .then_some(self.owner.as_ref())
            .flatten()
            .map(|owner| insecure_single_owner_authz(owner, AuthPath::HostBearer))
    }

    #[must_use]
    pub const fn system_authority(&self) -> &SystemAuthority {
        &self.system_authority
    }

    #[must_use]
    pub fn core_mcp_tools(&self) -> CoreMcpTools {
        CoreMcpTools::new(
            self.registry.clone(),
            self.engine.clone(),
            self.tool_extensions.clone(),
        )
    }

    #[must_use]
    pub fn engine(&self) -> Arc<Engine> {
        self.engine.clone()
    }

    #[must_use]
    pub fn registry(&self) -> &FlavorRegistryFrozen {
        &self.registry
    }

    #[cfg(any(test, feature = "testkit", debug_assertions))]
    #[must_use]
    pub fn pool_for_tests(&self) -> &PgPool {
        &self.pool
    }
}

impl std::fmt::Debug for BuiltProxima {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BuiltProxima")
            .field("has_service", &self.service.is_some())
            .field("pool", &self.pool)
            .field("blobs", &self.blobs)
            .field("owner", &self.owner)
            .field("insecure_single_owner", &self.insecure_single_owner)
            .finish_non_exhaustive()
    }
}

/// Running app with an optional facade listener.
pub struct RunningProxima {
    pub engine: Arc<Engine>,
    pub system_authority: SystemAuthority,
    pub handle: EngineHandle,
    pool: PgPool,
    pub registry: Arc<FlavorRegistryFrozen>,
    pub pg_sidecars: Arc<PgSidecarRegistryFrozen>,
    pub blobs: Option<CitedBlobStore>,
    pub owner: Option<Owner>,
    pub mcp_addr: Option<SocketAddr>,
    pub server: Option<JoinHandle<()>>,
    pub cancel: CancellationToken,
    pub insecure_single_owner: bool,
    tool_extensions: McpToolExtensions,
    /// Flavor-contributed background workers spawned by [`Proxima::run`]
    /// via `FlavorBundle::spawn_workers`; joined by [`Self::shutdown`].
    workers: Vec<FlavorWorker>,
}

impl RunningProxima {
    pub async fn shutdown(self) {
        self.cancel.cancel();
        if let Some(server) = self.server
            && let Err(err) = server.await
        {
            tracing::warn!(error = %err, "proxima facade server join failed");
        }
        for worker in self.workers {
            if let Err(err) = worker.handle.await {
                tracing::warn!(worker = worker.name, error = %err, "flavor worker join failed");
            }
        }
        self.engine.stop(self.handle);
    }

    #[must_use]
    pub fn spawn_embedding_worker(&self, cancel: CancellationToken) -> JoinHandle<()> {
        spawn_embedding_worker(self.engine.clone(), cancel)
    }

    #[must_use]
    pub fn single_owner_authz(&self) -> Option<AuthzContext> {
        self.insecure_single_owner
            .then_some(self.owner.as_ref())
            .flatten()
            .map(|owner| insecure_single_owner_authz(owner, AuthPath::HostBearer))
    }

    #[must_use]
    pub const fn system_authority(&self) -> &SystemAuthority {
        &self.system_authority
    }

    #[must_use]
    pub fn core_mcp_tools(&self) -> CoreMcpTools {
        CoreMcpTools::new(
            self.registry.clone(),
            self.engine.clone(),
            self.tool_extensions.clone(),
        )
    }

    #[must_use]
    pub fn engine(&self) -> Arc<Engine> {
        self.engine.clone()
    }

    #[must_use]
    pub fn registry(&self) -> &FlavorRegistryFrozen {
        &self.registry
    }

    #[cfg(any(test, feature = "testkit", debug_assertions))]
    #[must_use]
    pub fn pool_for_tests(&self) -> &PgPool {
        &self.pool
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
        OwnerRef::World => AuthzContext::denied_for_owner(owner),
    }
}

impl std::fmt::Debug for RunningProxima {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunningProxima")
            .field("pool", &self.pool)
            .field("blobs", &self.blobs)
            .field("owner", &self.owner)
            .field("mcp_addr", &self.mcp_addr)
            .field("has_server", &self.server.is_some())
            .field("insecure_single_owner", &self.insecure_single_owner)
            .field(
                "workers",
                &self
                    .workers
                    .iter()
                    .map(|worker| worker.name)
                    .collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

fn spawn_embedding_worker(engine: Arc<Engine>, cancel: CancellationToken) -> JoinHandle<()> {
    tokio::spawn(async move {
        if engine.embed_client().is_none() {
            return;
        }
        // Boot-time catch-up, not a recurring clock: memories written while
        // no embedding client was configured never got a job (and exhausted
        // `failed` jobs stay dead), so one reconcile pass before the first
        // drain keeps a restart from leaving them silently unsearchable.
        // Recurring maintenance stays outside the process
        // (`proxima-mcp maintain-embeddings`).
        match engine
            .reconcile_embeddings(proxima_core::EmbeddingReconcileScope::MissingOnly, None)
            .await
        {
            Ok(outcome) if outcome.enqueued > 0 => {
                tracing::info!(
                    scanned = outcome.scanned,
                    enqueued = outcome.enqueued,
                    "startup embedding reconcile enqueued missing jobs"
                );
            }
            Ok(_) => {}
            Err(err) => {
                tracing::warn!(error = %err, "startup embedding reconcile failed");
            }
        }
        loop {
            if cancel.is_cancelled() {
                break;
            }
            let mut processed = 0usize;
            let mut failed = 0usize;
            loop {
                if cancel.is_cancelled() {
                    return;
                }
                match engine
                    .drain_embedding_jobs(proxima_core::llm::EMBEDDING_BATCH_SIZE)
                    .await
                {
                    Ok(outcome) if outcome.processed > 0 => {
                        processed += outcome.processed;
                        failed += outcome.failed;
                    }
                    Ok(_) => break,
                    Err(err) => {
                        tracing::warn!(error = %err, "embedding drain failed");
                        break;
                    }
                }
            }
            if processed > 0 {
                tracing::info!(processed, failed, "drained embedding jobs");
            }
            tokio::select! {
                () = cancel.cancelled() => break,
                () = tokio::time::sleep(EMBEDDING_WORKER_INTERVAL) => {}
            }
        }
    })
}

/// Run the configured app from process environment.
///
/// # Errors
///
/// Returns config, security, storage, engine, bind, or MCP serving errors.
pub async fn run<A: FlavorApp + 'static>() -> Result<RunningProxima, ProximaError> {
    Proxima::<A>::app().from_env().run().await
}

/// Compose rmcp and host-mounted routes behind one body, Host, and auth policy.
///
/// `host_allowlist` must also be passed to [`streamable_http_service`] so the
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
        .nest_service("/mcp", mcp_service)
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

/// The single erasure site for the cited-blob lane: the concrete
/// `CitedBlobStore` becomes the core [`CitedBlobService`] port newtype, so
/// every in-process consumer — MCP tools and flavor workers alike — holds
/// one abstract handle and none of them names `proxima-blob-s3`.
fn cited_blob_service(blobs: Option<&CitedBlobStore>) -> Option<CitedBlobService> {
    blobs.map(|store| CitedBlobService(Arc::new(store.clone())))
}

/// Host tool extensions plus the substrate-owned services every composed
/// binary gets for free. Today that is one entry: when S3 is configured
/// (`app_ctx.blobs`), the cited-blob store is published as
/// [`CitedBlobService`] so `core_upload` can reach the upload lane
/// without any host wiring. Flavor workers receive the same service
/// through [`FlavorWorkerContext::blobs`], not through this map.
fn assemble_tool_extensions<A: FlavorApp>(app_ctx: &AppContext) -> McpToolExtensions {
    let mut tool_extensions = A::mcp_tool_extensions(app_ctx);
    if let Some(service) = cited_blob_service(app_ctx.blobs.as_ref()) {
        tool_extensions.insert(service);
    }
    tool_extensions
}

async fn boot_app<A: FlavorApp + 'static>(
    config: &crate::RuntimeConfig,
    parts: &crate::RuntimeParts,
) -> Result<crate::EmbeddedProxima, ProximaError> {
    let mut builder = ProximaBuilder::new_optional(
        EmbedConfig {
            database_url: config.database_url.clone(),
            s3: config.s3.clone(),
        },
        config.owner,
    )
    .bundle::<A>()
    .deployment_tool_scope(config.tool_scope.clone());
    if config.skip_migrations {
        builder = builder.skip_migrations();
    }
    if let Some(client) = parts.embed_client.clone() {
        builder = builder.embed_client(client);
    }
    if let Some(client) = parts.anthropic.clone() {
        builder = builder.anthropic(client);
    }
    builder.boot().await.map_err(Into::into)
}

fn build_router<A: FlavorApp>(
    app_ctx: AppContext,
    tool_extensions: McpToolExtensions,
    authenticator: Option<Arc<dyn Authenticator>>,
    allowlist: OriginAllowlist,
    cancel: &CancellationToken,
    config: &crate::RuntimeConfig,
) -> Router {
    let engine = app_ctx.engine.clone();
    let mut edge_auth = McpEdgeAuth::headless().with_tool_scope(config.tool_scope.clone());
    if let Some(authenticator) = authenticator {
        edge_auth = edge_auth.with_host(authenticator);
    }
    let edge_auth = Arc::new(edge_auth);
    let mcp_host = McpToolHost::from_parts(Arc::new(engine.registry().clone()), tool_extensions)
        .with_engine(engine);
    let host_allowlist = resolve_host_allowlist(config);
    let rest_router = rest_router(&mcp_host, config);
    let mcp_service = streamable_http_service(mcp_host, &allowlist, &host_allowlist, cancel);
    let app_router = A::mount_http(Router::new(), app_ctx);
    let www = config
        .resource_metadata
        .as_ref()
        .and_then(|md| axum::http::HeaderValue::from_str(&md.www_authenticate_value()).ok());
    let auth_layer = proxima_mcp_server::mcp_auth_layer_with_metadata(
        edge_auth,
        config.stream_revalidation,
        www,
    );
    let protected_router = Router::new()
        .nest_service("/mcp", mcp_service)
        .merge(rest_router)
        .merge(app_router)
        .layer(auth_layer);
    let mut router = protected_router;
    if let Some(md) = &config.resource_metadata {
        router = router.merge(proxima_mcp_server::protected_resource_router(md));
    }
    // Apply listener-wide layers only after anonymous OAuth metadata has been
    // merged. Body-size rejection remains outermost; Host validation then runs
    // before CORS and bearer auth. CORS covers public metadata and preflights;
    // bearer auth remains inside it on protected routes.
    router
        .layer(cors_layer(allowlist))
        .layer(host_guard_layer(host_allowlist))
        .layer(axum::middleware::from_fn(
            proxima_mcp_server::enforce_body_limit,
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

    mod beta {
        proxima_core::proxima_flavor! {
            name = "proxima-runtime-beta",
            fact_schemas = [],
            abstraction_schemas = [],
            perspective_schemas = [],
            goal_schemas = [],
            mcp_tools = [],
        }
    }

    struct AlphaApp;
    struct BetaApp;

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
            builder.database_url("postgres://alpha/proxima")
        }
    }

    impl FlavorBundle for BetaApp {
        fn register(registry: &mut FlavorRegistry) -> Result<(), FlavorRegistryError> {
            beta::register(registry)
        }

        fn migrators() -> Vec<crate::NamedMigrator> {
            Vec::new()
        }
    }

    impl FlavorApp for BetaApp {
        fn app_info() -> AppInfo {
            AppInfo {
                id: "beta",
                title: "Beta",
                version: "1",
            }
        }

        fn configure(builder: RuntimeBuilder) -> RuntimeBuilder {
            builder.database_url("postgres://beta/proxima")
        }
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
            .with_mcp()
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
            .with_mcp()
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
            .with_mcp()
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
            .with_mcp()
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
        // public_url host (the bug's `proxima.aqs-dev.cloud`) must be in.
        let owner = owner();
        let (config, _) = RuntimeBuilder::default()
            .database_url("postgres://unused/db")
            .owner(owner)
            .tool_scope(ToolScope::All)
            .with_mcp()
            .mcp_bind("0.0.0.0:8080".parse().unwrap())
            .expose_network(true)
            .allowed_origins(vec!["https://app.example.com".to_string()])
            .resource_metadata(proxima_mcp_server::ResourceServerMetadata {
                public_url: "https://proxima.aqs-dev.cloud".to_string(),
                authorization_servers: vec!["https://idp.test".to_string()],
            })
            .authenticator(Arc::new(StubAuth { owner }))
            .resolve()
            .unwrap();

        let allowlist = resolve_host_allowlist(&config);
        let hosts = allowlist.hosts();
        assert!(hosts.iter().any(|host| host == "proxima.aqs-dev.cloud"));
        assert!(hosts.iter().any(|host| host == "app.example.com"));
    }

    #[test]
    fn merge_over_precedence_is_overlay_over_env_over_configure() {
        let base = AlphaApp::configure(RuntimeBuilder::default())
            .owner(owner())
            .allowed_origins(vec!["https://base.test".to_string()]);
        let env = RuntimeBuilder::default()
            .database_url("postgres://env/proxima")
            .allowed_origins(vec!["https://env.test".to_string()])
            .with_mcp()
            .authenticator(Arc::new(StubAuth { owner: owner() }));
        let overlay = RuntimeBuilder::default()
            .database_url("postgres://overlay/proxima")
            .tool_scope(ToolScope::All)
            .stream_max_lifetime(std::time::Duration::from_secs(12));

        let (config, _) = overlay.merge_over(env.merge_over(base)).resolve().unwrap();

        assert_eq!(config.database_url, "postgres://overlay/proxima");
        assert_eq!(config.allowed_origins, ["https://env.test".to_string()]);
        assert!(config.mcp.is_some());
        assert_eq!(
            config.stream_revalidation.max_stream_lifetime,
            std::time::Duration::from_secs(12)
        );
    }

    #[test]
    fn tuple_flavor_app_uses_first_info_and_left_to_right_configure() {
        let _compiled = Proxima::<(AlphaApp, BetaApp)>::app();

        assert_eq!(<(AlphaApp, BetaApp) as FlavorApp>::app_info().id, "alpha");

        let builder = <(AlphaApp, BetaApp) as FlavorApp>::configure(RuntimeBuilder::default())
            .owner(owner())
            .tool_scope(ToolScope::All);
        let (config, _) = builder.resolve().unwrap();
        assert_eq!(config.database_url, "postgres://beta/proxima");
    }

    #[tokio::test]
    async fn build_refuses_mcp_without_authenticator_or_insecure_mode_before_storage() {
        let err = Proxima::<AlphaApp>::app()
            .database_url("postgres://unused:5432/unused")
            .owner(owner())
            .tool_scope(ToolScope::All)
            .with_mcp()
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
            .with_mcp()
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
            .with_mcp()
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
            .with_mcp()
            .expose_network(true)
            .authenticator(Arc::new(StubAuth { owner }))
            .build()
            .await
            .unwrap_err();

        assert!(matches!(err, ProximaError::Security(_)));
    }

    #[tokio::test]
    async fn build_refuses_missing_tool_scope_with_config_error() {
        // Fail-closed default: an embedding host that never calls
        // `.tool_scope(...)` gets a config error instead of silently
        // advertising `ToolScope::All` (the retired implicit default).
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

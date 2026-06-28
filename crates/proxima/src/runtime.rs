use std::convert::Infallible;
use std::marker::PhantomData;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::Request;
use axum::response::IntoResponse;
use proxima_blob_s3::{CitedBlobStore, S3RuntimeConfig};
use proxima_core::{
    AnthropicClient, AuthPath, Authenticator, AuthzContext, EmbeddingClient, FlavorRegistryFrozen,
    RevalidationConfig, ToolScope,
};
use proxima_core::{Engine, EngineHandle, Owner, OwnerRef, Role, UserId};
use proxima_mcp_server::{
    McpEdgeAuth, McpToolHost, OriginAllowlist, assert_loopback, default_allowlist,
    streamable_http_service,
};
use sqlx::PgPool;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tower::Service;

use crate::{
    AppContext, CoreMcpTools, EmbedConfig, FlavorApp, PgSidecarRegistryFrozen, ProximaBuilder,
    ProximaError, RuntimeBuilder,
};

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
    pub fn master_token(mut self, master_token: impl Into<String>) -> Self {
        self.overlay = self.overlay.master_token(master_token);
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

        let service = if let Some(allowlist) = allowlist {
            Some(
                build_router::<A>(
                    &booted.engine,
                    booted.pool.clone(),
                    booted.blobs.clone(),
                    parts.authenticator,
                    allowlist,
                    &cancel,
                    &config,
                )
                .await,
            )
        } else {
            None
        };

        Ok(BuiltProxima {
            service,
            engine: booted.engine,
            handle: booted.handle,
            pool: booted.pool,
            registry: booted.registry,
            pg_sidecars: booted.pg_sidecars,
            blobs: booted.blobs,
            owner: booted.owner,
            cancel,
            insecure_single_owner: config.insecure_single_owner,
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

        let (mcp_addr, server) = if let (Some(mcp), Some(allowlist)) = (config.mcp, allowlist) {
            if !config.expose_network {
                assert_loopback(&mcp.bind)
                    .map_err(|err| ProximaError::Security(err.to_string()))?;
            }
            let app = build_router::<A>(
                &booted.engine,
                booted.pool.clone(),
                booted.blobs.clone(),
                parts.authenticator,
                allowlist,
                &cancel,
                &config,
            )
            .await;
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

        Ok(RunningProxima {
            engine: booted.engine,
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
    pub handle: EngineHandle,
    pub pool: PgPool,
    pub registry: Arc<FlavorRegistryFrozen>,
    pub pg_sidecars: Arc<PgSidecarRegistryFrozen>,
    pub blobs: Option<CitedBlobStore>,
    pub owner: Owner,
    pub cancel: CancellationToken,
    pub insecure_single_owner: bool,
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
            .then(|| insecure_single_owner_authz(&self.owner, AuthPath::System))
    }

    #[must_use]
    pub fn core_mcp_tools(&self) -> CoreMcpTools {
        CoreMcpTools::new(
            self.pool.clone(),
            self.owner,
            self.registry.clone(),
            self.engine.clone(),
        )
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
    pub handle: EngineHandle,
    pub pool: PgPool,
    pub registry: Arc<FlavorRegistryFrozen>,
    pub pg_sidecars: Arc<PgSidecarRegistryFrozen>,
    pub blobs: Option<CitedBlobStore>,
    pub owner: Owner,
    pub mcp_addr: Option<SocketAddr>,
    pub server: Option<JoinHandle<()>>,
    pub cancel: CancellationToken,
    pub insecure_single_owner: bool,
}

impl RunningProxima {
    pub async fn shutdown(self) {
        self.cancel.cancel();
        if let Some(server) = self.server
            && let Err(err) = server.await
        {
            tracing::warn!(error = %err, "proxima facade server join failed");
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
            .then(|| insecure_single_owner_authz(&self.owner, AuthPath::System))
    }
}

type InsecureAuthz = AuthzContext;

fn insecure_single_owner_authz(owner: &Owner, auth_path: AuthPath) -> InsecureAuthz {
    match *owner {
        OwnerRef::Personal(subject) => AuthzContext::for_subject(subject, auth_path),
        OwnerRef::Group(group) => AuthzContext::for_subject_with_role(
            UserId::new(group.into_inner()),
            [(*owner, Role::admin())],
            auth_path,
        ),
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
            .finish_non_exhaustive()
    }
}

fn spawn_embedding_worker(engine: Arc<Engine>, cancel: CancellationToken) -> JoinHandle<()> {
    tokio::spawn(async move {
        if engine.embed_client().is_none() {
            return;
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
                match engine.drain_embedding_jobs(1).await {
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

pub fn layered_router<S>(
    mcp_service: S,
    app_router: Router,
    edge_auth: Arc<McpEdgeAuth>,
    allowlist: OriginAllowlist,
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
        RevalidationConfig::default(),
    )
}

pub fn layered_router_with_revalidation<S>(
    mcp_service: S,
    app_router: Router,
    edge_auth: Arc<McpEdgeAuth>,
    allowlist: OriginAllowlist,
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
            allowlist,
            revalidation,
        ))
}

async fn boot_app<A: FlavorApp + 'static>(
    config: &crate::RuntimeConfig,
    parts: &crate::RuntimeParts,
) -> Result<crate::EmbeddedProxima, ProximaError> {
    let mut builder = ProximaBuilder::new(
        EmbedConfig {
            database_url: config.database_url.clone(),
            s3: config.s3.clone(),
        },
        config.owner,
    )
    .bundle::<A>();
    if let Some(client) = parts.embed_client.clone() {
        builder = builder.embed_client(client);
    }
    if let Some(client) = parts.anthropic.clone() {
        builder = builder.anthropic(client);
    }
    builder.boot().await.map_err(Into::into)
}

async fn build_router<A: FlavorApp>(
    engine: &Arc<Engine>,
    pool: PgPool,
    blobs: Option<CitedBlobStore>,
    authenticator: Option<Arc<dyn Authenticator>>,
    allowlist: OriginAllowlist,
    cancel: &CancellationToken,
    config: &crate::RuntimeConfig,
) -> Router {
    let owner = config.owner;
    let mut edge_auth = McpEdgeAuth::headless().with_tool_scope(config.tool_scope.clone());
    if let Some(authenticator) = authenticator {
        edge_auth = edge_auth.with_host(authenticator, owner);
    }
    if let Some(token) = config.master_token {
        edge_auth.replace_local_master_token(token, owner).await;
    }
    let edge_auth = Arc::new(edge_auth);
    let mcp_host = McpToolHost::from_pool(pool.clone(), owner, Arc::new(engine.registry().clone()))
        .with_engine(engine.clone());
    let allowed_hosts = resolve_allowed_hosts(config);
    let mcp_service = streamable_http_service(mcp_host, &allowlist, &allowed_hosts, cancel);
    let app_router = A::mount_http(
        Router::new(),
        AppContext {
            engine: engine.clone(),
            pool,
            blobs,
            owner,
        },
    );
    let www = config
        .resource_metadata
        .as_ref()
        .and_then(|md| axum::http::HeaderValue::from_str(&md.www_authenticate_value()).ok());
    let auth_layer = proxima_mcp_server::mcp_auth_layer_with_metadata(
        edge_auth,
        allowlist,
        config.stream_revalidation,
        www,
    );
    let mut router = Router::new()
        .nest_service("/mcp", mcp_service)
        .merge(app_router)
        .layer(auth_layer);
    if let Some(md) = &config.resource_metadata {
        router = router.merge(proxima_mcp_server::protected_resource_router(md));
    }
    router
}

fn resolve_allowlist(config: &crate::RuntimeConfig) -> Result<OriginAllowlist, ProximaError> {
    if config.allowed_origins.is_empty() {
        return Ok(default_allowlist());
    }
    OriginAllowlist::parse(&config.allowed_origins)
        .map_err(|err| ProximaError::Security(err.to_string()))
}

/// Inbound `Host` allowlist for rmcp's DNS-rebinding guard.
///
/// Loopback binds return empty, keeping rmcp's loopback-only default.
/// Network-exposed binds get loopback (so a gateway may still rewrite
/// `Host` to localhost, and port-forwards work) plus the resolved
/// public host(s) — without which every public request 403s before
/// auth. `validate` has already guaranteed the public set is non-empty
/// when exposed.
fn resolve_allowed_hosts(config: &crate::RuntimeConfig) -> Vec<String> {
    if !config.expose_network {
        return Vec::new();
    }
    let mut hosts = vec![
        "localhost".to_string(),
        "127.0.0.1".to_string(),
        "::1".to_string(),
    ];
    for host in config.public_allowed_hosts() {
        if !hosts.contains(&host) {
            hosts.push(host);
        }
    }
    hosts
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use proxima_core::{
        AuthError, AuthPath, Authenticator, AuthzContext, Credentials, FlavorRegistry, Owner,
    };
    use uuid::Uuid;

    use super::*;
    use crate::{AppInfo, FlavorBundle, RuntimeBuilder, company_owner};

    mod alpha {
        proxima_core::proxima_flavor! {
            name = "proxima-runtime-alpha",
            fact_schemas = [],
            abstraction_schemas = [],
            perspective_schemas = [],
            goal_schemas = [],
            edge_schemas = [],
            relations = [],
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
            edge_schemas = [],
            relations = [],
            mcp_tools = [],
        }
    }

    struct AlphaApp;
    struct BetaApp;

    impl FlavorBundle for AlphaApp {
        fn register(registry: &mut FlavorRegistry) {
            alpha::register(registry);
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
        fn register(registry: &mut FlavorRegistry) {
            beta::register(registry);
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

    // --- Host-allowlist exposure invariants (DNS-rebinding guard) ---
    //
    // The hazard when configuring rmcp's guard is its allow-all state:
    // an EMPTY `allowed_hosts` makes rmcp accept any inbound Host. These
    // lock the two properties that keep that from happening accidentally.

    #[test]
    fn resolve_allowed_hosts_is_empty_for_loopback_bind() {
        let owner = owner();
        let (config, _) = RuntimeBuilder::default()
            .database_url("postgres://unused/db")
            .owner(owner)
            .with_mcp()
            .authenticator(Arc::new(StubAuth { owner }))
            .resolve()
            .unwrap();

        // Not exposed ⇒ no override; the transport keeps rmcp's
        // loopback-only DEFAULT. Empty here means "don't override",
        // never reaches `with_allowed_hosts`, so rmcp's allow-all
        // (its own empty-list state) is unreachable from this path.
        assert!(!config.expose_network);
        assert!(resolve_allowed_hosts(&config).is_empty());
    }

    #[test]
    fn resolve_allowed_hosts_exposed_is_loopback_plus_public_and_never_empty() {
        let owner = owner();
        let (config, _) = RuntimeBuilder::default()
            .database_url("postgres://unused/db")
            .owner(owner)
            .with_mcp()
            .mcp_bind("0.0.0.0:8080".parse().unwrap())
            .expose_network(true)
            .allowed_origins(vec!["https://app.example.com".to_string()])
            .authenticator(Arc::new(StubAuth { owner }))
            .resolve()
            .unwrap();

        let hosts = resolve_allowed_hosts(&config);
        // Loopback stays (gateway Host-rewrite + port-forward keep working)…
        assert!(hosts.contains(&"localhost".to_string()));
        assert!(hosts.contains(&"127.0.0.1".to_string()));
        assert!(hosts.contains(&"::1".to_string()));
        // …and the public host is present, but the list is NEVER empty —
        // so an exposed bind always hands rmcp a non-empty allowlist and
        // can never trip its allow-all state.
        assert!(hosts.contains(&"app.example.com".to_string()));
        assert!(!hosts.is_empty());
    }

    #[test]
    fn resolve_allowed_hosts_includes_public_url_host_distinct_from_origins() {
        // Split deployment: browser app origin differs from the MCP host.
        // Deriving from origins alone would miss the real Host; the
        // public_url host (the bug's `proxima.aqs-dev.cloud`) must be in.
        let owner = owner();
        let (config, _) = RuntimeBuilder::default()
            .database_url("postgres://unused/db")
            .owner(owner)
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

        let hosts = resolve_allowed_hosts(&config);
        assert!(hosts.contains(&"proxima.aqs-dev.cloud".to_string()));
        assert!(hosts.contains(&"app.example.com".to_string()));
    }

    #[test]
    fn merge_over_precedence_is_overlay_over_env_over_configure() {
        let base = AlphaApp::configure(RuntimeBuilder::default())
            .owner(owner())
            .allowed_origins(vec!["https://base.test".to_string()]);
        let env = RuntimeBuilder::default()
            .database_url("postgres://env/proxima")
            .allowed_origins(vec!["https://env.test".to_string()])
            .with_mcp();
        let overlay = RuntimeBuilder::default()
            .database_url("postgres://overlay/proxima")
            .allow_insecure_single_owner();

        let (config, _) = overlay.merge_over(env.merge_over(base)).resolve().unwrap();

        assert_eq!(config.database_url, "postgres://overlay/proxima");
        assert_eq!(config.allowed_origins, ["https://env.test".to_string()]);
        assert!(config.mcp.is_some());
        assert!(config.insecure_single_owner);
    }

    #[test]
    fn tuple_flavor_app_uses_first_info_and_left_to_right_configure() {
        let _compiled = Proxima::<(AlphaApp, BetaApp)>::app();

        assert_eq!(<(AlphaApp, BetaApp) as FlavorApp>::app_info().id, "alpha");

        let builder =
            <(AlphaApp, BetaApp) as FlavorApp>::configure(RuntimeBuilder::default()).owner(owner());
        let (config, _) = builder.resolve().unwrap();
        assert_eq!(config.database_url, "postgres://beta/proxima");
    }

    #[tokio::test]
    async fn build_refuses_mcp_without_authenticator_or_insecure_mode_before_storage() {
        let err = Proxima::<AlphaApp>::app()
            .database_url("postgres://unused:5432/unused")
            .owner(owner())
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
            .with_mcp()
            .expose_network(true)
            .authenticator(Arc::new(StubAuth { owner }))
            .build()
            .await
            .unwrap_err();

        assert!(matches!(err, ProximaError::Security(_)));
    }
}

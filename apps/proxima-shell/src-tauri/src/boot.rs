use std::sync::Arc;

use futures_util::future::BoxFuture;
use proxima_core::auth::NoAuth;
use proxima_core::engine::EngineMcpListener;
use proxima_core::llm::EmbeddingClient;
use proxima_core::secrets::ResolverRegistry;
use proxima_core::{
    EmbeddingClientReloader, Engine, FlavorRegistry, FlavorRegistryFrozen, OrgId, Owner, Principal,
    UserId,
};
use proxima_embed::{NamedMigrator, run_core_and_flavor_migrations};
use proxima_llm_openai_compat::{OpenAiCompatConfig, OpenAiCompatEmbeddingClient};
use proxima_mcp_server::{EngineHostedMcpListener, McpEdgeAuth, McpToolHost, default_allowlist};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

use crate::config::AppConfig;

const DEFAULT_MCP_BIND: &str = "127.0.0.1:31415";

/// Panics if `DATABASE_URL` is not set — settings persistence is
/// required for the desktop shell.
pub(crate) fn build_engine() -> (Engine, Arc<PgStorage>) {
    let owner = Owner {
        principal: Principal::User(UserId::new(Uuid::nil())),
        org_id: OrgId::new(Uuid::nil()),
    };

    let url = std::env::var("DATABASE_URL").expect(
        "DATABASE_URL must be set for the desktop shell — settings persistence is required",
    );

    tauri::async_runtime::block_on(async {
        let pg = PgStorage::connect(&url)
            .await
            .expect("failed to connect to Postgres; check DATABASE_URL");
        // Substrate migrations run before the engine is built so the
        // engine's snapshot path can LEFT-JOIN agent-note sidecars when
        // the Atlas inspects MCP-authored memories. Flavor migrations
        // are composition's job — the MCP listener no longer self-migrates.
        run_core_and_flavor_migrations(
            &pg,
            [
                NamedMigrator::new("proxima-code", proxima_code::migrator()),
                NamedMigrator::new("proxima-mcp-substrate", proxima_mcp_substrate::migrator()),
                NamedMigrator::new("proxima-flavor-goal", proxima_flavor_goal::migrator()),
            ],
        )
        .await
        .expect("failed to run core + flavor migrations");

        // Single-writer invariant (docs/09 §Embedded engine mode): any
        // queued/running run at boot is a prior-process orphan whose
        // in-memory driver is gone. Retire them so the partial unique
        // index doesn't block fresh `repo_ingest_start` calls.
        match proxima_code::sweep_orphaned_runs(pg.pool()).await {
            Ok(0) => {}
            Ok(swept) => tracing::info!(swept, "retired orphaned ingestion runs at boot"),
            Err(e) => tracing::warn!("orphan-run sweep failed at boot: {e}"),
        }

        pg.start_outbox()
            .await
            .expect("failed to start outbox listener");

        let pg_for_settings = Arc::new(pg.clone());
        let auth = NoAuth::new(owner.principal.clone(), owner.clone());
        // Compose substrate + code into the engine's schema registry so
        // Settings → Schemas surfaces both flavors and the Atlas
        // inspector can decode agent-note sidecars.
        let engine = proxima_code::build_engine_with(pg, Box::new(auth), |registry| {
            proxima_mcp_substrate::register(registry);
            proxima_flavor_goal::register(registry);
        })
        .with_embedding_reloader(Arc::new(ShellEmbeddingClientReloader {
            pg: pg_for_settings.clone(),
        }));

        (
            wire_consolidation_clients(engine, &pg_for_settings, &owner).await,
            pg_for_settings,
        )
    })
}

/// Sentinel owner for shell-side operations (v1 single-tenant).
/// Multi-tenant deployments (v1.1+) wire owner from the auth
/// context without changing the command shape.
pub(crate) fn sentinel_owner() -> Owner {
    Owner {
        principal: Principal::User(UserId::new(Uuid::nil())),
        org_id: OrgId::new(Uuid::nil()),
    }
}

#[derive(Debug)]
struct ShellEmbeddingClientReloader {
    pg: Arc<PgStorage>,
}

impl EmbeddingClientReloader for ShellEmbeddingClientReloader {
    fn reload<'a>(
        &'a self,
        owner: &'a Owner,
    ) -> BoxFuture<'a, Result<Option<Arc<dyn EmbeddingClient>>, String>> {
        Box::pin(async move {
            let cfg = crate::config::load_app_config(&self.pg, owner)
                .await
                .map_err(|e| e.to_string())?;
            crate::config::validate_config(&cfg).map_err(|e| e.to_string())?;
            if cfg.embedding.active.is_none() {
                return Ok(None);
            }
            let embed = resolve_consolidation_clients(&cfg)?;
            tracing::info!(
                embed_model = embed.model_id(),
                embed_dim = embed.dim(),
                "embedding client hot-reloaded"
            );
            Ok(Some(Arc::new(embed) as Arc<dyn EmbeddingClient>))
        })
    }
}

/// Load settings at engine boot and attach the embedding client when
/// the registered rows are complete. Inference target/tier validation
/// happens at `WakeEntry` write and dispatch time.
async fn wire_consolidation_clients(engine: Engine, pg: &Arc<PgStorage>, owner: &Owner) -> Engine {
    let cfg = match crate::config::load_app_config(pg, owner).await {
        Ok(cfg) => cfg,
        Err(e) => {
            tracing::warn!("could not load AppConfig at boot: {e}");
            return engine;
        }
    };
    if let Err(e) = crate::config::validate_config(&cfg) {
        tracing::warn!(
            "AppConfig validation failed at boot — running with degraded \
             config; user must fix via settings UI: {e}"
        );
        return engine;
    }

    match resolve_consolidation_clients(&cfg) {
        Ok(embed) => {
            tracing::info!(
                embed_model = embed.model_id(),
                embed_dim = embed.dim(),
                "embedding client attached"
            );
            engine.with_embed(Arc::new(embed))
        }
        Err(e) => {
            tracing::warn!("embedding disabled at boot: {e}");
            engine
        }
    }
}

pub(crate) fn resolve_consolidation_clients(
    cfg: &AppConfig,
) -> Result<OpenAiCompatEmbeddingClient, String> {
    let active_ref = cfg
        .embedding
        .active
        .as_ref()
        .ok_or_else(|| "missing active embedding model".to_string())?;
    let embed = cfg
        .embedding
        .models
        .iter()
        .find(|m| m.vendor == active_ref.vendor && m.model_id == active_ref.model_id)
        .ok_or_else(|| format!("active embedding points at unknown model {active_ref:?}"))?;

    let resolver = shell_secret_resolver();
    let embed_secret = resolve_optional_secret(&resolver, embed.secret_ref.as_deref())
        .map_err(|e| format!("could not resolve embedding secret_ref for {active_ref:?}: {e}"))?;
    let embed_base_url = normalize_openai_compat_base_url(&embed.vendor, &embed.base_url);

    let embed_client = OpenAiCompatEmbeddingClient::new(
        embed.model_id.clone(),
        embed.caps,
        OpenAiCompatConfig::new(embed_base_url, embed_secret),
    )
    .map_err(|e| {
        format!("could not construct OpenAI-compatible embedding client for {active_ref:?}: {e}")
    })?;
    Ok(embed_client)
}

pub(crate) fn normalize_openai_compat_base_url(vendor: &str, base_url: &str) -> String {
    let trimmed = base_url.trim().trim_end_matches('/').to_string();
    let vendor = vendor.to_ascii_lowercase();
    if vendor.contains("ollama") && !trimmed.ends_with("/v1") {
        format!("{trimmed}/v1")
    } else {
        trimmed
    }
}

fn shell_secret_resolver() -> ResolverRegistry {
    let mut resolver = ResolverRegistry::default_with_env();
    #[cfg(debug_assertions)]
    resolver.register(Box::new(crate::dev_secrets::DevFileSecretResolver::new()));
    #[cfg(not(debug_assertions))]
    resolver.register(Box::new(crate::secrets::KeychainResolver::new()));
    resolver
}

/// Wires the per-OS `keyring-core` default store. Idempotent —
/// returns early once a store is installed.
///
/// keyring 4.0 split the library into `keyring-core` plus per-OS
/// store crates; without an explicit `set_default_store(...)`, every
/// `Entry` op fails with `Error::NoDefaultStore`. Each host platform
/// pulls in one store crate via cfg-gated deps in `Cargo.toml`; this
/// fn picks the matching one at compile time. `run()` only invokes
/// this in release builds (debug uses file-backed `dev_secrets`),
/// but the fn stays callable in any profile so `#[ignore]`'d
/// integration tests that talk to the real OS keychain can opt in.
#[allow(dead_code)] // Unused in debug builds outside of `#[cfg(test)]` opt-in callers.
pub(crate) fn install_keychain_default_store() {
    if keyring_core::get_default_store().is_some() {
        return;
    }
    // Apple ships two store flavors (`keychain` / `protected`); we
    // enable only `keychain` for classic Generic Password items, which
    // matches keyring v3's `apple-native` behavior.
    #[cfg(target_os = "macos")]
    let store = apple_native_keyring_store::keychain::Store::new();
    #[cfg(target_os = "windows")]
    let store = windows_native_keyring_store::Store::new();
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    let store = dbus_secret_service_keyring_store::Store::new();

    match store {
        Ok(store) => keyring_core::set_default_store(store),
        Err(err) => tracing::error!(
            "failed to initialize OS keychain store: {err}; \
             keychain-backed secrets and the MCP master token will fail to resolve"
        ),
    }
}

fn resolve_optional_secret(
    resolver: &ResolverRegistry,
    secret_ref: Option<&str>,
) -> Result<Option<String>, String> {
    let Some(secret_ref) = secret_ref.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let bytes = resolver.resolve(secret_ref).map_err(|e| e.to_string())?;
    let value = bytes
        .as_str()
        .ok_or_else(|| format!("secret_ref {secret_ref:?} resolved to non-UTF8 bytes"))?;
    Ok(Some(value.to_string()))
}

/// # Errors
///
/// Returns bind, migration, or transport setup failures. Shell callers
/// log and continue without MCP.
///
/// `auth` must wrap the same wake-token store the engine's
/// dispatcher mints into ([`Engine::wake_token_store`]) and may also
/// carry Shell-local master tokens for user-configured MCP clients.
pub(crate) fn build_mcp_listener(
    pool: sqlx::PgPool,
    owner: Owner,
    auth: Arc<McpEdgeAuth>,
) -> Result<
    (
        Arc<dyn EngineMcpListener>,
        std::net::SocketAddr,
        McpToolHost,
    ),
    proxima_mcp_server::McpServerError,
> {
    let bind_raw =
        std::env::var("PROXIMA_MCP_BIND").unwrap_or_else(|_| DEFAULT_MCP_BIND.to_string());
    let bind = bind_raw.parse().map_err(|err| {
        proxima_mcp_server::McpServerError::Bind(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("PROXIMA_MCP_BIND={bind_raw}: {err}"),
        ))
    })?;

    let mut registry = FlavorRegistry::new();
    proxima_mcp_substrate::register(&mut registry);
    proxima_flavor_goal::register(&mut registry);
    proxima_code::register(&mut registry);
    let frozen: Arc<FlavorRegistryFrozen> = Arc::new(registry.freeze());
    let server = McpToolHost::from_pool(pool, owner, frozen);
    Ok((
        Arc::new(EngineHostedMcpListener::with_edge_auth(
            server.clone(),
            default_allowlist(),
            auth,
        )),
        bind,
        server,
    ))
}

#[cfg(test)]
mod resolve_client_tests {
    use super::*;
    use crate::config::{AppConfig, EmbeddingConfig, EmbeddingModelRecord, EmbeddingModelRef};
    use proxima_core::models::EmbedCaps;

    fn embed_row(vendor: &str, model_id: &str, dim: u32) -> EmbeddingModelRecord {
        EmbeddingModelRecord {
            vendor: vendor.into(),
            model_id: model_id.into(),
            base_url: "http://localhost:11434".into(),
            caps: EmbedCaps {
                dim,
                matryoshka: false,
            },
            secret_ref: None,
        }
    }

    fn config_with_embedding(dim: u32) -> AppConfig {
        AppConfig {
            embedding: EmbeddingConfig {
                models: vec![embed_row("ollama", "nomic-embed-text", dim)],
                active: Some(EmbeddingModelRef {
                    vendor: "ollama".into(),
                    model_id: "nomic-embed-text".into(),
                }),
            },
            ..AppConfig::default()
        }
    }

    #[test]
    fn model_resolution_builds_openai_compatible_embedding_client() {
        let cfg = config_with_embedding(768);
        let embed = resolve_consolidation_clients(&cfg).expect("clients");
        assert_eq!(embed.model_id(), "nomic-embed-text");
        assert_eq!(embed.dim(), 768);
    }

    #[test]
    fn model_resolution_missing_active_embedding_degrades() {
        let mut cfg = config_with_embedding(768);
        cfg.embedding.active = None;
        let err = resolve_consolidation_clients(&cfg).unwrap_err();
        assert!(err.contains("missing active embedding model"));
    }

    #[test]
    fn model_resolution_uses_embedding_caps_dim() {
        let cfg = config_with_embedding(1_536);
        let embed = resolve_consolidation_clients(&cfg).expect("clients");
        assert_eq!(embed.dim(), 1_536);
    }
}

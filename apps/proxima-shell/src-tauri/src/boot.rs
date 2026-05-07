use std::sync::Arc;

use proxima_core::auth::NoAuth;
use proxima_core::llm::anthropic_http::AnthropicHttpClient;
use proxima_core::llm::{AnthropicClient, EmbeddingClient};
use proxima_core::models::ModelTier;
use proxima_core::secrets::ResolverRegistry;
use proxima_core::{Engine, FlavorRegistry, FlavorRegistryFrozen, OrgId, Owner, Principal, UserId};
use proxima_llm_openai_compat::{OpenAiCompatConfig, OpenAiCompatEmbeddingClient};
use proxima_mcp_server::{DevMcpServer, default_allowlist, serve_streamable_http};
use proxima_storage_pg::PgStorage;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::config::AppConfig;

const DEFAULT_MCP_BIND: &str = "127.0.0.1:31415";

/// Panics if `DATABASE_URL` is not set — settings persistence is
/// required for the desktop shell.
pub(crate) fn build_engine() -> (Arc<Engine>, Arc<PgStorage>) {
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
        pg.run_migrations().await.expect("failed to run migrations");
        proxima_code::migrator()
            .run(pg.pool())
            .await
            .expect("failed to run proxima-code flavor migrations");
        // Substrate migrations run before the engine is built so the
        // engine's snapshot path can LEFT-JOIN agent-note sidecars when
        // the Atlas inspects MCP-authored memories. The MCP listener's
        // own migrator() call is now redundant but harmless (idempotent).
        proxima_mcp_substrate::migrator()
            .run(pg.pool())
            .await
            .expect("failed to run proxima-mcp-substrate flavor migrations");
        proxima_flavor_goal::migrator()
            .run(pg.pool())
            .await
            .expect("failed to run proxima-goal flavor migrations");

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
        });

        let engine = wire_consolidation_clients(engine, &pg_for_settings, &owner).await;
        let engine = Arc::new(engine);

        (engine, pg_for_settings)
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

/// Validate the loaded `AppConfig` at engine boot and attach local
/// OpenAI-compatible clients when the registered rows are complete.
/// Loads settings from PG, assembles an `AppConfig`, and runs
/// `validate_config` against the engine. Failures are logged as
/// warnings only — the settings UI exists to fix broken config;
/// panicking would brick the app.
async fn wire_consolidation_clients(engine: Engine, pg: &Arc<PgStorage>, owner: &Owner) -> Engine {
    let cfg = match crate::config::load_app_config(pg, owner).await {
        Ok(cfg) => cfg,
        Err(e) => {
            tracing::warn!("could not load AppConfig at boot: {e}");
            return engine;
        }
    };
    if let Err(e) = crate::config::validate_config(&cfg, &engine) {
        tracing::warn!(
            "AppConfig validation failed at boot — running with degraded \
             config; user must fix via settings UI: {e}"
        );
        return engine;
    }

    let engine = match resolve_consolidation_clients(&cfg) {
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
    };

    match resolve_anthropic_client(&cfg) {
        Ok(Some(anthropic)) => {
            tracing::info!(
                fast = anthropic.model_id_for(ModelTier::Fast),
                standard = anthropic.model_id_for(ModelTier::Standard),
                deep = anthropic.model_id_for(ModelTier::Deep),
                "Anthropic client attached; personality wakes enabled"
            );
            engine.with_anthropic(Arc::new(anthropic))
        }
        Ok(None) => {
            tracing::info!(
                "no Anthropic tier bindings found; personality wakes will defer \
                 until tiers are bound to an Anthropic model in settings"
            );
            engine
        }
        Err(e) => {
            tracing::warn!("Anthropic client disabled at boot: {e}");
            engine
        }
    }
}

/// Build an `AnthropicHttpClient` from the validated `AppConfig` if the
/// user has bound any tier to a `vendor = "anthropic"` model.
///
/// `validate_config` has already enforced that every tier required by
/// registered personalities is bound to a model with sufficient caps,
/// so a missing or non-Anthropic binding here just means the user
/// hasn't configured Anthropic for that tier — wakes at that tier will
/// defer rather than fail loudly.
pub(crate) fn resolve_anthropic_client(
    cfg: &AppConfig,
) -> Result<Option<AnthropicHttpClient>, String> {
    let anthropic_rows: Vec<&_> = cfg
        .llm
        .models
        .iter()
        .filter(|m| m.vendor.eq_ignore_ascii_case("anthropic"))
        .collect();
    if anthropic_rows.is_empty() {
        return Ok(None);
    }
    let key_source = anthropic_rows[0];
    let resolver = shell_secret_resolver();
    let api_key = resolve_optional_secret(&resolver, key_source.secret_ref.as_deref())
        .map_err(|e| format!("could not resolve anthropic secret_ref: {e}"))?
        .ok_or_else(|| {
            "anthropic model has no secret_ref or it resolved to empty".to_string()
        })?;
    let base_url = key_source.base_url.trim_end_matches('/').to_string();

    let resolve_tier = |tier: ModelTier| -> String {
        cfg.tiers
            .get(tier)
            .filter(|r| r.vendor.eq_ignore_ascii_case("anthropic"))
            .map(|r| r.model_id.clone())
            .unwrap_or_default()
    };
    let fast = resolve_tier(ModelTier::Fast);
    let standard = resolve_tier(ModelTier::Standard);
    let deep = resolve_tier(ModelTier::Deep);
    if fast.is_empty() && standard.is_empty() && deep.is_empty() {
        return Ok(None);
    }

    AnthropicHttpClient::with_base_url(base_url, api_key, fast, standard, deep)
        .map(Some)
        .map_err(|e| format!("could not construct AnthropicHttpClient: {e}"))
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
    resolver.register(Box::new(crate::secrets::KeychainResolver::new()));
    resolver
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
pub(crate) async fn spawn_mcp_listener(
    pool: sqlx::PgPool,
    owner: Owner,
) -> Result<
    (
        JoinHandle<Result<(), proxima_mcp_server::McpServerError>>,
        std::net::SocketAddr,
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

    proxima_mcp_substrate::migrator().run(&pool).await?;
    proxima_flavor_goal::migrator().run(&pool).await?;

    let mut registry = FlavorRegistry::new();
    proxima_mcp_substrate::register(&mut registry);
    proxima_flavor_goal::register(&mut registry);
    proxima_code::register(&mut registry);
    let frozen: Arc<FlavorRegistryFrozen> = Arc::new(registry.freeze());
    let server = DevMcpServer::from_pool(pool, owner, frozen);
    serve_streamable_http(bind, server, default_allowlist()).await
}

#[cfg(test)]
#[allow(unsafe_code)]
mod resolve_anthropic_client_tests {
    use super::*;
    use crate::config::{
        AppConfig, EmbeddingConfig, LlmConfig, LlmModelRecord, ModelRef, TierBindings,
    };
    use proxima_core::models::{Dialect, LlmCaps};

    fn anthropic_row(secret_ref: Option<&str>) -> LlmModelRecord {
        LlmModelRecord {
            vendor: "anthropic".into(),
            model_id: "claude-haiku-4-5".into(),
            dialect: Dialect::Anthropic,
            base_url: "https://api.anthropic.com".into(),
            caps: LlmCaps {
                tool_use: true,
                ..LlmCaps::none()
            },
            secret_ref: secret_ref.map(str::to_string),
        }
    }

    fn openai_row() -> LlmModelRecord {
        LlmModelRecord {
            vendor: "openai".into(),
            model_id: "gpt-4o-mini".into(),
            dialect: Dialect::OpenAI,
            base_url: "https://api.openai.com".into(),
            caps: LlmCaps::none(),
            secret_ref: Some("env:OPENAI_API_KEY".into()),
        }
    }

    #[test]
    fn empty_config_returns_none() {
        let cfg = AppConfig::default();
        assert!(matches!(resolve_anthropic_client(&cfg), Ok(None)));
    }

    #[test]
    fn only_openai_vendor_returns_none() {
        let cfg = AppConfig {
            llm: LlmConfig {
                models: vec![openai_row()],
            },
            ..AppConfig::default()
        };
        assert!(matches!(resolve_anthropic_client(&cfg), Ok(None)));
    }

    #[test]
    fn anthropic_present_but_no_anthropic_tier_returns_none() {
        let env_var = "PROXIMA_TEST_ANTH_KEY_NOTIER";
        unsafe { std::env::set_var(env_var, "sk-test") };
        let cfg = AppConfig {
            llm: LlmConfig {
                models: vec![anthropic_row(Some(&format!("env:{env_var}")))],
            },
            tiers: TierBindings::default(),
            embedding: EmbeddingConfig::default(),
        };
        let result = resolve_anthropic_client(&cfg);
        unsafe { std::env::remove_var(env_var) };
        assert!(matches!(result, Ok(None)));
    }

    #[test]
    fn anthropic_with_tier_and_secret_returns_some() {
        let env_var = "PROXIMA_TEST_ANTH_KEY_OK";
        unsafe { std::env::set_var(env_var, "sk-test") };
        let cfg = AppConfig {
            llm: LlmConfig {
                models: vec![anthropic_row(Some(&format!("env:{env_var}")))],
            },
            tiers: TierBindings {
                fast: Some(ModelRef {
                    vendor: "anthropic".into(),
                    model_id: "claude-haiku-4-5".into(),
                }),
                ..TierBindings::default()
            },
            embedding: EmbeddingConfig::default(),
        };
        let result = resolve_anthropic_client(&cfg);
        unsafe { std::env::remove_var(env_var) };
        let client = result.expect("ok").expect("some");
        assert_eq!(client.model_id_for(ModelTier::Fast), "claude-haiku-4-5");
        assert_eq!(client.model_id_for(ModelTier::Standard), "");
        assert_eq!(client.model_id_for(ModelTier::Deep), "");
    }

    #[test]
    fn anthropic_with_tier_but_missing_env_returns_err() {
        let cfg = AppConfig {
            llm: LlmConfig {
                models: vec![anthropic_row(Some("env:PROXIMA_TEST_ANTH_KEY_ABSENT"))],
            },
            tiers: TierBindings {
                fast: Some(ModelRef {
                    vendor: "anthropic".into(),
                    model_id: "claude-haiku-4-5".into(),
                }),
                ..TierBindings::default()
            },
            embedding: EmbeddingConfig::default(),
        };
        assert!(resolve_anthropic_client(&cfg).is_err());
    }
}

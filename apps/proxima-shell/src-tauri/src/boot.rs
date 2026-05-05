use std::sync::Arc;

use proxima_core::auth::NoAuth;
use proxima_core::models::Dialect;
use proxima_core::operators::{EmbeddingClient, F2AOperator, LlmClient};
use proxima_core::secrets::ResolverRegistry;
use proxima_core::{Engine, OrgId, Owner, Principal, UserId};
use proxima_llm_openai_compat::{
    OpenAiCompatConfig, OpenAiCompatEmbeddingClient, OpenAiCompatLlmClient,
};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

use crate::config::AppConfig;

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
        pg.start_outbox()
            .await
            .expect("failed to start outbox listener");

        let pg_for_settings = Arc::new(pg.clone());
        let auth = NoAuth::new(owner.principal.clone(), owner.clone());
        let engine = proxima_code::build_engine(pg, Box::new(auth))
            .with_operators(proxima_code::f2a_operator_registry());

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

    match resolve_consolidation_clients(&cfg) {
        Ok((llm, embed)) => {
            tracing::info!(
                llm_model = llm.model_id(),
                embed_model = embed.model_id(),
                embed_dim = embed.dim(),
                "F→A consolidation clients attached"
            );
            engine.with_llm(Arc::new(llm)).with_embed(Arc::new(embed))
        }
        Err(e) => {
            tracing::warn!("F→A consolidation disabled at boot: {e}");
            engine
        }
    }
}

pub(crate) fn resolve_consolidation_clients(
    cfg: &AppConfig,
) -> Result<(OpenAiCompatLlmClient, OpenAiCompatEmbeddingClient), String> {
    let tier = proxima_code::CommitSummaryOperator::new().tier();
    let model_ref = cfg
        .tiers
        .get(tier)
        .ok_or_else(|| format!("missing tier binding for {tier:?}"))?;
    let llm = cfg
        .llm
        .models
        .iter()
        .find(|m| m.vendor == model_ref.vendor && m.model_id == model_ref.model_id)
        .ok_or_else(|| format!("tier {tier:?} bound to unknown model {model_ref:?}"))?;
    if llm.dialect != Dialect::OpenAI {
        return Err(format!(
            "unsupported LLM dialect for {model_ref:?}: {:?}",
            llm.dialect
        ));
    }

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
    let llm_secret = resolve_optional_secret(&resolver, llm.secret_ref.as_deref())
        .map_err(|e| format!("could not resolve LLM secret_ref for {model_ref:?}: {e}"))?;
    let embed_secret = resolve_optional_secret(&resolver, embed.secret_ref.as_deref())
        .map_err(|e| format!("could not resolve embedding secret_ref for {active_ref:?}: {e}"))?;

    let llm_client = OpenAiCompatLlmClient::new(
        llm.model_id.clone(),
        OpenAiCompatConfig::new(llm.base_url.clone(), llm_secret),
    )
    .map_err(|e| {
        format!("could not construct OpenAI-compatible LLM client for {model_ref:?}: {e}")
    })?;
    let embed_dim = usize::try_from(embed.caps.dim)
        .map_err(|_| format!("embedding dim out of range: {}", embed.caps.dim))?;
    let embed_client = OpenAiCompatEmbeddingClient::new(
        embed.model_id.clone(),
        embed_dim,
        OpenAiCompatConfig::new(embed.base_url.clone(), embed_secret),
    )
    .map_err(|e| {
        format!("could not construct OpenAI-compatible embedding client for {active_ref:?}: {e}")
    })?;
    Ok((llm_client, embed_client))
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

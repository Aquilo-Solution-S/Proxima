//! Embedded engine wiring for the desktop shell.
//!
//! The shell holds an `Arc<Engine>` via `tauri::Builder::manage` and
//! exposes the engine command surfaces as `#[tauri::command]`
//! handlers. tauri-specta generates the matching TS bindings into
//! `packages/frontend-core/src/bindings.ts`; Rust command signatures
//! remain the source of truth (docs/09 §Schema-Driven UI + Codegen).
//!
//! v1 uses Postgres-backed storage (mandatory via `DATABASE_URL`)
//! with the proxima-code flavor. `NoopStorage` was removed once
//! settings persistence became required.

mod boot;
pub mod command_error;
mod commands;
pub mod config;
#[cfg(debug_assertions)]
mod dev_secrets;
mod mcp;
mod perf;
mod repo_ingest_hub;
pub mod secrets;

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Entry point for the Tauri application.
///
/// # Panics
///
/// Panics if the Tauri application fails to start (window creation,
/// plugin init, or context generation).
pub fn run() {
    // Load `.env` from the working directory if present. The shell's
    // working dir under `pnpm tauri:dev` is `apps/proxima-shell/`, so
    // `apps/proxima-shell/.env` (gitignored) is the standard location for
    // local DATABASE_URL etc. Production builds set env at the OS
    // layer; missing .env is silently fine.
    dotenvy::dotenv().ok();

    // Also load `~/.proxima/.env` as a shared user-level dotenv for
    // inference API keys (MISTRAL_API_KEY, OPENAI_API_KEY, …). dotenvy
    // does not override variables already present in the process env,
    // so precedence is: shell env > CWD `.env` > `~/.proxima/.env`.
    if let Some(home) = std::env::var_os("HOME") {
        let path = std::path::PathBuf::from(home).join(".proxima/.env");
        dotenvy::from_path(&path).ok();
    }

    // rmcp 1.6 logs idle-session keep-alive expiry and the resulting
    // session-cleanup race at ERROR; both are clean lifecycle events
    // (`quit_reason=Closed`). Pin those targets to `warn` until rmcp
    // upstream lowers them.
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new(
            "info,rmcp::transport::worker=warn,\
             rmcp::transport::streamable_http_server::tower=warn",
        )
    });

    let (chrome_layer, _chrome_guard) = match perf::session::dir() {
        Some(dir) => {
            let (layer, guard) = perf::chrome::layer(dir);
            (Some(layer), Some(guard))
        }
        None => (None, None),
    };

    tracing_subscriber::registry()
        .with(chrome_layer)
        .with(env_filter)
        .with(tracing_subscriber::fmt::layer())
        .try_init()
        .ok();

    if let Some(dir) = perf::session::dir() {
        tracing::info!(perf_session_dir = %dir.display(), "perf capture active");
    }

    let (engine, pg) = boot::build_engine();
    let wake_token_store = engine.wake_token_store();
    let mcp_auth_store =
        std::sync::Arc::new(proxima_mcp_server::McpAuthStore::new(wake_token_store));
    let owner = boot::sentinel_owner();
    match mcp::load_or_create_master_token(&owner) {
        Ok(token) => {
            tauri::async_runtime::block_on(
                mcp_auth_store.replace_local_master_token(token, owner.clone()),
            );
        }
        Err(err) => tracing::warn!("MCP master token unavailable at boot: {err}"),
    }
    let (mcp_listener, mcp_addr, mcp_tool_host) = tauri::async_runtime::block_on(async {
        boot::build_mcp_listener(pg.pool().clone(), owner, mcp_auth_store.clone()).await
    })
    .expect("failed to build Shell MCP listener");
    let engine = std::sync::Arc::new(
        engine
            .with_mcp_listener(mcp_listener)
            .with_mcp_listen_addr(mcp_addr),
    );
    tauri::async_runtime::block_on(engine.set_target_adapter(std::sync::Arc::new(
        proxima_harness::HarnessLoop::new(
            engine.clone(),
            std::sync::Arc::new(mcp_tool_host.with_engine(engine.clone()))
                as std::sync::Arc<dyn proxima_core::mcp::HarnessSubstrateBridge>,
        ),
    )));
    let engine_handle =
        tauri::async_runtime::block_on(engine.clone().start()).expect("failed to start engine");
    let mcp_url = engine
        .mcp_url()
        .expect("engine start with MCP listener must expose mcp_url");
    tracing::info!(url = %mcp_url, "Shell MCP listener up");
    let mcp_handle = Some(mcp::McpListenerHandle::new(engine_handle, mcp_addr));
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(engine)
        .manage(pg)
        .manage(mcp_auth_store)
        .manage(repo_ingest_hub::RepoIngestHub::new())
        .manage(mcp_handle)
        .invoke_handler(commands::specta_builder().invoke_handler())
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use crate::boot::{normalize_openai_compat_base_url, resolve_consolidation_clients};
    use crate::commands::specta_builder;
    use crate::config::{AppConfig, EmbeddingConfig, EmbeddingModelRecord, EmbeddingModelRef};
    use proxima_core::llm::EmbeddingClient;
    use proxima_core::models::EmbedCaps;

    /// Regenerate `packages/frontend-core/src/bindings.ts` from the
    /// command surface. Run via `cargo test -p proxima-shell`. The
    /// emitted file is git-tracked so JS-only contributors see the
    /// types without compiling Rust; CI compares the regen against
    /// the committed file to catch missed regenerations.
    #[test]
    fn export_ts_bindings() {
        specta_builder()
            .export(
                specta_typescript::Typescript::default(),
                "../../../packages/frontend-core/src/bindings.ts",
            )
            .expect("failed to export TS bindings");
    }

    fn embed_record(vendor: &str, model_id: &str, dim: u32) -> EmbeddingModelRecord {
        EmbeddingModelRecord {
            vendor: vendor.to_string(),
            model_id: model_id.to_string(),
            base_url: "http://localhost:11434/v1".to_string(),
            caps: EmbedCaps {
                dim,
                matryoshka: false,
            },
            secret_ref: None,
        }
    }

    fn config_with_models(embed_dim: u32) -> AppConfig {
        AppConfig {
            embedding: EmbeddingConfig {
                models: vec![embed_record("ollama", "nomic-embed-text", embed_dim)],
                active: Some(EmbeddingModelRef {
                    vendor: "ollama".to_string(),
                    model_id: "nomic-embed-text".to_string(),
                }),
            },
            ..AppConfig::default()
        }
    }

    #[test]
    fn model_resolution_builds_openai_compatible_clients() {
        let cfg = config_with_models(768);
        let embed = resolve_consolidation_clients(&cfg).expect("clients");
        assert_eq!(embed.model_id(), "nomic-embed-text");
        assert_eq!(embed.dim(), 768);
    }

    #[test]
    fn model_resolution_missing_active_embedding_degrades() {
        let mut cfg = config_with_models(768);
        cfg.embedding.active = None;
        let err = resolve_consolidation_clients(&cfg).unwrap_err();
        assert!(err.contains("missing active embedding model"));
    }

    #[test]
    fn model_resolution_uses_embedding_caps_dim() {
        let cfg = config_with_models(1_536);
        let embed = resolve_consolidation_clients(&cfg).expect("clients");
        assert_eq!(embed.dim(), 1_536);
    }

    #[test]
    fn ollama_openai_base_url_gets_v1_suffix() {
        assert_eq!(
            normalize_openai_compat_base_url("ollama", "http://localhost:11434"),
            "http://localhost:11434/v1"
        );
        assert_eq!(
            normalize_openai_compat_base_url("Google OLLAMA", "http://localhost:11434"),
            "http://localhost:11434/v1"
        );
        assert_eq!(
            normalize_openai_compat_base_url("QWEN Ollama", "http://localhost:11434"),
            "http://localhost:11434/v1"
        );
        assert_eq!(
            normalize_openai_compat_base_url("ollama", "http://localhost:11434/v1/"),
            "http://localhost:11434/v1"
        );
        assert_eq!(
            normalize_openai_compat_base_url("openai", "https://api.openai.com/v1/"),
            "https://api.openai.com/v1"
        );
    }
}

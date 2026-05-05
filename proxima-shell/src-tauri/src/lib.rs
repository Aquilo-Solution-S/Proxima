//! Embedded engine wiring for the desktop shell.
//!
//! The shell holds an `Arc<Engine>` via `tauri::Builder::manage` and
//! exposes the five verb surfaces from docs/14 as `#[tauri::command]`
//! handlers. tauri-specta generates the matching TS bindings into
//! `../src/lib/bindings.ts` on debug builds — Rust traits remain the
//! source of truth (docs/09 §Generation pipeline).
//!
//! v1 uses Postgres-backed storage (mandatory via `DATABASE_URL`)
//! with the proxima-code flavor. `NoopStorage` was removed once
//! settings persistence became required.

pub mod command_error;
pub mod config;
pub mod secrets;

use std::sync::Arc;

use futures_util::StreamExt;
use proxima_core::auth::{Credentials, NoAuth};
use proxima_core::error::ProtocolError;
use proxima_core::models::{LlmCaps, ModelTier};
use proxima_core::operators::F2AOperator;
use proxima_core::verbs::event_ingest::{EventDraft, EventIngestOutcome};
use proxima_core::verbs::goal_write::{GoalDraft, GoalWriteOutcome};
use proxima_core::verbs::query::{QueryRequest, QueryResponse};
use proxima_core::verbs::schema::{SchemaRequest, SchemaResponse};
use proxima_core::verbs::subscribe::SubscribeRequest;
use proxima_core::{ChangeEvent, Engine, OrgId, Owner, Principal, UserId};
use proxima_llm_ollama::{OllamaConfig, OllamaEmbeddingClient, OllamaLlmClient};
use proxima_storage_pg::PgStorage;
use tauri::ipc::Channel;
use tauri::State;
use tauri_specta::{Builder, collect_commands};
use uuid::Uuid;

use crate::command_error::CommandError;
use crate::config::{
    AppConfig, EmbeddingModelRecord, LlmModelRecord, ModelRef, TierBindings,
};

/// Build the embedded engine for v1 desktop.
///
/// Connects to Postgres (mandatory via `DATABASE_URL`), runs migrations,
/// starts the outbox listener, and wires the proxima-code flavor's
/// schemas via `proxima_code::build_engine`. Returns both the
/// `Arc<Engine>` (for verb handlers) and an `Arc<PgStorage>` clone
/// (for settings commands) so the Tauri command layer can hold both
/// independently.
///
/// # Panics
///
/// Panics if `DATABASE_URL` is not set — settings persistence is
/// required for the desktop shell.
fn build_engine() -> (Arc<Engine>, Arc<PgStorage>) {
    let owner = Owner {
        principal: Principal::User(UserId::new(Uuid::nil())),
        org_id: OrgId::new(Uuid::nil()),
    };

    let url = std::env::var("DATABASE_URL")
        .expect("DATABASE_URL must be set for the desktop shell — settings persistence is required");

    tauri::async_runtime::block_on(async {
        let pg = PgStorage::connect(&url)
            .await
            .expect("failed to connect to Postgres; check DATABASE_URL");
        pg.run_migrations()
            .await
            .expect("failed to run migrations");
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
fn sentinel_owner() -> Owner {
    Owner {
        principal: Principal::User(UserId::new(Uuid::nil())),
        org_id: OrgId::new(Uuid::nil()),
    }
}

/// Validate the loaded `AppConfig` at engine boot and attach local
/// Ollama clients when the registered rows are complete.
/// Loads settings from PG, assembles an `AppConfig`, and runs
/// `validate_config` against the engine. Failures are logged as
/// warnings only — the settings UI exists to fix broken config;
/// panicking would brick the app.
async fn wire_consolidation_clients(
    engine: Engine,
    pg: &Arc<PgStorage>,
    owner: &Owner,
) -> Engine {
    let cfg = match crate::config::load_app_config(pg, owner).await {
        Ok(cfg) => cfg,
        Err(e) => {
            tracing::warn!("could not load AppConfig at boot: {e}");
            return engine;
        }
    };
    if let Err(e) = crate::config::validate_config(&cfg, engine) {
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

fn resolve_consolidation_clients(
    cfg: &AppConfig,
) -> Result<(OllamaLlmClient, OllamaEmbeddingClient), String> {
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
    if !looks_like_ollama_base_url(&llm.base_url) {
        return Err(format!(
            "unsupported LLM provider shape for {model_ref:?}: base_url must point at an Ollama-compatible endpoint"
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
    if !looks_like_ollama_base_url(&embed.base_url) {
        return Err(format!(
            "unsupported embedding provider shape for {active_ref:?}: base_url must point at an Ollama-compatible endpoint"
        ));
    }

    let llm_client = OllamaLlmClient::new(
        llm.model_id.clone(),
        OllamaConfig {
            base_url: llm.base_url.clone(),
            ..OllamaConfig::default()
        },
    )
    .map_err(|e| format!("could not construct Ollama LLM client for {model_ref:?}: {e}"))?;
    let embed_dim = usize::try_from(embed.caps.dim)
        .map_err(|_| format!("embedding dim out of range: {}", embed.caps.dim))?;
    let embed_client = OllamaEmbeddingClient::new(
        embed.model_id.clone(),
        embed_dim,
        OllamaConfig {
            base_url: embed.base_url.clone(),
            ..OllamaConfig::default()
        },
    )
    .map_err(|e| {
        format!("could not construct Ollama embedding client for {active_ref:?}: {e}")
    })?;
    Ok((llm_client, embed_client))
}

fn looks_like_ollama_base_url(base_url: &str) -> bool {
    let lower = base_url.trim().to_ascii_lowercase();
    (lower.starts_with("http://") || lower.starts_with("https://"))
        && (lower.contains(":11434")
            || lower.contains("localhost")
            || lower.contains("127.0.0.1")
            || lower.contains("ollama"))
}

#[tauri::command]
#[specta::specta]
async fn schema(engine: State<'_, Arc<Engine>>) -> Result<SchemaResponse, ProtocolError> {
    Ok(engine.schema(&SchemaRequest))
}

#[tauri::command]
#[specta::specta]
async fn query(
    engine: State<'_, Arc<Engine>>,
    req: QueryRequest,
) -> Result<QueryResponse, ProtocolError> {
    engine.query(&Credentials::None, &req).await
}

#[tauri::command]
#[specta::specta]
async fn event_ingest(
    engine: State<'_, Arc<Engine>>,
    draft: EventDraft,
) -> Result<EventIngestOutcome, ProtocolError> {
    engine.event_ingest(&Credentials::None, draft).await
}

#[tauri::command]
#[specta::specta]
async fn goal_write(
    engine: State<'_, Arc<Engine>>,
    draft: GoalDraft,
) -> Result<GoalWriteOutcome, ProtocolError> {
    engine.write_goal(&Credentials::None, draft).await
}

/// Subscribe — engine returns a `Stream<Item = ChangeEvent>`; we
/// spawn a forwarder onto the caller-supplied `Channel<ChangeEvent>`
/// so events flow back through Tauri IPC. The handler returns when
/// the subscription is established; the stream lifetime is bound to
/// the spawned task and ends when storage closes its end (or the JS
/// side drops the channel, surfaced as a send error).
#[tauri::command]
#[specta::specta]
async fn subscribe(
    engine: State<'_, Arc<Engine>>,
    req: SubscribeRequest,
    on_event: Channel<ChangeEvent>,
) -> Result<(), ProtocolError> {
    let stream = engine.subscribe(&Credentials::None, req).await?;
    tokio::spawn(async move {
        let mut inbound = stream;
        while let Some(event) = inbound.next().await {
            if on_event.send(event).is_err() {
                break;
            }
        }
    });
    Ok(())
}

// ---------------------------------------------------------------
// Settings commands (m6.23) — DB-backed AppConfig surface for the
// Models settings panel (S1.g). Each handler pulls Arc<PgStorage>
// from Tauri state, calls the storage method, and maps errors via
// CommandError::from impls. tier_requires reads the engine's
// operator-union directly (no PG).
//
// Owner is a sentinel today (Uuid::nil) — multi-tenant deployments
// in v1.1+ wire owner from the auth context without changing the
// command shape.
// ---------------------------------------------------------------

/// # Errors
/// Returns `CommandError::Storage` on database failures.
#[tauri::command]
#[specta::specta]
async fn models_list_llm(
    pg: State<'_, Arc<PgStorage>>,
) -> Result<Vec<LlmModelRecord>, CommandError> {
    let owner = sentinel_owner();
    let rows = pg.list_llm_models(&owner).await?;
    Ok(rows.into_iter().map(LlmModelRecord::from).collect())
}

/// # Errors
/// Returns `CommandError::Storage` on database failures.
#[tauri::command]
#[specta::specta]
async fn models_list_embedding(
    pg: State<'_, Arc<PgStorage>>,
) -> Result<Vec<EmbeddingModelRecord>, CommandError> {
    let owner = sentinel_owner();
    let rows = pg.list_embedding_models(&owner).await?;
    Ok(rows.into_iter().map(EmbeddingModelRecord::from).collect())
}

/// # Errors
/// Returns `CommandError::Storage` on database failures.
#[tauri::command]
#[specta::specta]
async fn tier_bindings_get(
    pg: State<'_, Arc<PgStorage>>,
) -> Result<TierBindings, CommandError> {
    let owner = sentinel_owner();
    let raw = pg.list_tier_bindings(&owner).await?;
    let mut tb = TierBindings::default();
    for (tier, vendor, model_id) in raw {
        let r = ModelRef { vendor, model_id };
        match tier {
            ModelTier::Fast => tb.fast = Some(r),
            ModelTier::Standard => tb.standard = Some(r),
            ModelTier::Deep => tb.deep = Some(r),
        }
    }
    Ok(tb)
}

/// # Errors
/// Returns `CommandError::Storage` on database failures.
#[tauri::command]
#[specta::specta]
async fn embedding_active_get(
    pg: State<'_, Arc<PgStorage>>,
) -> Result<Option<ModelRef>, CommandError> {
    let owner = sentinel_owner();
    let pair = pg.get_embedding_active(&owner).await?;
    Ok(pair.map(|(vendor, model_id)| ModelRef { vendor, model_id }))
}

/// # Errors
/// Returns `CommandError::Storage` on database failures.
#[tauri::command]
#[specta::specta]
async fn tier_requires(
    engine: State<'_, Arc<Engine>>,
    tier: ModelTier,
) -> Result<LlmCaps, CommandError> {
    Ok(engine.tier_requires_union(tier))
}

/// # Errors
/// Returns `CommandError::DuplicateLlmModel` if the model already exists,
/// `CommandError::Storage` on database failures.
#[tauri::command]
#[specta::specta]
async fn models_register_llm(
    pg: State<'_, Arc<PgStorage>>,
    record: LlmModelRecord,
) -> Result<(), CommandError> {
    let owner = sentinel_owner();
    pg.register_llm_model(&owner, record.into())
        .await
        .map_err(CommandError::from)
}

/// # Errors
/// Returns `CommandError::DuplicateEmbeddingModel` if the model already exists,
/// `CommandError::Storage` on database failures.
#[tauri::command]
#[specta::specta]
async fn models_register_embedding(
    pg: State<'_, Arc<PgStorage>>,
    record: EmbeddingModelRecord,
) -> Result<(), CommandError> {
    let owner = sentinel_owner();
    pg.register_embedding_model(&owner, record.into())
        .await
        .map_err(CommandError::from)
}

/// # Errors
/// Returns `CommandError::Storage` on database failures.
#[tauri::command]
#[specta::specta]
async fn models_delete_llm(
    pg: State<'_, Arc<PgStorage>>,
    vendor: String,
    model_id: String,
) -> Result<bool, CommandError> {
    let owner = sentinel_owner();
    pg.delete_llm_model(&owner, &vendor, &model_id)
        .await
        .map_err(CommandError::from)
}

/// # Errors
/// Returns `CommandError::Storage` on database failures.
#[tauri::command]
#[specta::specta]
async fn models_delete_embedding(
    pg: State<'_, Arc<PgStorage>>,
    vendor: String,
    model_id: String,
) -> Result<bool, CommandError> {
    let owner = sentinel_owner();
    pg.delete_embedding_model(&owner, &vendor, &model_id)
        .await
        .map_err(CommandError::from)
}

/// # Errors
/// Returns `CommandError::InsufficientTierCaps` if the model's caps don't satisfy
/// the tier's operator-union requirements, `CommandError::UnknownLlmModel` if the
/// model is not registered, or `CommandError::Storage` on database failures.
#[tauri::command]
#[specta::specta]
async fn tier_bind(
    engine: State<'_, Arc<Engine>>,
    pg: State<'_, Arc<PgStorage>>,
    tier: ModelTier,
    vendor: String,
    model_id: String,
) -> Result<(), CommandError> {
    let owner = sentinel_owner();
    // Caps pre-check — refuse to bind if the model can't satisfy
    // the engine's operator-union for this tier.
    let llm_models = pg.list_llm_models(&owner).await?;
    let bound = llm_models.iter()
        .find(|m| m.vendor == vendor && m.model_id == model_id)
        .ok_or(CommandError::UnknownLlmModel {
            model_ref: ModelRef {
                vendor: vendor.clone(),
                model_id: model_id.clone(),
            },
        })?;
    let required = engine.tier_requires_union(tier);
    if !bound.caps.satisfies(&required) {
        return Err(CommandError::InsufficientTierCaps {
            tier,
            model_ref: ModelRef {
                vendor: vendor.clone(),
                model_id: model_id.clone(),
            },
        });
    }
    pg.bind_tier(&owner, tier, &vendor, &model_id)
        .await
        .map_err(CommandError::from)
}

/// # Errors
/// Returns `CommandError::Storage` on database failures.
#[tauri::command]
#[specta::specta]
async fn tier_unbind(
    pg: State<'_, Arc<PgStorage>>,
    tier: ModelTier,
) -> Result<bool, CommandError> {
    let owner = sentinel_owner();
    pg.unbind_tier(&owner, tier)
        .await
        .map_err(CommandError::from)
}

/// # Errors
/// Returns `CommandError::UnknownEmbeddingModel` if the model is not registered,
/// `CommandError::Storage` on database failures.
#[tauri::command]
#[specta::specta]
async fn embedding_active_set(
    pg: State<'_, Arc<PgStorage>>,
    vendor: String,
    model_id: String,
) -> Result<(), CommandError> {
    let owner = sentinel_owner();
    pg.set_embedding_active(&owner, &vendor, &model_id)
        .await
        .map_err(CommandError::from)
}

/// # Errors
/// Returns `CommandError::Storage` on database failures.
#[tauri::command]
#[specta::specta]
async fn embedding_active_clear(
    pg: State<'_, Arc<PgStorage>>,
) -> Result<bool, CommandError> {
    let owner = sentinel_owner();
    pg.clear_embedding_active(&owner)
        .await
        .map_err(CommandError::from)
}

// ---------------------------------------------------------------
// Repo registry commands (M6.S2) — DB-backed repo registry for
// LocalGitSource ingestion. Each handler pulls Arc<PgStorage> from
// Tauri state and uses the sentinel owner.
// ---------------------------------------------------------------

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct RepoRecordTs {
    pub repo_id: String,
    pub canonical_path: String,
    pub display_name: String,
    pub has_been_polled: bool,
    pub last_polled_at: Option<String>,
    pub created_at: String,
}

impl From<proxima_code::RepoRecord> for RepoRecordTs {
    fn from(r: proxima_code::RepoRecord) -> Self {
        Self {
            repo_id: r.repo_id.to_string(),
            canonical_path: r.canonical_path,
            display_name: r.display_name,
            has_been_polled: r.last_polled_at.is_some(),
            last_polled_at: r.last_polled_at.map(|t| t.to_string()),
            created_at: r.created_at.to_string(),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, specta::Type)]
pub struct IngestProgressTs {
    pub commit_index: u32,
    pub total_commits: u32,
    pub commit_sha: String,
    pub commits_emitted: u32,
    pub commits_replayed: u32,
    pub chunks_emitted: u32,
    pub chunks_reused: u32,
}

impl From<proxima_code::IngestProgress> for IngestProgressTs {
    fn from(p: proxima_code::IngestProgress) -> Self {
        Self {
            commit_index: u32::try_from(p.commit_index).unwrap_or(u32::MAX),
            total_commits: u32::try_from(p.total_commits).unwrap_or(u32::MAX),
            commit_sha: p.commit_sha,
            commits_emitted: u32::try_from(p.commits_emitted).unwrap_or(u32::MAX),
            commits_replayed: u32::try_from(p.commits_replayed).unwrap_or(u32::MAX),
            chunks_emitted: u32::try_from(p.chunks_emitted).unwrap_or(u32::MAX),
            chunks_reused: u32::try_from(p.chunks_reused).unwrap_or(u32::MAX),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, specta::Type)]
pub struct IndexReportTs {
    pub commits_emitted: u32,
    pub commits_replayed: u32,
    pub files_present_emitted: u32,
    pub files_tombstoned: u32,
    pub chunks_emitted: u32,
    pub chunks_reused: u32,
    pub chunks_tombstoned: u32,
}

impl From<proxima_code::IndexReport> for IndexReportTs {
    fn from(r: proxima_code::IndexReport) -> Self {
        Self {
            commits_emitted: u32::try_from(r.commits_emitted).unwrap_or(u32::MAX),
            commits_replayed: u32::try_from(r.commits_replayed).unwrap_or(u32::MAX),
            files_present_emitted: u32::try_from(r.files_present_emitted).unwrap_or(u32::MAX),
            files_tombstoned: u32::try_from(r.files_tombstoned).unwrap_or(u32::MAX),
            chunks_emitted: u32::try_from(r.chunks_emitted).unwrap_or(u32::MAX),
            chunks_reused: u32::try_from(r.chunks_reused).unwrap_or(u32::MAX),
            chunks_tombstoned: u32::try_from(r.chunks_tombstoned).unwrap_or(u32::MAX),
        }
    }
}

/// # Errors
/// Returns `CommandError::Storage` on database failures.
#[tauri::command]
#[specta::specta]
async fn repos_list(pg: State<'_, Arc<PgStorage>>) -> Result<Vec<RepoRecordTs>, CommandError> {
    let owner = sentinel_owner();
    let repos = proxima_code::list_repos(pg.pool(), &owner).await?;
    Ok(repos.into_iter().map(Into::into).collect())
}

/// # Errors
/// `InvalidRepoPath` if canonicalize fails, `NotAGitRepo` if `<path>/.git`
/// doesn't exist, `DuplicateRepo` on UNIQUE violation, `Storage` otherwise.
#[tauri::command]
#[specta::specta]
async fn repos_register(
    pg: State<'_, Arc<PgStorage>>,
    path: String,
    display_name: Option<String>,
) -> Result<RepoRecordTs, CommandError> {
    // 1. canonicalize
    let canonical = std::fs::canonicalize(&path).map_err(|io_err| CommandError::InvalidRepoPath {
        path: path.clone(),
        reason: io_err.to_string(),
    })?;

    // 2. Verify .git exists (directory or file for worktrees)
    let git_path = canonical.join(".git");
    if !git_path.exists() {
        return Err(CommandError::NotAGitRepo {
            path: canonical.to_string_lossy().into_owned(),
        });
    }

    // 3. Build display name
    let canonical_str = canonical.to_string_lossy().into_owned();
    let display = display_name.unwrap_or_else(|| {
        canonical.file_name().map_or_else(
            || canonical_str.clone(),
            |s| s.to_string_lossy().into_owned(),
        )
    });

    // 4. Register
    let owner = sentinel_owner();
    let repo_id = Uuid::now_v7();
    let record = proxima_code::register_repo(
        pg.pool(),
        &owner,
        repo_id,
        &canonical_str,
        &display,
    )
    .await?;

    Ok(record.into())
}

/// # Errors
/// `InvalidUuid` if `repo_id` doesn't parse, `Storage` otherwise.
#[tauri::command]
#[specta::specta]
async fn repos_delete(
    pg: State<'_, Arc<PgStorage>>,
    repo_id: String,
) -> Result<bool, CommandError> {
    let owner = sentinel_owner();
    let uuid = Uuid::parse_str(&repo_id).map_err(|_| CommandError::InvalidUuid { value: repo_id })?;
    proxima_code::delete_repo(pg.pool(), &owner, uuid)
        .await
        .map_err(CommandError::from)
}

/// Spawns a detached background task. Returns immediately. Per-commit
/// progress flows on `on_progress`; the final report flows on `on_done`.
/// Errors during the background ingest are logged via `tracing::warn` —
/// surface to UI is deferred (v1.1 adds an `on_error` channel).
///
/// # Errors
/// `UnknownRepo` if the `repo_id` isn't registered, `InvalidUuid` if the
/// id doesn't parse, `Storage` on lookup failures.
#[tauri::command]
#[specta::specta]
async fn repo_ingest(
    pg: State<'_, Arc<PgStorage>>,
    repo_id: String,
    on_progress: Channel<IngestProgressTs>,
    on_done: Channel<IndexReportTs>,
) -> Result<(), CommandError> {
    use proxima_core::Cursor;

    let owner = sentinel_owner();

    // 1. Parse uuid
    let uuid = Uuid::parse_str(&repo_id)
        .map_err(|_| CommandError::InvalidUuid { value: repo_id.clone() })?;

    // 2. Get repo record
    let record = proxima_code::get_repo(pg.pool(), &owner, uuid)
        .await?
        .ok_or(CommandError::UnknownRepo { repo_id })?;

    // 3. Build cursor from stored bytes
    let cursor = match record.last_cursor {
        Some(bytes) => Cursor::from_bytes(bytes),
        None => Cursor::from_bytes(Vec::new()),
    };

    // 4. Create source
    let source = proxima_code::LocalGitSource::new(
        uuid,
        std::path::PathBuf::from(record.canonical_path.clone()),
        owner.clone(),
    );

    // 5. Clone pg for the background task
    let pg_clone = pg.inner().clone();

    // 6. Spawn background task
    tokio::spawn(async move {
        let mut sink = |p: proxima_code::IngestProgress| {
            let _ = on_progress.send(p.into());
        };
        match source.run_poll(pg_clone.pool(), &cursor, &mut sink).await {
            Ok((report, new_cursor)) => {
                if let Err(e) = proxima_code::update_cursor(
                    pg_clone.pool(),
                    &owner,
                    uuid,
                    new_cursor.as_bytes(),
                    time::OffsetDateTime::now_utc(),
                )
                .await
                {
                    tracing::warn!("update_cursor failed: {e}");
                }
                let _ = on_done.send(report.into());
            }
            Err(e) => tracing::warn!("ingest run_poll failed: {e}"),
        }
    });

    Ok(())
}

fn specta_builder() -> Builder<tauri::Wry> {
    Builder::<tauri::Wry>::new().commands(collect_commands![
        // existing wire-protocol commands
        schema,
        query,
        event_ingest,
        goal_write,
        subscribe,
        // settings commands (m6.23)
        models_list_llm,
        models_list_embedding,
        models_register_llm,
        models_register_embedding,
        models_delete_llm,
        models_delete_embedding,
        tier_bindings_get,
        tier_bind,
        tier_unbind,
        tier_requires,
        embedding_active_get,
        embedding_active_set,
        embedding_active_clear,
        // repo registry commands (M6.S2)
        repos_list,
        repos_register,
        repos_delete,
        repo_ingest,
    ])
}

/// Entry point for the Tauri application.
///
/// # Panics
///
/// Panics if the Tauri application fails to start (window creation,
/// plugin init, or context generation).
pub fn run() {
    // Load `.env` from the working directory if present. The shell's
    // working dir under `pnpm tauri:dev` is `proxima-shell/`, so a
    // `proxima-shell/.env` (gitignored) is the standard location for
    // local DATABASE_URL etc. Production builds set env at the OS
    // layer; missing .env is silently fine.
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init()
        .ok();

    let (engine, pg) = build_engine();
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(engine)
        .manage(pg)
        .invoke_handler(specta_builder().invoke_handler())
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::specta_builder;

    /// Regenerate `proxima-shell/src/lib/bindings.ts` from the
    /// command surface. Run via `cargo test -p proxima-shell`. The
    /// emitted file is git-tracked so JS-only contributors see the
    /// types without compiling Rust; CI compares the regen against
    /// the committed file to catch missed regenerations.
    #[test]
    fn export_ts_bindings() {
        specta_builder()
            .export(
                specta_typescript::Typescript::default(),
                "../../frontend-core/src/bindings.ts",
            )
            .expect("failed to export TS bindings");
    }
}

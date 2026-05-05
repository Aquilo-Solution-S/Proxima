use std::sync::Arc;

use futures_util::StreamExt;
use proxima_core::auth::Credentials;
use proxima_core::error::ProtocolError;
use proxima_core::models::{LlmCaps, ModelTier};
use proxima_core::verbs::event_ingest::{EventDraft, EventIngestOutcome};
use proxima_core::verbs::goal_write::{GoalDraft, GoalWriteOutcome};
use proxima_core::verbs::query::{QueryRequest, QueryResponse};
use proxima_core::verbs::schema::{SchemaRequest, SchemaResponse};
use proxima_core::verbs::subscribe::SubscribeRequest;
use proxima_core::{ChangeEvent, Engine};
use proxima_storage_pg::PgStorage;
use tauri::State;
use tauri::ipc::Channel;
use tauri_specta::{Builder, collect_commands};
use uuid::Uuid;

use crate::boot::sentinel_owner;
use crate::command_error::CommandError;
use crate::config::{EmbeddingModelRecord, LlmModelRecord, ModelRef, TierBindings};

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
async fn tier_bindings_get(pg: State<'_, Arc<PgStorage>>) -> Result<TierBindings, CommandError> {
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
    let bound = llm_models
        .iter()
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
async fn tier_unbind(pg: State<'_, Arc<PgStorage>>, tier: ModelTier) -> Result<bool, CommandError> {
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
async fn embedding_active_clear(pg: State<'_, Arc<PgStorage>>) -> Result<bool, CommandError> {
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

#[derive(Debug, Clone, serde::Serialize, specta::Type)]
pub struct RepoEraseReceiptTs {
    pub repo_id: String,
    pub completed_at: String,
    pub facts_deleted: u64,
    pub abstractions_deleted: u64,
    pub edges_deleted: u64,
    pub embeddings_deleted: u64,
    pub events_deleted: u64,
    pub citation_mappings_deleted: u64,
    pub cited_objects_deleted: u64,
    pub source_batches_deleted: u64,
    pub f2a_rows_deleted: u64,
    pub repo_record_deleted: bool,
}

impl From<proxima_code::RepoEraseReceipt> for RepoEraseReceiptTs {
    fn from(r: proxima_code::RepoEraseReceipt) -> Self {
        Self {
            repo_id: r.repo_id.to_string(),
            completed_at: r.completed_at.to_string(),
            facts_deleted: r.facts_deleted,
            abstractions_deleted: r.abstractions_deleted,
            edges_deleted: r.edges_deleted,
            embeddings_deleted: r.embeddings_deleted,
            events_deleted: r.events_deleted,
            citation_mappings_deleted: r.citation_mappings_deleted,
            cited_objects_deleted: r.cited_objects_deleted,
            source_batches_deleted: r.source_batches_deleted,
            f2a_rows_deleted: r.f2a_rows_deleted,
            repo_record_deleted: r.repo_record_deleted,
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
    let canonical =
        std::fs::canonicalize(&path).map_err(|io_err| CommandError::InvalidRepoPath {
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
    let record =
        proxima_code::register_repo(pg.pool(), &owner, repo_id, &canonical_str, &display).await?;

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
    let uuid =
        Uuid::parse_str(&repo_id).map_err(|_| CommandError::InvalidUuid { value: repo_id })?;
    proxima_code::delete_repo(pg.pool(), &owner, uuid)
        .await
        .map_err(CommandError::from)
}

/// # Errors
/// `InvalidUuid` if `repo_id` doesn't parse, `UnknownRepo` if the repo
/// is not registered for the sentinel owner, `Storage` otherwise.
#[tauri::command]
#[specta::specta]
async fn repos_erase(
    pg: State<'_, Arc<PgStorage>>,
    repo_id: String,
) -> Result<RepoEraseReceiptTs, CommandError> {
    let owner = sentinel_owner();
    let uuid =
        Uuid::parse_str(&repo_id).map_err(|_| CommandError::InvalidUuid { value: repo_id })?;
    let receipt = proxima_code::erase_repo(pg.pool(), &owner, uuid).await?;
    Ok(receipt.into())
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
    engine: State<'_, Arc<Engine>>,
    pg: State<'_, Arc<PgStorage>>,
    repo_id: String,
    on_progress: Channel<IngestProgressTs>,
    on_done: Channel<IndexReportTs>,
) -> Result<(), CommandError> {
    use proxima_core::Cursor;

    let owner = sentinel_owner();

    // 1. Parse uuid
    let uuid = Uuid::parse_str(&repo_id).map_err(|_| CommandError::InvalidUuid {
        value: repo_id.clone(),
    })?;

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
    let engine_clone = engine.inner().clone();

    // 6. Spawn background task
    tokio::spawn(async move {
        let mut sink = |p: proxima_code::IngestProgress| {
            let _ = on_progress.send(p.into());
        };
        match source.run_poll(pg_clone.pool(), &cursor, &mut sink).await {
            Ok((report, new_cursor)) => {
                let cursor_updated = if let Err(e) = proxima_code::update_cursor(
                    pg_clone.pool(),
                    &owner,
                    uuid,
                    new_cursor.as_bytes(),
                    time::OffsetDateTime::now_utc(),
                )
                .await
                {
                    tracing::warn!("update_cursor failed: {e}");
                    false
                } else {
                    true
                };
                if cursor_updated && let Err(e) = engine_clone.run_pending_f2a(&owner).await {
                    tracing::warn!("repo_ingest F→A consolidation failed: {e}");
                }
                let _ = on_done.send(report.into());
            }
            Err(e) => tracing::warn!("ingest run_poll failed: {e}"),
        }
    });

    Ok(())
}

pub(crate) fn specta_builder() -> Builder<tauri::Wry> {
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
        repos_erase,
        repo_ingest,
    ])
}

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
use proxima_core::{ChangeEvent, Engine, Owner};
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
        use time::format_description::well_known::Rfc3339;
        Self {
            repo_id: r.repo_id.to_string(),
            canonical_path: r.canonical_path,
            display_name: r.display_name,
            has_been_polled: r.last_polled_at.is_some(),
            last_polled_at: r.last_polled_at.map(|t| {
                t.format(&Rfc3339)
                    .expect("OffsetDateTime always formats as RFC3339")
            }),
            created_at: r
                .created_at
                .format(&Rfc3339)
                .expect("OffsetDateTime always formats as RFC3339"),
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
#[serde(tag = "kind", content = "data", rename_all = "camelCase")]
pub enum RepoIngestEventTs {
    Progress(IngestProgressTs),
    Snapshot(RepoIngestionRunTs),
    Done(IndexReportTs),
    Error { message: String },
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
        use time::format_description::well_known::Rfc3339;
        Self {
            repo_id: r.repo_id.to_string(),
            completed_at: r
                .completed_at
                .format(&Rfc3339)
                .expect("OffsetDateTime always formats as RFC3339"),
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

#[derive(Debug, Clone, serde::Serialize, specta::Type)]
pub struct RepoIngestionRunTs {
    pub run_id: String,
    pub repo_id: String,
    pub status: proxima_code::RunStatus,
    pub stage: proxima_code::RunStage,
    pub commits_emitted: u32,
    pub files_emitted: u32,
    pub chunks_emitted: u32,
    pub chunks_reused: u32,
    pub chunks_tombstoned: u32,
    pub ast_edges_emitted: u32,
    pub abstractions_emitted: u32,
    pub embeddings_landed: u32,
    pub citations_emitted: u32,
    pub error_message: Option<String>,
    pub started_at: String,
    pub updated_at: String,
    pub finished_at: Option<String>,
}

impl From<proxima_code::RepoIngestionRun> for RepoIngestionRunTs {
    fn from(r: proxima_code::RepoIngestionRun) -> Self {
        use time::format_description::well_known::Rfc3339;
        Self {
            run_id: r.run_id.to_string(),
            repo_id: r.repo_id.to_string(),
            status: r.status,
            stage: r.stage,
            commits_emitted: r.commits_emitted,
            files_emitted: r.files_emitted,
            chunks_emitted: r.chunks_emitted,
            chunks_reused: r.chunks_reused,
            chunks_tombstoned: r.chunks_tombstoned,
            ast_edges_emitted: r.ast_edges_emitted,
            abstractions_emitted: r.abstractions_emitted,
            embeddings_landed: r.embeddings_landed,
            citations_emitted: r.citations_emitted,
            error_message: r.error_message,
            started_at: r
                .started_at
                .format(&Rfc3339)
                .expect("OffsetDateTime always formats as RFC3339"),
            updated_at: r
                .updated_at
                .format(&Rfc3339)
                .expect("OffsetDateTime always formats as RFC3339"),
            finished_at: r.finished_at.map(|t| {
                t.format(&Rfc3339)
                    .expect("OffsetDateTime always formats as RFC3339")
            }),
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

/// Persist or return the active ingestion run, then kick the driver.
///
/// # Errors
/// `UnknownRepo` if the repo is not registered; `InvalidUuid` if the id
/// does not parse; `Storage` on database failures.
#[tauri::command]
#[specta::specta]
async fn repo_ingest_start(
    engine: State<'_, Arc<Engine>>,
    pg: State<'_, Arc<PgStorage>>,
    hub: State<'_, crate::repo_ingest_hub::RepoIngestHub>,
    repo_id: String,
) -> Result<RepoIngestionRunTs, CommandError> {
    let owner = sentinel_owner();
    let uuid = Uuid::parse_str(&repo_id).map_err(|_| CommandError::InvalidUuid {
        value: repo_id.clone(),
    })?;
    let record = proxima_code::get_repo(pg.pool(), &owner, uuid)
        .await?
        .ok_or(CommandError::UnknownRepo { repo_id })?;

    let (run, created) = proxima_code::start_run_with_created(pg.pool(), &owner, uuid).await?;
    let cached = hub.snapshot(&owner, uuid).await.is_some();
    let should_spawn = (created || !cached)
        && run.status == proxima_code::RunStatus::Queued
        && run.stage == proxima_code::RunStage::Starting;
    hub.publish_snapshot(owner.clone(), run.clone()).await;

    if should_spawn {
        spawn_run_driver(
            engine.inner().clone(),
            pg.inner().clone(),
            hub.inner().clone(),
            owner,
            record,
            run.run_id,
        );
    }

    Ok(run.into())
}

/// Return the active ingestion run for a repo, if any.
///
/// # Errors
/// `InvalidUuid` if the id does not parse; `Storage` on database failures.
#[tauri::command]
#[specta::specta]
async fn repo_ingest_status(
    pg: State<'_, Arc<PgStorage>>,
    repo_id: String,
) -> Result<Option<RepoIngestionRunTs>, CommandError> {
    let owner = sentinel_owner();
    let uuid = Uuid::parse_str(&repo_id).map_err(|_| CommandError::InvalidUuid {
        value: repo_id.clone(),
    })?;
    let active = proxima_code::get_active_run(pg.pool(), &owner, uuid).await?;
    Ok(active.map(Into::into))
}

/// Subscribe to current run snapshot plus live events for a repo.
///
/// # Errors
/// `InvalidUuid` if the id does not parse; `Storage` on database failures.
#[tauri::command]
#[specta::specta]
async fn repo_ingest_subscribe(
    pg: State<'_, Arc<PgStorage>>,
    hub: State<'_, crate::repo_ingest_hub::RepoIngestHub>,
    repo_id: String,
    on_event: Channel<RepoIngestEventTs>,
) -> Result<(), CommandError> {
    let owner = sentinel_owner();
    let uuid = Uuid::parse_str(&repo_id).map_err(|_| CommandError::InvalidUuid {
        value: repo_id.clone(),
    })?;

    // Register the receiver before publishing the initial snapshot so a
    // terminal event from a short-lived run cannot fire in the gap
    // between snapshot read and subscribe — the prior split call shape
    // could leave the frontend stuck in `running` indefinitely.
    let (hub_snap, mut rx) = hub.subscribe(owner.clone(), uuid).await;
    let snap = match hub_snap {
        Some(s) => Some(s),
        None => proxima_code::get_active_run(pg.pool(), &owner, uuid).await?,
    };
    if let Some(run) = snap {
        let _ = on_event.send(RepoIngestEventTs::Snapshot(run.into()));
    }
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    if on_event.send(ev).is_err() {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn spawn_run_driver(
    engine: Arc<Engine>,
    pg: Arc<PgStorage>,
    hub: crate::repo_ingest_hub::RepoIngestHub,
    owner: Owner,
    record: proxima_code::RepoRecord,
    run_id: Uuid,
) {
    tokio::spawn(async move {
        let drive = async {
            let Some(run) = proxima_code::begin_run(pg.pool(), run_id)
                .await
                .map_err(|e| e.to_string())?
            else {
                return Ok::<(), String>(());
            };
            hub.publish_snapshot(owner.clone(), run).await;

            let cursor = match record.last_cursor.clone() {
                Some(b) => proxima_core::Cursor::from_bytes(b),
                None => proxima_core::Cursor::empty(),
            };
            let source = proxima_code::LocalGitSource::new(
                record.repo_id,
                std::path::PathBuf::from(record.canonical_path.clone()),
                owner.clone(),
            );

            let owner_for_progress = owner.clone();
            let hub_for_progress = hub.clone();
            let repo_id = record.repo_id;
            let mut sink = move |p: proxima_code::IngestProgress| {
                let owner = owner_for_progress.clone();
                let hub = hub_for_progress.clone();
                tokio::spawn(async move {
                    hub.publish_progress(owner, repo_id, IngestProgressTs::from(p))
                        .await;
                });
            };

            let (report, new_cursor) = source
                .run_poll(pg.pool(), &cursor, &mut sink)
                .await
                .map_err(|e| e.to_string())?;

            let mut counters = proxima_code::StageCounters::zeroed();
            counters.commits_emitted = u32::try_from(report.commits_emitted).unwrap_or(u32::MAX);
            counters.files_emitted =
                u32::try_from(report.files_present_emitted).unwrap_or(u32::MAX);
            counters.chunks_emitted = u32::try_from(report.chunks_emitted).unwrap_or(u32::MAX);
            counters.chunks_reused = u32::try_from(report.chunks_reused).unwrap_or(u32::MAX);
            counters.chunks_tombstoned =
                u32::try_from(report.chunks_tombstoned).unwrap_or(u32::MAX);

            let run = proxima_code::advance_stage(
                pg.pool(),
                run_id,
                proxima_code::RunStage::AstEdges,
                &counters,
            )
            .await
            .map_err(|e| e.to_string())?;
            hub.publish_snapshot(owner.clone(), run).await;

            counters.ast_edges_emitted = count_ast_edges_for_run(pg.pool(), &owner, record.repo_id)
                .await
                .map_err(|e| e.to_string())?;
            let run = proxima_code::advance_stage(
                pg.pool(),
                run_id,
                proxima_code::RunStage::F2a,
                &counters,
            )
            .await
            .map_err(|e| e.to_string())?;
            hub.publish_snapshot(owner.clone(), run).await;

            engine
                .run_pending_f2a(&owner)
                .await
                .map_err(|e| explain_driver_error("f2a", &e.to_string()))?;
            counters.abstractions_emitted =
                count_abstractions_for_run(pg.pool(), &owner, record.repo_id)
                    .await
                    .map_err(|e| e.to_string())?;
            let run = proxima_code::advance_stage(
                pg.pool(),
                run_id,
                proxima_code::RunStage::Embeddings,
                &counters,
            )
            .await
            .map_err(|e| e.to_string())?;
            hub.publish_snapshot(owner.clone(), run).await;

            counters.embeddings_landed = wait_for_embeddings(
                pg.pool(),
                &owner,
                record.repo_id,
                counters.abstractions_emitted,
                std::time::Duration::from_mins(1),
            )
            .await?;
            counters.citations_emitted = count_citations_for_run(pg.pool(), &owner, record.repo_id)
                .await
                .map_err(|e| e.to_string())?;

            proxima_code::update_cursor(
                pg.pool(),
                &owner,
                record.repo_id,
                new_cursor.as_bytes(),
                time::OffsetDateTime::now_utc(),
            )
            .await
            .map_err(|e| e.to_string())?;

            let run = proxima_code::mark_succeeded(pg.pool(), run_id, &counters)
                .await
                .map_err(|e| e.to_string())?;
            hub.publish_snapshot(owner.clone(), run).await;
            hub.publish_done(owner.clone(), record.repo_id, IndexReportTs::from(report))
                .await;
            Ok::<(), String>(())
        };

        if let Err(message) = drive.await {
            tracing::warn!("repo_ingest run {run_id} failed: {message}");
            if let Ok(run) = proxima_code::mark_failed(pg.pool(), run_id, &message).await {
                hub.publish_snapshot(owner.clone(), run).await;
            }
            hub.publish_error(owner, record.repo_id, message).await;
        }
    });
}

fn explain_driver_error(stage: &str, message: &str) -> String {
    if message.contains("HTTP send") && message.contains("timed out") {
        return format!(
            "{stage}: model request timed out. The model endpoint is reachable, \
             but the selected model did not respond before Proxima's timeout. \
             Use a faster model in Settings -> Models or retry after the model \
             is warm."
        );
    }
    if message.contains("localhost:11434") && message.contains("HTTP send") {
        return format!(
            "{stage}: Ollama is not reachable at http://localhost:11434. \
             Start Ollama or update Settings -> Models to a reachable \
             OpenAI-compatible endpoint, then run ingest again."
        );
    }
    if message.contains("chat/completions") && message.contains("HTTP send") {
        return format!(
            "{stage}: LLM endpoint is not reachable. Check Settings -> Models \
             base URL and network access, then run ingest again."
        );
    }
    if message.contains("/embeddings") && message.contains("HTTP send") {
        return format!(
            "{stage}: embedding endpoint is not reachable. Check Settings -> \
             Models embedding configuration, then run ingest again."
        );
    }
    format!("{stage}: {message}")
}

async fn count_ast_edges_for_run(
    pool: &sqlx::PgPool,
    owner: &Owner,
    repo_id: Uuid,
) -> Result<u32, sqlx::Error> {
    let (kind, principal_id, org_id) = proxima_code::repos::owner_columns_pub(owner);
    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint \
         FROM proxima_core.edges e \
         JOIN proxima_code.code_calls_v1 s ON s.edge_id = e.edge_id \
         JOIN proxima_code.code_chunk_v1 src ON src.memory_id = e.source_memory_id \
         JOIN proxima_code.code_chunk_v1 tgt ON tgt.memory_id = e.target_memory_id \
         WHERE e.owner_principal_kind = $1 AND e.owner_principal_id = $2 \
           AND e.owner_org_id = $3 AND src.repo_id = $4 AND tgt.repo_id = $4",
    )
    .bind(kind)
    .bind(principal_id)
    .bind(org_id)
    .bind(repo_id)
    .fetch_one(pool)
    .await?;
    Ok(u32::try_from(n).unwrap_or(u32::MAX))
}

async fn count_abstractions_for_run(
    pool: &sqlx::PgPool,
    owner: &Owner,
    repo_id: Uuid,
) -> Result<u32, sqlx::Error> {
    let (kind, principal_id, org_id) = proxima_code::repos::owner_columns_pub(owner);
    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint \
         FROM proxima_code.commit_summary_v1 cs \
         JOIN proxima_core.memories m ON m.memory_id = cs.memory_id \
         WHERE m.owner_principal_kind = $1 AND m.owner_principal_id = $2 \
           AND m.owner_org_id = $3 AND cs.repo_id = $4",
    )
    .bind(kind)
    .bind(principal_id)
    .bind(org_id)
    .bind(repo_id)
    .fetch_one(pool)
    .await?;
    Ok(u32::try_from(n).unwrap_or(u32::MAX))
}

async fn count_citations_for_run(
    pool: &sqlx::PgPool,
    owner: &Owner,
    repo_id: Uuid,
) -> Result<u32, sqlx::Error> {
    let (kind, principal_id, org_id) = proxima_code::repos::owner_columns_pub(owner);
    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint \
         FROM proxima_core.citation_mappings cm \
         JOIN proxima_core.memories m ON m.memory_id = cm.memory_id \
         WHERE m.owner_principal_kind = $1 AND m.owner_principal_id = $2 \
           AND m.owner_org_id = $3 AND ( \
             cm.memory_id IN (SELECT memory_id FROM proxima_code.commit_v1 WHERE repo_id = $4) OR \
             cm.memory_id IN (SELECT memory_id FROM proxima_code.file_revision_v1 WHERE repo_id = $4) OR \
             cm.memory_id IN (SELECT memory_id FROM proxima_code.code_chunk_v1 WHERE repo_id = $4))",
    )
    .bind(kind)
    .bind(principal_id)
    .bind(org_id)
    .bind(repo_id)
    .fetch_one(pool)
    .await?;
    Ok(u32::try_from(n).unwrap_or(u32::MAX))
}

async fn wait_for_embeddings(
    pool: &sqlx::PgPool,
    owner: &Owner,
    repo_id: Uuid,
    expected: u32,
    timeout: std::time::Duration,
) -> Result<u32, String> {
    if expected == 0 {
        return Ok(0);
    }
    let (kind, principal_id, org_id) = proxima_code::repos::owner_columns_pub(owner);
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint \
             FROM proxima_core.embeddings e \
             JOIN proxima_code.commit_summary_v1 cs ON cs.memory_id = e.entity_id \
             JOIN proxima_core.memories m ON m.memory_id = cs.memory_id \
             WHERE m.owner_principal_kind = $1 AND m.owner_principal_id = $2 \
               AND m.owner_org_id = $3 AND cs.repo_id = $4",
        )
        .bind(kind)
        .bind(principal_id)
        .bind(org_id)
        .bind(repo_id)
        .fetch_one(pool)
        .await
        .map_err(|e| e.to_string())?;
        let landed = u32::try_from(n).unwrap_or(u32::MAX);
        if landed >= expected {
            return Ok(landed);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "embeddings_timeout: expected={expected} got={landed}"
            ));
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
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
        repo_ingest_start,
        repo_ingest_status,
        repo_ingest_subscribe,
    ])
}

#[cfg(test)]
mod tests {
    #[test]
    fn explain_driver_error_names_unreachable_ollama() {
        let raw = "Internal: operator: LLM call failed: HTTP send: error sending request \
                   for url (http://localhost:11434/v1/chat/completions)";
        let msg = super::explain_driver_error("f2a", raw);
        assert!(msg.contains("Ollama is not reachable"));
        assert!(msg.contains("run ingest again"));
    }

    #[test]
    fn explain_driver_error_names_model_timeout() {
        let raw = "Internal: operator: LLM call failed: HTTP send: operation timed out";
        let msg = super::explain_driver_error("f2a", raw);
        assert!(msg.contains("model request timed out"));
        assert!(!msg.contains("not reachable"));
    }
}

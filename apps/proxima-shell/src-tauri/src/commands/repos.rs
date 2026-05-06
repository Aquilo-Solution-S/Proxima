use std::sync::Arc;

use proxima_core::Engine;
use proxima_storage_pg::PgStorage;
use tauri::State;
use tauri::ipc::Channel;
use uuid::Uuid;

use super::repo_ingest::spawn_run_driver;
use super::ts_types::{RepoEraseReceiptTs, RepoIngestEventTs, RepoIngestionRunTs, RepoRecordTs};
use crate::boot::sentinel_owner;
use crate::command_error::CommandError;

/// # Errors
/// Returns `CommandError::Storage` on database failures.
#[tauri::command]
#[specta::specta]
pub async fn repos_list(pg: State<'_, Arc<PgStorage>>) -> Result<Vec<RepoRecordTs>, CommandError> {
    crate::perf::ipc::record("repos_list", 0, async move {
        let owner = sentinel_owner();
        let repos = proxima_code::list_repos(pg.pool(), &owner).await?;
        Ok(repos.into_iter().map(Into::into).collect())
    })
    .await
}

/// # Errors
/// `InvalidRepoPath` if canonicalize fails, `NotAGitRepo` if `<path>/.git`
/// doesn't exist, `DuplicateRepo` on UNIQUE violation, `Storage` otherwise.
#[tauri::command]
#[specta::specta]
pub async fn repos_register(
    pg: State<'_, Arc<PgStorage>>,
    path: String,
    display_name: Option<String>,
) -> Result<RepoRecordTs, CommandError> {
    let req_bytes = crate::perf::ipc::req_size(&(&path, &display_name));
    crate::perf::ipc::record("repos_register", req_bytes, async move {
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
            proxima_code::register_repo(pg.pool(), &owner, repo_id, &canonical_str, &display)
                .await?;

        Ok(record.into())
    })
    .await
}

/// # Errors
/// `InvalidUuid` if `repo_id` doesn't parse, `Storage` otherwise.
#[tauri::command]
#[specta::specta]
pub async fn repos_delete(
    pg: State<'_, Arc<PgStorage>>,
    repo_id: String,
) -> Result<bool, CommandError> {
    let req_bytes = crate::perf::ipc::req_size(&repo_id);
    crate::perf::ipc::record("repos_delete", req_bytes, async move {
        let owner = sentinel_owner();
        let uuid =
            Uuid::parse_str(&repo_id).map_err(|_| CommandError::InvalidUuid { value: repo_id })?;
        proxima_code::delete_repo(pg.pool(), &owner, uuid)
            .await
            .map_err(CommandError::from)
    })
    .await
}

/// # Errors
/// `InvalidUuid` if `repo_id` doesn't parse, `UnknownRepo` if the repo
/// is not registered for the sentinel owner, `Storage` otherwise.
#[tauri::command]
#[specta::specta]
pub async fn repos_erase(
    pg: State<'_, Arc<PgStorage>>,
    repo_id: String,
) -> Result<RepoEraseReceiptTs, CommandError> {
    let req_bytes = crate::perf::ipc::req_size(&repo_id);
    crate::perf::ipc::record("repos_erase", req_bytes, async move {
        let owner = sentinel_owner();
        let uuid =
            Uuid::parse_str(&repo_id).map_err(|_| CommandError::InvalidUuid { value: repo_id })?;
        let receipt = proxima_code::erase_repo(pg.pool(), &owner, uuid).await?;
        Ok(receipt.into())
    })
    .await
}

/// Persist or return the active ingestion run, then kick the driver.
///
/// # Errors
/// `UnknownRepo` if the repo is not registered; `InvalidUuid` if the id
/// does not parse; `Storage` on database failures.
#[tauri::command]
#[specta::specta]
pub async fn repo_ingest_start(
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
pub async fn repo_ingest_status(
    pg: State<'_, Arc<PgStorage>>,
    repo_id: String,
) -> Result<Option<RepoIngestionRunTs>, CommandError> {
    let req_bytes = crate::perf::ipc::req_size(&repo_id);
    crate::perf::ipc::record("repo_ingest_status", req_bytes, async move {
        let owner = sentinel_owner();
        let uuid = Uuid::parse_str(&repo_id).map_err(|_| CommandError::InvalidUuid {
            value: repo_id.clone(),
        })?;
        let active = proxima_code::get_active_run(pg.pool(), &owner, uuid).await?;
        Ok(active.map(Into::into))
    })
    .await
}

/// Subscribe to current run snapshot plus live events for a repo.
///
/// # Errors
/// `InvalidUuid` if the id does not parse; `Storage` on database failures.
#[tauri::command]
#[specta::specta]
pub async fn repo_ingest_subscribe(
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

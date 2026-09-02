#![allow(dead_code)]

use super::records::{RepoIngestionRun, RepoRegistryError, RunStage, RunStatus, StageCounters};
use super::rows::RunRow;
use proxima_core::Owner;
use sqlx::PgPool;
use uuid::Uuid;

/// Create a queued run or return the active row for `(owner, repo_id)`.
///
/// # Errors
/// Returns `RepoRegistryError::Database` on database failures.
pub async fn start_run(
    pool: &PgPool,
    owner: &Owner,
    repo_id: Uuid,
) -> Result<RepoIngestionRun, RepoRegistryError> {
    let (run, _) = start_run_with_created(pool, owner, repo_id).await?;
    Ok(run)
}

/// Create a queued run or return the active row plus whether this call inserted.
///
/// The insert is the one writer here that can bring new repository-scoped
/// state into existence, so it is the one that takes the repository fence
/// (shared, in its own transaction, before the row lock the `runs_repo_fk`
/// foreign key implies). `runs_repo_fk` would refuse a run for a repository
/// erased before this transaction anyway; the fence is what makes the
/// refusal a refusal rather than a race, and puts this writer in the same
/// lane and the same order as every other repository-scoped write. The
/// stage/terminal updaters below need no fence: they only ever narrow an
/// existing row, and an erase cascades that row away rather than leaving it
/// to be updated.
///
/// # Errors
/// Returns `RepoRegistryError::NotFound` when the repository is not
/// registered for `owner`, `RepoRegistryError::Database` on database
/// failures.
pub async fn start_run_with_created(
    pool: &PgPool,
    owner: &Owner,
    repo_id: Uuid,
) -> Result<(RepoIngestionRun, bool), RepoRegistryError> {
    let (kind, principal_id) = owner.columns();
    let new_run_id = Uuid::now_v7();

    let mut tx = pool.begin().await?;
    super::fence::lock_repo_fence_shared_tx(&mut tx, owner, repo_id).await?;
    if !super::fence::repo_registered_tx(&mut tx, owner, repo_id).await? {
        return Err(RepoRegistryError::NotFound { repo_id });
    }

    let inserted = sqlx::query_as::<_, RunRow>(
        "INSERT INTO proxima_code.repo_ingestion_runs \
            (run_id, owner_kind, owner_id, \
             repo_id, status, stage) \
         VALUES ($1, $2, $3, $4, 'queued', 'starting') \
         ON CONFLICT (owner_kind, owner_id, repo_id) \
             WHERE status IN ('queued', 'running') \
         DO NOTHING \
         RETURNING run_id, repo_id, status, stage, \
                   commits_emitted, files_emitted, chunks_emitted, chunks_reused, \
                   chunks_tombstoned, ast_edges_emitted, abstractions_emitted, \
                   embeddings_landed, citations_emitted, \
                   error_message, started_at, updated_at, finished_at",
    )
    .bind(new_run_id)
    .bind(kind)
    .bind(principal_id)
    .bind(repo_id)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;

    if let Some(row) = inserted {
        return Ok((row.into(), true));
    }

    let run = get_active_run(pool, owner, repo_id)
        .await?
        .ok_or(RepoRegistryError::NotFound { repo_id })?;
    Ok((run, false))
}

/// Return the active queued/running run for `(owner, repo_id)`.
///
/// # Errors
/// Returns `RepoRegistryError::Database` on database failures.
pub async fn get_active_run(
    pool: &PgPool,
    owner: &Owner,
    repo_id: Uuid,
) -> Result<Option<RepoIngestionRun>, RepoRegistryError> {
    let (kind, principal_id) = owner.columns();
    let row = sqlx::query_as::<_, RunRow>(
        "SELECT run_id, repo_id, status, stage, \
                commits_emitted, files_emitted, chunks_emitted, chunks_reused, \
                chunks_tombstoned, ast_edges_emitted, abstractions_emitted, \
                embeddings_landed, citations_emitted, \
                error_message, started_at, updated_at, finished_at \
         FROM proxima_code.repo_ingestion_runs \
         WHERE owner_kind = $1 AND owner_id = $2 \
           AND repo_id = $3 \
           AND status IN ('queued', 'running') \
         ORDER BY started_at DESC \
         LIMIT 1",
    )
    .bind(kind)
    .bind(principal_id)
    .bind(repo_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(Into::into))
}

/// Return one ingestion run by id.
///
/// # Errors
/// Returns `RepoRegistryError::Database` on database failures.
pub async fn get_run(
    pool: &PgPool,
    run_id: Uuid,
) -> Result<Option<RepoIngestionRun>, RepoRegistryError> {
    let row = sqlx::query_as::<_, RunRow>(
        "SELECT run_id, repo_id, status, stage, \
                commits_emitted, files_emitted, chunks_emitted, chunks_reused, \
                chunks_tombstoned, ast_edges_emitted, abstractions_emitted, \
                embeddings_landed, citations_emitted, \
                error_message, started_at, updated_at, finished_at \
         FROM proxima_code.repo_ingestion_runs \
         WHERE run_id = $1",
    )
    .bind(run_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(Into::into))
}

/// Persist a stage boundary snapshot and return the updated row.
///
/// # Errors
/// Returns `RunNotFound`, `RunAlreadyTerminal`, or database errors.
pub async fn advance_stage(
    pool: &PgPool,
    run_id: Uuid,
    next_stage: RunStage,
    counters: &StageCounters,
) -> Result<RepoIngestionRun, RepoRegistryError> {
    let row = sqlx::query_as::<_, RunRow>(
        "UPDATE proxima_code.repo_ingestion_runs SET \
            status = 'running', stage = $2, \
            commits_emitted = $3, files_emitted = $4, chunks_emitted = $5, \
            chunks_reused = $6, chunks_tombstoned = $7, ast_edges_emitted = $8, \
            abstractions_emitted = $9, embeddings_landed = $10, citations_emitted = $11, \
            updated_at = now() \
          WHERE run_id = $1 AND status NOT IN ('succeeded', 'failed') \
          RETURNING run_id, repo_id, status, stage, \
                    commits_emitted, files_emitted, chunks_emitted, chunks_reused, \
                    chunks_tombstoned, ast_edges_emitted, abstractions_emitted, \
                    embeddings_landed, citations_emitted, \
                    error_message, started_at, updated_at, finished_at",
    )
    .bind(run_id)
    .bind(next_stage)
    .bind(i32_from_u32(counters.commits_emitted))
    .bind(i32_from_u32(counters.files_emitted))
    .bind(i32_from_u32(counters.chunks_emitted))
    .bind(i32_from_u32(counters.chunks_reused))
    .bind(i32_from_u32(counters.chunks_tombstoned))
    .bind(i32_from_u32(counters.ast_edges_emitted))
    .bind(i32_from_u32(counters.abstractions_emitted))
    .bind(i32_from_u32(counters.embeddings_landed))
    .bind(i32_from_u32(counters.citations_emitted))
    .fetch_optional(pool)
    .await?;

    if let Some(row) = row {
        Ok(row.into())
    } else {
        terminal_or_not_found(pool, run_id).await
    }
}

/// Atomically claim a queued run for the background driver.
///
/// Returns `None` when another driver already claimed the row.
///
/// # Errors
/// Returns `RepoRegistryError::Database` on database failures.
pub async fn begin_run(
    pool: &PgPool,
    run_id: Uuid,
) -> Result<Option<RepoIngestionRun>, RepoRegistryError> {
    let row = sqlx::query_as::<_, RunRow>(
        "UPDATE proxima_code.repo_ingestion_runs SET \
            status = 'running', stage = 'facts', updated_at = now() \
          WHERE run_id = $1 AND status = 'queued' AND stage = 'starting' \
          RETURNING run_id, repo_id, status, stage, \
                    commits_emitted, files_emitted, chunks_emitted, chunks_reused, \
                    chunks_tombstoned, ast_edges_emitted, abstractions_emitted, \
                    embeddings_landed, citations_emitted, \
                    error_message, started_at, updated_at, finished_at",
    )
    .bind(run_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(Into::into))
}

/// Mark a run succeeded and return the terminal snapshot.
///
/// # Errors
/// Returns `RunNotFound`, `RunAlreadyTerminal`, or database errors.
pub async fn mark_succeeded(
    pool: &PgPool,
    run_id: Uuid,
    counters: &StageCounters,
) -> Result<RepoIngestionRun, RepoRegistryError> {
    let row = sqlx::query_as::<_, RunRow>(
        "UPDATE proxima_code.repo_ingestion_runs SET \
            status = 'succeeded', stage = 'done', \
            commits_emitted = $2, files_emitted = $3, chunks_emitted = $4, \
            chunks_reused = $5, chunks_tombstoned = $6, ast_edges_emitted = $7, \
            abstractions_emitted = $8, embeddings_landed = $9, citations_emitted = $10, \
            updated_at = now(), finished_at = now() \
          WHERE run_id = $1 AND status NOT IN ('succeeded', 'failed') \
          RETURNING run_id, repo_id, status, stage, \
                    commits_emitted, files_emitted, chunks_emitted, chunks_reused, \
                    chunks_tombstoned, ast_edges_emitted, abstractions_emitted, \
                    embeddings_landed, citations_emitted, \
                    error_message, started_at, updated_at, finished_at",
    )
    .bind(run_id)
    .bind(i32_from_u32(counters.commits_emitted))
    .bind(i32_from_u32(counters.files_emitted))
    .bind(i32_from_u32(counters.chunks_emitted))
    .bind(i32_from_u32(counters.chunks_reused))
    .bind(i32_from_u32(counters.chunks_tombstoned))
    .bind(i32_from_u32(counters.ast_edges_emitted))
    .bind(i32_from_u32(counters.abstractions_emitted))
    .bind(i32_from_u32(counters.embeddings_landed))
    .bind(i32_from_u32(counters.citations_emitted))
    .fetch_optional(pool)
    .await?;

    if let Some(row) = row {
        Ok(row.into())
    } else {
        terminal_or_not_found(pool, run_id).await
    }
}

/// Mark every queued/running run as failed.
///
/// Intended to be called once at process boot under the single-writer
/// invariant: any active run in the DB belongs to a prior process whose
/// in-memory driver and event hub are gone, so the row is unreachable and
/// must be retired before it blocks new runs through the partial unique
/// index `repo_ingestion_runs_one_active`.
///
/// Returns the number of rows transitioned.
///
/// # Errors
/// Returns `RepoRegistryError::Database` on database failures.
pub async fn sweep_orphaned_runs(pool: &PgPool) -> Result<u64, RepoRegistryError> {
    let result = sqlx::query(
        "UPDATE proxima_code.repo_ingestion_runs SET \
            status = 'failed', \
            error_message = 'abandoned by process restart', \
            updated_at = now(), \
            finished_at = now() \
          WHERE status IN ('queued', 'running')",
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Mark a run failed and return the terminal snapshot.
///
/// # Errors
/// Returns `RunNotFound`, `RunAlreadyTerminal`, or database errors.
pub async fn mark_failed(
    pool: &PgPool,
    run_id: Uuid,
    error_message: &str,
) -> Result<RepoIngestionRun, RepoRegistryError> {
    let row = sqlx::query_as::<_, RunRow>(
        "UPDATE proxima_code.repo_ingestion_runs SET \
            status = 'failed', error_message = $2, updated_at = now(), finished_at = now() \
          WHERE run_id = $1 AND status NOT IN ('succeeded', 'failed') \
          RETURNING run_id, repo_id, status, stage, \
                    commits_emitted, files_emitted, chunks_emitted, chunks_reused, \
                    chunks_tombstoned, ast_edges_emitted, abstractions_emitted, \
                    embeddings_landed, citations_emitted, \
                    error_message, started_at, updated_at, finished_at",
    )
    .bind(run_id)
    .bind(error_message)
    .fetch_optional(pool)
    .await?;

    if let Some(row) = row {
        Ok(row.into())
    } else {
        terminal_or_not_found(pool, run_id).await
    }
}

async fn terminal_or_not_found(
    pool: &PgPool,
    run_id: Uuid,
) -> Result<RepoIngestionRun, RepoRegistryError> {
    match get_run(pool, run_id).await? {
        Some(run) if matches!(run.status, RunStatus::Succeeded | RunStatus::Failed) => {
            Err(RepoRegistryError::RunAlreadyTerminal {
                run_id,
                status: run.status,
            })
        }
        Some(run) => Ok(run),
        None => Err(RepoRegistryError::RunNotFound { run_id }),
    }
}

fn i32_from_u32(v: u32) -> i32 {
    i32::try_from(v).unwrap_or(i32::MAX)
}

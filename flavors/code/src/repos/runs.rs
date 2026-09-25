#![allow(dead_code)]

use super::records::{RepoIngestionRun, RepoRegistryError, RunStage, RunStatus, StageCounters};
use super::rows::RunRow;
use proxima_core::{Owner, OwnerScope};
use proxima_storage_pg::PgPlatformScope;
use proxima_storage_pg::begin_compatible_owner_transaction;
use sqlx::PgPool;
use uuid::Uuid;

/// How often a run's driver touches `updated_at` while it works.
pub const RUN_HEARTBEAT: std::time::Duration = std::time::Duration::from_secs(30);

/// A queued or running run untouched this long has no live driver — the
/// process driving it stopped — and is retired as failed the next time a
/// start or a read reaches it. Ten heartbeats, so a slow statement cannot
/// retire a live run.
pub const RUN_STALE_AFTER: std::time::Duration = std::time::Duration::from_mins(5);

pub(crate) const ABANDONED_RUN: &str =
    "abandoned: no heartbeat for 5 minutes; the process driving this run stopped";

/// Create a queued run or return the active row for `(owner, repo_id)`.
///
/// # Errors
/// Returns `RepoRegistryError::Database` on database failures.
pub async fn start_run(
    pool: &PgPool,
    owner_scope: Option<&OwnerScope>,
    owner: &Owner,
    repo_id: Uuid,
) -> Result<RepoIngestionRun, RepoRegistryError> {
    let (run, _) = start_run_with_created(pool, owner_scope, owner, repo_id).await?;
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
    owner_scope: Option<&OwnerScope>,
    owner: &Owner,
    repo_id: Uuid,
) -> Result<(RepoIngestionRun, bool), RepoRegistryError> {
    let (kind, principal_id) = owner.columns();
    let new_run_id = Uuid::now_v7();

    let mut tx = begin_compatible_owner_transaction(pool, owner_scope).await?;
    // Not a Memory admission — `repo_ingestion_runs` is a flavor state row
    // the Engine never sees — so this lane takes the declared scope fence
    // itself, shared, and asks the same liveness question under it.
    proxima::flavor::lock_scope_fence_shared_tx(&mut tx, super::CODE_REPO_SCOPE, owner, repo_id)
        .await?;
    if !super::fence::repo_registered_tx(&mut tx, owner, repo_id).await? {
        return Err(RepoRegistryError::NotFound { repo_id });
    }
    // A dead process's run would hold the one-active-run slot forever.
    retire_stale_runs_on(&mut tx, owner, repo_id).await?;

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
    if let Some(row) = inserted {
        tx.commit().await?;
        return Ok((row.into(), true));
    }

    let run = sqlx::query_as::<_, RunRow>(
        "SELECT run_id, repo_id, status, stage,
                commits_emitted, files_emitted, chunks_emitted, chunks_reused,
                chunks_tombstoned, ast_edges_emitted, abstractions_emitted,
                embeddings_landed, citations_emitted,
                error_message, started_at, updated_at, finished_at
           FROM proxima_code.repo_ingestion_runs
          WHERE owner_kind = $1 AND owner_id = $2 AND repo_id = $3
            AND status IN ('queued', 'running')
          ORDER BY started_at DESC
          LIMIT 1",
    )
    .bind(kind)
    .bind(principal_id)
    .bind(repo_id)
    .fetch_optional(&mut *tx)
    .await?
    .map(Into::into)
    .ok_or(RepoRegistryError::NotFound { repo_id })?;
    tx.commit().await?;
    Ok((run, false))
}

/// Return the active queued/running run for `(owner, repo_id)`.
///
/// # Errors
/// Returns `RepoRegistryError::Database` on database failures.
pub async fn get_active_run(
    pool: &PgPool,
    owner_scope: Option<&OwnerScope>,
    owner: &Owner,
    repo_id: Uuid,
) -> Result<Option<RepoIngestionRun>, RepoRegistryError> {
    let (kind, principal_id) = owner.columns();
    let mut tx = begin_compatible_owner_transaction(pool, owner_scope).await?;
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
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(row.map(Into::into))
}

/// Retire `owner`'s queued or running runs of `repo_id` whose driver has
/// not touched them for `RUN_STALE_AFTER` (5 min). Returns how many it retired.
///
/// # Errors
/// Returns `RepoRegistryError::Database` on database failures.
pub async fn retire_stale_runs(
    pool: &PgPool,
    owner_scope: Option<&OwnerScope>,
    owner: &Owner,
    repo_id: Uuid,
) -> Result<u64, RepoRegistryError> {
    let mut tx = begin_compatible_owner_transaction(pool, owner_scope).await?;
    let retired = retire_stale_runs_on(&mut tx, owner, repo_id).await?;
    tx.commit().await?;
    Ok(retired)
}

async fn retire_stale_runs_on(
    conn: &mut sqlx::PgConnection,
    owner: &Owner,
    repo_id: Uuid,
) -> Result<u64, sqlx::Error> {
    let (kind, principal_id) = owner.columns();
    let result = sqlx::query(
        "UPDATE proxima_code.repo_ingestion_runs SET \
            status = 'failed', error_message = $4, updated_at = now(), finished_at = now() \
          WHERE owner_kind = $1 AND owner_id = $2 AND repo_id = $3 \
            AND status IN ('queued', 'running') \
            AND updated_at < now() - make_interval(secs => $5)",
    )
    .bind(kind)
    .bind(principal_id)
    .bind(repo_id)
    .bind(ABANDONED_RUN)
    .bind(RUN_STALE_AFTER.as_secs_f64())
    .execute(conn)
    .await?;
    Ok(result.rows_affected())
}

/// The driver's heartbeat: touch a queued or running run's `updated_at`.
/// Returns `false` once the run is terminal (or gone), which tells the
/// driver nobody is waiting on this row any more.
///
/// # Errors
/// Returns `RepoRegistryError::Database` on database failures.
pub async fn touch_run(
    pool: &PgPool,
    owner_scope: Option<&OwnerScope>,
    run_id: Uuid,
) -> Result<bool, RepoRegistryError> {
    let mut tx = begin_compatible_owner_transaction(pool, owner_scope).await?;
    let result = sqlx::query(
        "UPDATE proxima_code.repo_ingestion_runs SET updated_at = now() \
          WHERE run_id = $1 AND status IN ('queued', 'running')",
    )
    .bind(run_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(result.rows_affected() > 0)
}

/// `owner`'s run `run_id`, or `None` when it is not theirs or not there.
///
/// # Errors
/// Returns `RepoRegistryError::Database` on database failures.
pub async fn get_owner_run(
    pool: &PgPool,
    owner_scope: Option<&OwnerScope>,
    owner: &Owner,
    run_id: Uuid,
) -> Result<Option<RepoIngestionRun>, RepoRegistryError> {
    let (kind, principal_id) = owner.columns();
    let mut tx = begin_compatible_owner_transaction(pool, owner_scope).await?;
    let row = sqlx::query_as::<_, RunRow>(
        "SELECT run_id, repo_id, status, stage, \
                commits_emitted, files_emitted, chunks_emitted, chunks_reused, \
                chunks_tombstoned, ast_edges_emitted, abstractions_emitted, \
                embeddings_landed, citations_emitted, \
                error_message, started_at, updated_at, finished_at \
         FROM proxima_code.repo_ingestion_runs \
         WHERE run_id = $1 AND owner_kind = $2 AND owner_id = $3",
    )
    .bind(run_id)
    .bind(kind)
    .bind(principal_id)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(row.map(Into::into))
}

/// `owner`'s most recent run of `repo_id`, whatever its status.
///
/// # Errors
/// Returns `RepoRegistryError::Database` on database failures.
pub async fn latest_run(
    pool: &PgPool,
    owner_scope: Option<&OwnerScope>,
    owner: &Owner,
    repo_id: Uuid,
) -> Result<Option<RepoIngestionRun>, RepoRegistryError> {
    let (kind, principal_id) = owner.columns();
    let mut tx = begin_compatible_owner_transaction(pool, owner_scope).await?;
    let row = sqlx::query_as::<_, RunRow>(
        "SELECT run_id, repo_id, status, stage, \
                commits_emitted, files_emitted, chunks_emitted, chunks_reused, \
                chunks_tombstoned, ast_edges_emitted, abstractions_emitted, \
                embeddings_landed, citations_emitted, \
                error_message, started_at, updated_at, finished_at \
         FROM proxima_code.repo_ingestion_runs \
         WHERE owner_kind = $1 AND owner_id = $2 AND repo_id = $3 \
         ORDER BY started_at DESC, run_id DESC \
         LIMIT 1",
    )
    .bind(kind)
    .bind(principal_id)
    .bind(repo_id)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(row.map(Into::into))
}

/// Return one ingestion run by id.
///
/// # Errors
/// Returns `RepoRegistryError::Database` on database failures.
pub async fn get_run(
    pool: &PgPool,
    owner_scope: Option<&OwnerScope>,
    run_id: Uuid,
) -> Result<Option<RepoIngestionRun>, RepoRegistryError> {
    let mut tx = begin_compatible_owner_transaction(pool, owner_scope).await?;
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
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(row.map(Into::into))
}

/// Persist a stage boundary snapshot and return the updated row.
///
/// # Errors
/// Returns `RunNotFound`, `RunAlreadyTerminal`, or database errors.
pub async fn advance_stage(
    pool: &PgPool,
    owner_scope: Option<&OwnerScope>,
    run_id: Uuid,
    next_stage: RunStage,
    counters: &StageCounters,
) -> Result<RepoIngestionRun, RepoRegistryError> {
    let mut tx = begin_compatible_owner_transaction(pool, owner_scope).await?;
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
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;

    if let Some(row) = row {
        Ok(row.into())
    } else {
        terminal_or_not_found(pool, owner_scope, run_id).await
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
    owner_scope: Option<&OwnerScope>,
    run_id: Uuid,
) -> Result<Option<RepoIngestionRun>, RepoRegistryError> {
    let mut tx = begin_compatible_owner_transaction(pool, owner_scope).await?;
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
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(row.map(Into::into))
}

/// Mark a run succeeded and return the terminal snapshot.
///
/// # Errors
/// Returns `RunNotFound`, `RunAlreadyTerminal`, or database errors.
pub async fn mark_succeeded(
    pool: &PgPool,
    owner_scope: Option<&OwnerScope>,
    run_id: Uuid,
    counters: &StageCounters,
) -> Result<RepoIngestionRun, RepoRegistryError> {
    let mut tx = begin_compatible_owner_transaction(pool, owner_scope).await?;
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
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;

    if let Some(row) = row {
        Ok(row.into())
    } else {
        terminal_or_not_found(pool, owner_scope, run_id).await
    }
}

/// Retire every queued or running run, of any owner, that no driver has
/// touched for `RUN_STALE_AFTER` (5 min): its process stopped. A live run is left
/// alone, so this is safe beside other processes driving runs.
///
/// Returns the number of rows transitioned.
///
/// # Errors
/// Returns `RepoRegistryError::Database` on database failures.
pub async fn sweep_orphaned_runs(pool: &PgPool) -> Result<u64, RepoRegistryError> {
    sweep_orphaned_runs_with_platform(pool, None).await
}

pub async fn sweep_orphaned_runs_with_platform(
    pool: &PgPool,
    platform: Option<&PgPlatformScope>,
) -> Result<u64, RepoRegistryError> {
    if let Some(platform) = platform {
        let mut tx = platform.begin().await.map_err(RepoRegistryError::Storage)?;
        // Stale by the driver heartbeat, not merely active: other processes
        // behind the same platform scope may be driving live runs.
        let result = sqlx::query(
            "UPDATE proxima_code.repo_ingestion_runs SET \
                status = 'failed', error_message = $1, updated_at = now(), finished_at = now() \
              WHERE status IN ('queued', 'running') \
                AND updated_at < now() - make_interval(secs => $2)",
        )
        .bind(ABANDONED_RUN)
        .bind(RUN_STALE_AFTER.as_secs_f64())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        return Ok(result.rows_affected());
    }
    let mut tx = proxima_storage_pg::begin_compatible_owner_transaction(pool, None)
        .await
        .map_err(RepoRegistryError::Storage)?;
    let result = sqlx::query(
        "UPDATE proxima_code.repo_ingestion_runs SET \
            status = 'failed', error_message = $1, updated_at = now(), finished_at = now() \
          WHERE status IN ('queued', 'running') \
            AND updated_at < now() - make_interval(secs => $2)",
    )
    .bind(ABANDONED_RUN)
    .bind(RUN_STALE_AFTER.as_secs_f64())
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(result.rows_affected())
}

/// Mark a run failed and return the terminal snapshot.
///
/// # Errors
/// Returns `RunNotFound`, `RunAlreadyTerminal`, or database errors.
pub async fn mark_failed(
    pool: &PgPool,
    owner_scope: Option<&OwnerScope>,
    run_id: Uuid,
    error_message: &str,
) -> Result<RepoIngestionRun, RepoRegistryError> {
    let mut tx = begin_compatible_owner_transaction(pool, owner_scope).await?;
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
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;

    if let Some(row) = row {
        Ok(row.into())
    } else {
        terminal_or_not_found(pool, owner_scope, run_id).await
    }
}

async fn terminal_or_not_found(
    pool: &PgPool,
    owner_scope: Option<&OwnerScope>,
    run_id: Uuid,
) -> Result<RepoIngestionRun, RepoRegistryError> {
    match get_run(pool, owner_scope, run_id).await? {
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

//! HEAD-snapshot ingest as a tracked run (issue #347).
//!
//! A `tools/call` that outlives the MCP session idle timeout loses its
//! result, and a large repository's ingest can. So an ingest is a row in
//! `repo_ingestion_runs`: `proxima-code_start_ingest_head_snapshot` returns
//! the run at once and drives it in the background, and
//! `proxima-code_get_ingest_run` reads it. The synchronous
//! `proxima-code_ingest_head_snapshot` drives the same kind of run inline,
//! so a caller whose response was lost can still read what happened.
//!
//! One active run per owner and repository (`repo_ingestion_runs_one_active`).
//! The driver touches its row every `RUN_HEARTBEAT` (30 s); a run untouched for
//! `RUN_STALE_AFTER` (5 min) lost its process, reads as failed, and is retired by
//! the next start.

use std::path::PathBuf;
use std::sync::Arc;

use proxima_core::{Cursor, Tool, ToolCtx, ToolError};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::repos::runs::{
    ABANDONED_RUN, RUN_HEARTBEAT, RUN_STALE_AFTER, begin_run, get_owner_run, latest_run,
    mark_failed, mark_succeeded, start_run_with_created, touch_run,
};
use crate::repos::{RepoIngestionRun, RepoRecord, RunStatus, StageCounters};
use crate::{CodeFlavorStore, IndexReport};

use super::code_store;
use super::repos::{EMBEDDING_BACKFILL_LIMIT, format_time, map_index_error, map_repo_registry};
use super::sql::resolve_repo_identifier;

/// One ingestion run as the tools report it.
#[derive(Debug, Serialize, JsonSchema)]
pub struct IngestRunItem {
    pub run_id: String,
    pub repo_handle: String,
    /// `queued`, `running`, `succeeded` or `failed`. A queued or running
    /// run whose driver stopped for five minutes reads `failed`.
    pub status: String,
    /// `starting`, `facts` while it runs, `done` once it succeeded.
    pub stage: String,
    /// Counters are written when the run succeeds; `0` until then.
    pub commits_emitted: u32,
    /// File-revision Facts written: present files and tombstones.
    pub files_emitted: u32,
    pub chunks_emitted: u32,
    pub chunks_reused: u32,
    pub chunks_tombstoned: u32,
    pub call_references_emitted: u32,
    pub error_message: Option<String>,
    pub started_at: String,
    pub updated_at: String,
    pub finished_at: Option<String>,
}

fn run_item(ctx: &ToolCtx, run: RepoIngestionRun) -> Result<IngestRunItem, ToolError> {
    use super::CodeToolCtxExt;

    let abandoned = matches!(run.status, RunStatus::Queued | RunStatus::Running)
        && time::OffsetDateTime::now_utc() - run.updated_at > RUN_STALE_AFTER;
    let (status, error_message) = if abandoned {
        (RunStatus::Failed, Some(ABANDONED_RUN.to_owned()))
    } else {
        (run.status, run.error_message)
    };
    Ok(IngestRunItem {
        run_id: run.run_id.to_string(),
        repo_handle: ctx.format_flavor_object(
            super::REPO_HANDLE_KIND,
            run.repo_id,
            super::REPO_HANDLE_PREFIX,
        ),
        status: status.as_str().to_owned(),
        stage: run.stage.as_str().to_owned(),
        commits_emitted: run.commits_emitted,
        files_emitted: run.files_emitted,
        chunks_emitted: run.chunks_emitted,
        chunks_reused: run.chunks_reused,
        chunks_tombstoned: run.chunks_tombstoned,
        call_references_emitted: run.ast_edges_emitted,
        error_message,
        started_at: format_time(run.started_at)?,
        updated_at: format_time(run.updated_at)?,
        finished_at: run.finished_at.map(format_time).transpose()?,
    })
}

/// What one HEAD-snapshot ingest produced.
pub(super) struct HeadSnapshotDone {
    pub(super) repo: RepoRecord,
    pub(super) head_commit_sha: String,
    pub(super) head_tree_sha: String,
    pub(super) report: IndexReport,
    pub(super) embeddings_enqueued: usize,
}

/// Start `owner`'s run of `repo_id`, or find the one already active.
/// Returns the run and whether this call created it.
pub(super) async fn start_run(
    ctx: &ToolCtx,
    repo_id: Uuid,
) -> Result<(RepoIngestionRun, bool), ToolError> {
    let pool = code_store(ctx)?;
    start_run_with_created(pool.pool(), pool.owner_scope(), &ctx.owner(), repo_id)
        .await
        .map_err(map_repo_registry)
}

/// Drive run `run_id` of `repo_id` to a terminal state: claim it,
/// heartbeat it, ingest, and record the outcome — counters on success, the
/// error on failure. The repository (and so the cursor the ingest starts
/// from) is read after the run holds the one active slot, so no other run
/// can move the cursor underneath it.
pub(super) async fn drive_run(
    ctx: &ToolCtx,
    repo_id: Uuid,
    run_id: Uuid,
) -> Result<HeadSnapshotDone, ToolError> {
    let pool = code_store(ctx)?;
    begin_run(pool.pool(), pool.owner_scope(), run_id)
        .await
        .map_err(map_repo_registry)?
        .ok_or_else(|| ToolError::Other(format!("ingestion run {run_id} is not queued")))?;
    let _heartbeat = RunHeartbeat::spawn(Arc::clone(&pool), run_id);
    let outcome = match crate::repos::get_repo(
        pool.pool(),
        pool.owner_scope(),
        &ctx.owner(),
        repo_id,
    )
    .await
    {
        Ok(Some(repo)) => ingest_head_snapshot(ctx, &pool, repo).await,
        Ok(None) => Err(ToolError::NotFound(format!("repo not found: {repo_id}"))),
        Err(err) => Err(map_repo_registry(err)),
    };
    match &outcome {
        Ok(done) => {
            mark_succeeded(
                pool.pool(),
                pool.owner_scope(),
                run_id,
                &stage_counters(&done.report),
            )
            .await
            .map_err(map_repo_registry)?;
        }
        Err(err) => {
            if let Err(mark) =
                mark_failed(pool.pool(), pool.owner_scope(), run_id, &err.to_string()).await
            {
                tracing::warn!(%run_id, error = %mark, "could not record a failed ingestion run");
            }
        }
    }
    outcome
}

async fn ingest_head_snapshot(
    ctx: &ToolCtx,
    pool: &CodeFlavorStore,
    repo: RepoRecord,
) -> Result<HeadSnapshotDone, ToolError> {
    let source = crate::LocalGitSource::new(
        repo.repo_id,
        PathBuf::from(repo.canonical_path.clone()),
        ctx.owner(),
    );
    let engine = super::engine(ctx)?;
    let ingest_ctx = crate::CodeIngestContext::new(&engine, ctx.authz(), pool);
    let prior = Cursor::from_bytes(repo.last_cursor.clone().unwrap_or_default());
    let outcome = source
        .run_head_snapshot(&ingest_ctx, &prior)
        .await
        .map_err(|err| map_index_error(&err))?;
    crate::repos::update_cursor(
        pool.pool(),
        pool.owner_scope(),
        &ctx.owner(),
        repo.repo_id,
        outcome.cursor.as_bytes(),
        time::OffsetDateTime::now_utc(),
    )
    .await
    .map_err(map_repo_registry)?;

    // Present chunks enqueue embedding_jobs in the derive txn when the
    // engine has a client. Backfill remains crash-residue for heads written
    // without a model; it is one anti-join, not a second flavor-table scan.
    let embeddings_enqueued = engine
        .backfill_missing_embeddings(ctx.authz(), &ctx.owner(), EMBEDDING_BACKFILL_LIMIT)
        .await
        .map_err(|err| ToolError::Other(err.to_string()))?;

    let repo = crate::repos::get_repo(pool.pool(), pool.owner_scope(), &ctx.owner(), repo.repo_id)
        .await
        .map_err(map_repo_registry)?
        .ok_or_else(|| ToolError::NotFound(format!("repo not found: {}", repo.repo_id)))?;
    Ok(HeadSnapshotDone {
        repo,
        head_commit_sha: outcome.head_sha,
        head_tree_sha: outcome.head_tree_sha,
        report: outcome.report,
        embeddings_enqueued,
    })
}

fn stage_counters(report: &IndexReport) -> StageCounters {
    let count = |value: usize| u32::try_from(value).unwrap_or(u32::MAX);
    StageCounters {
        commits_emitted: count(report.commits_emitted),
        files_emitted: count(
            report
                .files_present_emitted
                .saturating_add(report.files_tombstoned),
        ),
        chunks_emitted: count(report.chunks_emitted),
        chunks_reused: count(report.chunks_reused),
        chunks_tombstoned: count(report.chunks_tombstoned),
        ast_edges_emitted: count(report.call_references_emitted),
        ..StageCounters::zeroed()
    }
}

/// Touches the run every `RUN_HEARTBEAT` (30 s) while its driver lives; aborted
/// when the driver drops it.
struct RunHeartbeat(tokio::task::JoinHandle<()>);

impl RunHeartbeat {
    fn spawn(pool: Arc<CodeFlavorStore>, run_id: Uuid) -> Self {
        Self(tokio::spawn(async move {
            loop {
                tokio::time::sleep(RUN_HEARTBEAT).await;
                match touch_run(pool.pool(), pool.owner_scope(), run_id).await {
                    Ok(true) => {}
                    Ok(false) => return,
                    Err(err) => {
                        tracing::warn!(%run_id, error = %err, "ingestion run heartbeat failed");
                    }
                }
            }
        }))
    }
}

impl Drop for RunHeartbeat {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CodeStartIngestHeadSnapshotArgs {
    #[schemars(
        description = "Repo handle returned by proxima-code_register_repo or proxima-code_list_repos, for example `R:<uuid>`."
    )]
    pub repo_handle: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct CodeStartIngestHeadSnapshotOutput {
    pub run: IngestRunItem,
    /// `false` when a run of this repository was already active for the
    /// owner: `run` is that run, and no second one started.
    pub started: bool,
}

#[derive(Debug)]
pub struct CodeStartIngestHeadSnapshotTool;

impl Tool for CodeStartIngestHeadSnapshotTool {
    const NAME: &'static str = "proxima-code_start_ingest_head_snapshot";
    const DESCRIPTION: &'static str = "Start ingesting the current HEAD tree of one registered local Git repository in the background and return its ingestion run at once. Poll proxima-code_get_ingest_run until status is succeeded or failed. When a run of this repository is already active, returns that run instead of starting another.";
    const ANNOTATIONS: Option<proxima_core::mcp::McpToolAnnotations> =
        Some(super::WRITE_IDEMPOTENT);

    type Args = CodeStartIngestHeadSnapshotArgs;
    type Output = CodeStartIngestHeadSnapshotOutput;

    fn call(
        ctx: ToolCtx,
        args: CodeStartIngestHeadSnapshotArgs,
    ) -> futures::future::BoxFuture<'static, Result<CodeStartIngestHeadSnapshotOutput, ToolError>>
    {
        Box::pin(async move {
            let repo_id = resolve_repo_identifier(&ctx, &args.repo_handle).await?;
            let (run, started) = start_run(&ctx, repo_id).await?;
            let item = run_item(&ctx, run.clone())?;
            if started {
                let run_id = run.run_id;
                let driver = ctx.clone();
                tokio::spawn(async move {
                    if let Err(err) = drive_run(&driver, repo_id, run_id).await {
                        tracing::warn!(%run_id, error = %err, "background ingestion run failed");
                    }
                });
            }
            Ok(CodeStartIngestHeadSnapshotOutput { run: item, started })
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CodeGetIngestRunArgs {
    #[schemars(
        description = "Run id returned by proxima-code_start_ingest_head_snapshot or proxima-code_ingest_head_snapshot. Give this or repo_handle, not both."
    )]
    #[serde(default)]
    pub run_id: Option<String>,
    #[schemars(
        description = "Repo handle; reads that repository's most recent run. Give this or run_id, not both."
    )]
    #[serde(default)]
    pub repo_handle: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct CodeGetIngestRunOutput {
    pub run: IngestRunItem,
}

#[derive(Debug)]
pub struct CodeGetIngestRunTool;

impl Tool for CodeGetIngestRunTool {
    const NAME: &'static str = "proxima-code_get_ingest_run";
    const DESCRIPTION: &'static str = "Read one ingestion run: status (queued, running, succeeded, failed), stage, counters, error and timestamps. Name the run by run_id, or a repository by repo_handle for its most recent run.";
    const ANNOTATIONS: Option<proxima_core::mcp::McpToolAnnotations> = Some(super::READ_ONLY);

    type Args = CodeGetIngestRunArgs;
    type Output = CodeGetIngestRunOutput;

    fn call(
        ctx: ToolCtx,
        args: CodeGetIngestRunArgs,
    ) -> futures::future::BoxFuture<'static, Result<CodeGetIngestRunOutput, ToolError>> {
        Box::pin(async move {
            let pool = code_store(&ctx)?;
            let run = match (args.run_id.as_deref(), args.repo_handle.as_deref()) {
                (Some(run_id), None) => {
                    let run_id = Uuid::parse_str(run_id.trim()).map_err(|_| {
                        ToolError::InvalidInput(format!("run_id is not a UUID: {run_id:?}"))
                    })?;
                    get_owner_run(pool.pool(), pool.owner_scope(), &ctx.owner(), run_id)
                        .await
                        .map_err(map_repo_registry)?
                        .ok_or_else(|| {
                            ToolError::NotFound(format!("ingestion run not found: {run_id}"))
                        })?
                }
                (None, Some(repo_handle)) => {
                    let repo_id = resolve_repo_identifier(&ctx, repo_handle).await?;
                    latest_run(pool.pool(), pool.owner_scope(), &ctx.owner(), repo_id)
                        .await
                        .map_err(map_repo_registry)?
                        .ok_or_else(|| {
                            ToolError::NotFound(format!("no ingestion run for repo {repo_id}"))
                        })?
                }
                _ => {
                    return Err(ToolError::InvalidInput(
                        "give exactly one of run_id or repo_handle".into(),
                    ));
                }
            };
            Ok(CodeGetIngestRunOutput {
                run: run_item(&ctx, run)?,
            })
        })
    }
}

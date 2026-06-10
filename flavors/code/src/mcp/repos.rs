use std::path::PathBuf;

use proxima_core::mcp::{McpTool, McpToolCtx, McpToolError};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::IndexReport;
use crate::repos::{RepoRecord, RepoRegistryError};

use super::sql::{map_storage, resolve_repo_identifier};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CodeRegisterRepoArgs {
    #[schemars(
        description = "Absolute or relative local filesystem path to a Git repository. The path is canonicalized before registration."
    )]
    pub path: String,
    #[schemars(
        description = "Optional display name. Omit or null to use the repository directory name."
    )]
    pub display_name: Option<String>,
    #[schemars(
        description = "Optional target branch for workspace runs. Omit or null to use the current symbolic branch when available."
    )]
    pub target_branch: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CodeRegisterRepoOutput {
    pub repo: RepoItem,
    pub created: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CodeListReposArgs {}

#[derive(Debug, Serialize)]
pub struct CodeListReposOutput {
    pub repos: Vec<RepoItem>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CodeIngestHeadSnapshotArgs {
    #[schemars(
        description = "Repo handle returned by code_register_repo or code_list_repos, for example `R1`."
    )]
    pub repo_handle: String,
}

#[derive(Debug, Serialize)]
pub struct CodeIngestHeadSnapshotOutput {
    pub repo: RepoItem,
    pub head_commit_sha: String,
    pub head_tree_sha: String,
    pub report: IndexReportItem,
}

#[derive(Debug, Serialize)]
pub struct RepoItem {
    pub repo_handle: String,
    pub repo_id: String,
    pub canonical_path: String,
    pub display_name: String,
    pub target_branch: Option<String>,
    pub has_cursor: bool,
    pub last_polled_at: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Serialize)]
pub struct IndexReportItem {
    pub commits_emitted: usize,
    pub commits_replayed: usize,
    pub files_present_emitted: usize,
    pub files_tombstoned: usize,
    pub chunks_emitted: usize,
    pub chunks_reused: usize,
    pub chunks_tombstoned: usize,
}

#[derive(Debug)]
pub struct CodeRegisterRepoTool;

impl McpTool for CodeRegisterRepoTool {
    const NAME: &'static str = "proxima-code/code_register_repo";
    const DESCRIPTION: &'static str = "Register one local Git repository for the current owner. Returns a repo_handle for code MCP tools.";

    type Args = CodeRegisterRepoArgs;
    type Output = CodeRegisterRepoOutput;

    fn call(
        ctx: McpToolCtx,
        args: CodeRegisterRepoArgs,
    ) -> futures::future::BoxFuture<'static, Result<CodeRegisterRepoOutput, McpToolError>> {
        Box::pin(async move {
            let canonical = canonical_git_repo(&args.path)?;
            let canonical_path = canonical.to_string_lossy().into_owned();
            if let Some(existing) = repo_by_path(&ctx, &canonical_path).await? {
                let repo =
                    maybe_set_target_branch(&ctx, existing, args.target_branch.as_deref()).await?;
                return Ok(CodeRegisterRepoOutput {
                    repo: repo_item(&ctx, repo)?,
                    created: false,
                });
            }

            let display_name = args
                .display_name
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map_or_else(
                    || display_name_for_path(&canonical, &canonical_path),
                    ToOwned::to_owned,
                );
            let repo_id = Uuid::now_v7();
            let record = crate::register_repo(
                &ctx.pool,
                &ctx.owner,
                repo_id,
                &canonical_path,
                &display_name,
            )
            .await
            .map_err(map_repo_registry)?;
            let record =
                maybe_set_target_branch(&ctx, record, args.target_branch.as_deref()).await?;

            Ok(CodeRegisterRepoOutput {
                repo: repo_item(&ctx, record)?,
                created: true,
            })
        })
    }
}

#[derive(Debug)]
pub struct CodeIngestHeadSnapshotTool;

impl McpTool for CodeIngestHeadSnapshotTool {
    const NAME: &'static str = "proxima-code/code_ingest_head_snapshot";
    const DESCRIPTION: &'static str = "Ingest the current HEAD tree for one registered local Git repository and advance its cursor to HEAD. Does not walk commit history.";

    type Args = CodeIngestHeadSnapshotArgs;
    type Output = CodeIngestHeadSnapshotOutput;

    fn call(
        ctx: McpToolCtx,
        args: CodeIngestHeadSnapshotArgs,
    ) -> futures::future::BoxFuture<'static, Result<CodeIngestHeadSnapshotOutput, McpToolError>>
    {
        Box::pin(async move {
            let repo_id = resolve_repo_identifier(&ctx, &args.repo_handle).await?;
            let repo = crate::get_repo(&ctx.pool, &ctx.owner, repo_id)
                .await
                .map_err(map_repo_registry)?
                .ok_or_else(|| McpToolError::InvalidInput(format!("repo not found: {repo_id}")))?;

            let source = crate::LocalGitSource::new(
                repo.repo_id,
                PathBuf::from(repo.canonical_path.clone()),
                ctx.owner.clone(),
            );
            let outcome = source
                .run_head_snapshot(&ctx.pool)
                .await
                .map_err(|err| map_index_error(&err))?;
            crate::update_cursor(
                &ctx.pool,
                &ctx.owner,
                repo.repo_id,
                outcome.cursor.as_bytes(),
                time::OffsetDateTime::now_utc(),
            )
            .await
            .map_err(map_repo_registry)?;

            let repo = crate::get_repo(&ctx.pool, &ctx.owner, repo.repo_id)
                .await
                .map_err(map_repo_registry)?
                .ok_or_else(|| McpToolError::InvalidInput(format!("repo not found: {repo_id}")))?;

            Ok(CodeIngestHeadSnapshotOutput {
                repo: repo_item(&ctx, repo)?,
                head_commit_sha: outcome.head_sha,
                head_tree_sha: outcome.head_tree_sha,
                report: IndexReportItem::from(outcome.report),
            })
        })
    }
}

#[derive(Debug)]
pub struct CodeListReposTool;

impl McpTool for CodeListReposTool {
    const NAME: &'static str = "proxima-code/code_list_repos";
    const DESCRIPTION: &'static str =
        "List local Git repositories registered for the current owner.";

    type Args = CodeListReposArgs;
    type Output = CodeListReposOutput;

    fn call(
        ctx: McpToolCtx,
        _args: CodeListReposArgs,
    ) -> futures::future::BoxFuture<'static, Result<CodeListReposOutput, McpToolError>> {
        Box::pin(async move {
            let repos = crate::list_repos(&ctx.pool, &ctx.owner)
                .await
                .map_err(map_repo_registry)?
                .into_iter()
                .map(|record| repo_item(&ctx, record))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(CodeListReposOutput { repos })
        })
    }
}

fn canonical_git_repo(path: &str) -> Result<PathBuf, McpToolError> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err(McpToolError::InvalidInput("path required".into()));
    }
    let canonical = std::fs::canonicalize(trimmed).map_err(|err| {
        McpToolError::InvalidInput(format!("invalid repo path {trimmed:?}: {err}"))
    })?;
    if !canonical.join(".git").exists() {
        return Err(McpToolError::InvalidInput(format!(
            "not a Git repository: {}",
            canonical.to_string_lossy()
        )));
    }
    Ok(canonical)
}

fn display_name_for_path(canonical: &std::path::Path, canonical_path: &str) -> String {
    canonical.file_name().map_or_else(
        || canonical_path.to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

async fn repo_by_path(
    ctx: &McpToolCtx,
    canonical_path: &str,
) -> Result<Option<RepoRecord>, McpToolError> {
    let row = crate::list_repos(&ctx.pool, &ctx.owner)
        .await
        .map_err(map_repo_registry)?
        .into_iter()
        .find(|record| record.canonical_path == canonical_path);
    Ok(row)
}

async fn maybe_set_target_branch(
    ctx: &McpToolCtx,
    record: RepoRecord,
    target_branch: Option<&str>,
) -> Result<RepoRecord, McpToolError> {
    let Some(target_branch) = target_branch else {
        return Ok(record);
    };
    if target_branch.trim().is_empty() {
        return Ok(record);
    }
    crate::set_repo_target_branch(&ctx.pool, &ctx.owner, record.repo_id, Some(target_branch))
        .await
        .map_err(map_repo_registry)
}

fn repo_item(ctx: &McpToolCtx, record: RepoRecord) -> Result<RepoItem, McpToolError> {
    let last_polled_at = record.last_polled_at.map(format_time).transpose()?;
    Ok(RepoItem {
        repo_handle: ctx.format_flavor_object(
            super::REPO_HANDLE_KIND,
            record.repo_id,
            super::REPO_HANDLE_PREFIX,
        ),
        repo_id: record.repo_id.to_string(),
        canonical_path: record.canonical_path,
        display_name: record.display_name,
        target_branch: record.target_branch,
        has_cursor: record.last_cursor.is_some(),
        last_polled_at,
        created_at: format_time(record.created_at)?,
    })
}

fn format_time(value: time::OffsetDateTime) -> Result<String, McpToolError> {
    value
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|err| McpToolError::Other(format!("format time: {err}")))
}

impl From<IndexReport> for IndexReportItem {
    fn from(report: IndexReport) -> Self {
        Self {
            commits_emitted: report.commits_emitted,
            commits_replayed: report.commits_replayed,
            files_present_emitted: report.files_present_emitted,
            files_tombstoned: report.files_tombstoned,
            chunks_emitted: report.chunks_emitted,
            chunks_reused: report.chunks_reused,
            chunks_tombstoned: report.chunks_tombstoned,
        }
    }
}

fn map_index_error(error: &crate::IndexError) -> McpToolError {
    McpToolError::Other(error.to_string())
}

fn map_repo_registry(error: RepoRegistryError) -> McpToolError {
    match error {
        RepoRegistryError::DuplicatePath { canonical_path } => McpToolError::InvalidInput(format!(
            "repo already registered for owner: {canonical_path}"
        )),
        RepoRegistryError::NotFound { repo_id } => {
            McpToolError::InvalidInput(format!("repo not found: {repo_id}"))
        }
        RepoRegistryError::InvalidTargetBranch {
            repo_id,
            target_branch,
            reason,
        } => McpToolError::InvalidInput(format!(
            "invalid target branch for repo {repo_id}: {target_branch} ({reason})"
        )),
        RepoRegistryError::RunNotFound { run_id } => {
            McpToolError::InvalidInput(format!("ingestion run not found: {run_id}"))
        }
        RepoRegistryError::RunAlreadyTerminal { run_id, status } => McpToolError::InvalidInput(
            format!("ingestion run is already terminal: {run_id} ({status:?})"),
        ),
        RepoRegistryError::Database(error) => map_storage(error),
    }
}

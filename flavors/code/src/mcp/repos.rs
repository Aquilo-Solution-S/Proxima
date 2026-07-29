use std::path::PathBuf;

use proxima_core::mcp::cursor as wire_cursor;
use proxima_core::{Tool, ToolCtx, ToolError};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::IndexReport;
use crate::repos::{RepoRecord, RepoRegistryError};

use super::CodeToolCtxExt;
use super::code_store;
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
    #[serde(default)]
    #[schemars(
        description = "Gitignore-shaped globs limiting ingest to matching paths, for example `src/**` or `packages/*/src/**/*.ts`. `*` stops at a `/`; use `**` to cross directories. Omit or leave empty to consider every tracked file. At most 64 patterns."
    )]
    pub include_globs: Option<Vec<String>>,
    #[serde(default)]
    #[schemars(
        description = "Gitignore-shaped globs removing paths from ingest, for example `**/fixtures/**`. Beats include_globs where both match. Omit or leave empty to exclude nothing. At most 64 patterns."
    )]
    pub exclude_globs: Option<Vec<String>>,
}

impl CodeRegisterRepoArgs {
    /// The scope this call asks for, or `None` when it says nothing about
    /// scope at all.
    ///
    /// Omitting both lists on a re-registration leaves the stored scope
    /// alone; sending either one replaces both, so `exclude_globs: []` is
    /// how an operator clears a scope rather than an accident that
    /// silently keeps it.
    fn requested_scope(&self) -> Option<crate::repos::RepoScope> {
        match (&self.include_globs, &self.exclude_globs) {
            (None, None) => None,
            (include, exclude) => Some(crate::repos::RepoScope {
                include: include.clone().unwrap_or_default(),
                exclude: exclude.clone().unwrap_or_default(),
            }),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct CodeRegisterRepoOutput {
    pub repo: RepoItem,
    pub created: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CodeListReposArgs {
    #[schemars(
        description = "Max repos per page; values above 200 are clamped, 0 is rejected, default 50."
    )]
    #[serde(default)]
    pub limit: Option<u32>,
    #[schemars(
        description = "Opaque pagination cursor from a previous response's `next_cursor`. `limit` may vary between pages."
    )]
    #[serde(default)]
    pub cursor: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CodeListReposOutput {
    pub repos: Vec<RepoItem>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

/// Upper bound on embedding jobs enqueued by one ingest call. A HEAD
/// snapshot of a large repository can emit tens of thousands of memories;
/// this bounds the post-ingest enqueue to one generous pass, and the
/// startup reconcile plus `maintain-embeddings` pick up any remainder.
const EMBEDDING_BACKFILL_LIMIT: usize = 50_000;

const MAX_REPO_PAGE_LIMIT: u32 = 200;
const DEFAULT_REPO_PAGE_LIMIT: u32 = 50;

/// Opaque cursor codec: the shared `{v, fp, c}` envelope with the
/// `(created_at, repo_id)` keyset under `c`. The fingerprint binds the
/// calling owner (the listing has no other query shape).
const REPO_CURSOR: wire_cursor::FingerprintedCursor = wire_cursor::FingerprintedCursor {
    version: 1,
    source: "proxima-code_list_repos response",
    rebind_hint: "repeat the call as the same owner that produced it",
};

/// Keyset resume point carried inside the opaque repo cursor.
#[derive(Debug, Serialize, Deserialize)]
struct RepoCursorPos {
    created_at_nanos: i128,
    repo_id: Uuid,
}

fn repo_fingerprint(owner: &proxima_core::Owner) -> String {
    let canon = serde_json::to_string(&owner.external_key()).expect("fingerprint canon serializes");
    wire_cursor::fingerprint(&canon)
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CodeIngestHeadSnapshotArgs {
    #[schemars(
        description = "Repo handle returned by proxima-code_register_repo or proxima-code_list_repos, for example `R:<uuid>`."
    )]
    pub repo_handle: String,
}

#[derive(Debug, Serialize)]
pub struct CodeIngestHeadSnapshotOutput {
    pub repo: RepoItem,
    pub head_commit_sha: String,
    pub head_tree_sha: String,
    pub report: IndexReportItem,
    /// Embedding jobs enqueued for this owner after the ingest. `0` when no
    /// embedding client is configured (the deployment is lexical-only) or
    /// when every memory already had one.
    pub embeddings_enqueued: usize,
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
    /// Which paths ingest indexes. Both empty means every tracked file
    /// under the size cap. Always present, so a caller who wonders why a
    /// file is missing from search can see the scope that dropped it
    /// rather than having to guess.
    pub include_globs: Vec<String>,
    pub exclude_globs: Vec<String>,
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
    /// Tracked files this repo's `include_globs`/`exclude_globs` kept out
    /// of the ingest. Non-zero means the index deliberately does not hold
    /// them — check the repo's scope in proxima-code_list_repos before
    /// concluding a file is missing.
    pub files_excluded: usize,
}

#[derive(Debug)]
pub struct CodeRegisterRepoTool;

impl Tool for CodeRegisterRepoTool {
    const NAME: &'static str = "proxima-code_register_repo";
    const DESCRIPTION: &'static str = "Register one local Git repository for the current owner. Returns a repo_handle for code MCP tools.";

    type Args = CodeRegisterRepoArgs;
    type Output = CodeRegisterRepoOutput;

    fn call(
        ctx: ToolCtx,
        args: CodeRegisterRepoArgs,
    ) -> futures::future::BoxFuture<'static, Result<CodeRegisterRepoOutput, ToolError>> {
        Box::pin(async move {
            let canonical = canonical_git_repo(&args.path)?;
            let canonical_path = canonical.to_string_lossy().into_owned();
            let requested_scope = args.requested_scope();
            if let Some(existing) = repo_by_path(&ctx, &canonical_path).await? {
                let repo =
                    maybe_set_target_branch(&ctx, existing, args.target_branch.as_deref()).await?;
                // Unlike `display_name`, which is ignored on replay,
                // scope is authoritative when the caller states it: it
                // decides what gets indexed, and there is no other verb
                // that can change it once a repo exists.
                let repo = match requested_scope {
                    Some(scope) => {
                        let pool = code_store(&ctx)?;
                        crate::repos::set_repo_scope(
                            pool.pool(),
                            &ctx.owner(),
                            repo.repo_id,
                            &scope,
                        )
                        .await
                        .map_err(map_repo_registry)?
                    }
                    None => repo,
                };
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
            let pool = code_store(&ctx)?;
            let scope = requested_scope.unwrap_or_default();
            // Validated before the row exists, so a malformed glob is a
            // rejected registration rather than a repo whose every ingest
            // fails on a pattern nobody can now change.
            scope.compile().map_err(|source| {
                map_repo_registry(RepoRegistryError::InvalidScope { repo_id, source })
            })?;
            let record = crate::repos::register_repo(
                pool.pool(),
                &ctx.owner(),
                repo_id,
                &canonical_path,
                &display_name,
                &scope,
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

impl Tool for CodeIngestHeadSnapshotTool {
    const NAME: &'static str = "proxima-code_ingest_head_snapshot";
    const DESCRIPTION: &'static str = "Ingest the current HEAD tree for one registered local Git repository and advance its cursor to HEAD. Does not walk commit history.";

    type Args = CodeIngestHeadSnapshotArgs;
    type Output = CodeIngestHeadSnapshotOutput;

    fn call(
        ctx: ToolCtx,
        args: CodeIngestHeadSnapshotArgs,
    ) -> futures::future::BoxFuture<'static, Result<CodeIngestHeadSnapshotOutput, ToolError>> {
        Box::pin(async move {
            let repo_id = resolve_repo_identifier(&ctx, &args.repo_handle).await?;
            let pool = code_store(&ctx)?;
            let repo = crate::repos::get_repo(pool.pool(), &ctx.owner(), repo_id)
                .await
                .map_err(map_repo_registry)?
                .ok_or_else(|| ToolError::InvalidInput(format!("repo not found: {repo_id}")))?;

            let source = crate::LocalGitSource::new(
                repo.repo_id,
                PathBuf::from(repo.canonical_path.clone()),
                ctx.owner(),
            );
            let engine = super::engine(&ctx)?;
            let ingest_ctx = crate::CodeIngestContext::new(&engine, ctx.authz(), pool.as_ref());
            let outcome = source
                .run_head_snapshot(&ingest_ctx)
                .await
                .map_err(|err| map_index_error(&err))?;
            crate::repos::update_cursor(
                pool.pool(),
                &ctx.owner(),
                repo.repo_id,
                outcome.cursor.as_bytes(),
                time::OffsetDateTime::now_utc(),
            )
            .await
            .map_err(map_repo_registry)?;

            // Git ingest writes Facts and derived chunks directly through the
            // flavor's own sidecar path, which carries no embedding client and
            // so enqueues no embedding jobs. Without this, a freshly indexed
            // repository is lexically searchable and semantically invisible —
            // with nothing to indicate it, until someone happens to run
            // `maintain-embeddings`. The backfill is owner-scoped and
            // idempotent, so a re-poll that ingested nothing enqueues nothing.
            let embeddings_enqueued = engine
                .backfill_missing_embeddings(ctx.authz(), &ctx.owner(), EMBEDDING_BACKFILL_LIMIT)
                .await
                .map_err(|err| ToolError::Other(err.to_string()))?;

            let repo = crate::repos::get_repo(pool.pool(), &ctx.owner(), repo.repo_id)
                .await
                .map_err(map_repo_registry)?
                .ok_or_else(|| ToolError::InvalidInput(format!("repo not found: {repo_id}")))?;

            Ok(CodeIngestHeadSnapshotOutput {
                repo: repo_item(&ctx, repo)?,
                head_commit_sha: outcome.head_sha,
                head_tree_sha: outcome.head_tree_sha,
                report: IndexReportItem::from(outcome.report),
                embeddings_enqueued,
            })
        })
    }
}

#[derive(Debug)]
pub struct CodeListReposTool;

impl Tool for CodeListReposTool {
    const NAME: &'static str = "proxima-code_list_repos";
    const DESCRIPTION: &'static str =
        "List local Git repositories registered for the current owner.";

    type Args = CodeListReposArgs;
    type Output = CodeListReposOutput;

    fn call(
        ctx: ToolCtx,
        args: CodeListReposArgs,
    ) -> futures::future::BoxFuture<'static, Result<CodeListReposOutput, ToolError>> {
        Box::pin(async move {
            let pool = code_store(&ctx)?;
            proxima_core::reject_zero_limit(args.limit)?;
            let limit = args
                .limit
                .unwrap_or(DEFAULT_REPO_PAGE_LIMIT)
                .min(MAX_REPO_PAGE_LIMIT);
            let fingerprint = repo_fingerprint(&ctx.owner());
            let after = args
                .cursor
                .as_deref()
                .map(|raw| {
                    let pos: RepoCursorPos = REPO_CURSOR.decode(&fingerprint, raw)?;
                    let created_at =
                        time::OffsetDateTime::from_unix_timestamp_nanos(pos.created_at_nanos)
                            .map_err(|_| wire_cursor::malformed_cursor(REPO_CURSOR.source))?;
                    Ok::<_, ToolError>((created_at, pos.repo_id))
                })
                .transpose()?;
            let fetch = i64::from(limit).saturating_add(1);
            let mut records =
                crate::repos::list_repos_page(pool.pool(), &ctx.owner(), after, fetch)
                    .await
                    .map_err(map_repo_registry)?;
            let page_len = usize::try_from(limit).unwrap_or(usize::MAX);
            let has_more = records.len() > page_len;
            records.truncate(page_len);
            let next_cursor = (has_more && !records.is_empty()).then(|| {
                let last = records.last().expect("non-empty page");
                REPO_CURSOR.encode(
                    &fingerprint,
                    &RepoCursorPos {
                        created_at_nanos: last.created_at.unix_timestamp_nanos(),
                        repo_id: last.repo_id,
                    },
                )
            });
            let repos = records
                .into_iter()
                .map(|record| repo_item(&ctx, record))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(CodeListReposOutput {
                repos,
                next_cursor,
                has_more,
            })
        })
    }
}

fn canonical_git_repo(path: &str) -> Result<PathBuf, ToolError> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err(ToolError::InvalidInput("path required".into()));
    }
    let canonical = std::fs::canonicalize(trimmed)
        .map_err(|err| ToolError::InvalidInput(format!("invalid repo path {trimmed:?}: {err}")))?;
    if !canonical.join(".git").exists() {
        return Err(ToolError::InvalidInput(format!(
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
    ctx: &ToolCtx,
    canonical_path: &str,
) -> Result<Option<RepoRecord>, ToolError> {
    let pool = code_store(ctx)?;
    let row = crate::repos::list_repos(pool.pool(), &ctx.owner())
        .await
        .map_err(map_repo_registry)?
        .into_iter()
        .find(|record| record.canonical_path == canonical_path);
    Ok(row)
}

async fn maybe_set_target_branch(
    ctx: &ToolCtx,
    record: RepoRecord,
    target_branch: Option<&str>,
) -> Result<RepoRecord, ToolError> {
    let Some(target_branch) = target_branch else {
        return Ok(record);
    };
    if target_branch.trim().is_empty() {
        return Ok(record);
    }
    let pool = code_store(ctx)?;
    crate::repos::set_repo_target_branch(
        pool.pool(),
        &ctx.owner(),
        record.repo_id,
        Some(target_branch),
    )
    .await
    .map_err(map_repo_registry)
}

fn repo_item(ctx: &ToolCtx, record: RepoRecord) -> Result<RepoItem, ToolError> {
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
        include_globs: record.scope.include,
        exclude_globs: record.scope.exclude,
    })
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CodeEraseRepoArgs {
    #[schemars(
        description = "Repo handle returned by proxima-code_register_repo or proxima-code_list_repos, for example `R:<uuid>`."
    )]
    pub repo_handle: String,
    #[schemars(
        description = "The repo's canonical path, exactly as reported by proxima-code_list_repos. Required, and must match — erasure is irreversible, so the caller has to name what it is destroying."
    )]
    pub confirm_canonical_path: String,
}

#[derive(Debug, Serialize)]
pub struct CodeEraseRepoOutput {
    pub repo_id: String,
    pub canonical_path: String,
    pub completed_at: String,
    pub facts_deleted: u64,
    pub abstractions_deleted: u64,
    pub edges_deleted: u64,
    pub embeddings_deleted: u64,
    pub receipts_deleted: u64,
    pub citation_mappings_deleted: u64,
    pub cited_objects_deleted: u64,
    pub source_batches_deleted: u64,
    pub repo_record_deleted: bool,
}

/// Erase one registered repository and everything derived from it.
///
/// The storage verb behind this has existed and been tested since the code
/// flavor shipped, but was reachable only through `proxima_code::testkit`,
/// which is `cfg(debug_assertions)`. In a release build there was no way to
/// remove an indexed repository at all: `proxima-code_register_repo` upserts
/// and keeps the cursor, so a repository, once indexed, was permanent.
///
/// It is also the supported re-index path. A HEAD snapshot re-derives only
/// files whose content moved, and a derived Abstraction must carry the same
/// `source_batch_id` as the Facts it came from — so when the *deriver*
/// changes (a chunker or render upgrade), previously indexed files cannot be
/// re-derived in place. Erasing and re-ingesting produces fresh Facts in
/// fresh batches, which is the model working as intended rather than around.
#[derive(Debug)]
pub struct CodeEraseRepoTool;

impl Tool for CodeEraseRepoTool {
    const NAME: &'static str = "proxima-code_erase_repo";
    const DESCRIPTION: &'static str = "Erase one registered repository and every Fact, Abstraction, edge, embedding and receipt derived from it. Irreversible; requires the canonical path as confirmation. Also the supported way to re-index a repository from scratch after a Proxima upgrade changes chunking.";

    type Args = CodeEraseRepoArgs;
    type Output = CodeEraseRepoOutput;

    fn call(
        ctx: ToolCtx,
        args: CodeEraseRepoArgs,
    ) -> futures::future::BoxFuture<'static, Result<CodeEraseRepoOutput, ToolError>> {
        Box::pin(async move {
            let repo_id = resolve_repo_identifier(&ctx, &args.repo_handle).await?;
            let pool = code_store(&ctx)?;
            let repo = crate::repos::get_repo(pool.pool(), &ctx.owner(), repo_id)
                .await
                .map_err(map_repo_registry)?
                .ok_or_else(|| ToolError::InvalidInput(format!("repo not found: {repo_id}")))?;

            // Confirm against the stored path rather than the caller's, so a
            // handle typo cannot erase a different repository than the one
            // the caller believes it named.
            if args.confirm_canonical_path.trim() != repo.canonical_path {
                return Err(ToolError::InvalidInput(format!(
                    "confirm_canonical_path does not match repo {repo_id}: expected {}",
                    repo.canonical_path
                )));
            }

            let canonical_path = repo.canonical_path.clone();
            let receipt = crate::repos::erase_repo(pool.pool(), &ctx.owner(), repo_id)
                .await
                .map_err(map_repo_registry)?;

            Ok(CodeEraseRepoOutput {
                repo_id: receipt.repo_id.to_string(),
                canonical_path,
                completed_at: format_time(receipt.completed_at)?,
                facts_deleted: receipt.facts_deleted,
                abstractions_deleted: receipt.abstractions_deleted,
                edges_deleted: receipt.edges_deleted,
                embeddings_deleted: receipt.embeddings_deleted,
                receipts_deleted: receipt.receipts_deleted,
                citation_mappings_deleted: receipt.citation_mappings_deleted,
                cited_objects_deleted: receipt.cited_objects_deleted,
                source_batches_deleted: receipt.source_batches_deleted,
                repo_record_deleted: receipt.repo_record_deleted,
            })
        })
    }
}

fn format_time(value: time::OffsetDateTime) -> Result<String, ToolError> {
    value
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|err| ToolError::Other(format!("format time: {err}")))
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
            files_excluded: report.files_excluded,
        }
    }
}

fn map_index_error(error: &crate::IndexError) -> ToolError {
    ToolError::Other(error.to_string())
}

fn map_repo_registry(error: RepoRegistryError) -> ToolError {
    match error {
        RepoRegistryError::DuplicatePath { canonical_path } => ToolError::InvalidInput(format!(
            "repo already registered for owner: {canonical_path}"
        )),
        RepoRegistryError::NotFound { repo_id } => {
            ToolError::InvalidInput(format!("repo not found: {repo_id}"))
        }
        RepoRegistryError::InvalidTargetBranch {
            repo_id,
            target_branch,
            reason,
        } => ToolError::InvalidInput(format!(
            "invalid target branch for repo {repo_id}: {target_branch} ({reason})"
        )),
        RepoRegistryError::InvalidScope { repo_id, source } => {
            ToolError::InvalidInput(format!("invalid ingest scope for repo {repo_id}: {source}"))
        }
        RepoRegistryError::RunNotFound { run_id } => {
            ToolError::InvalidInput(format!("ingestion run not found: {run_id}"))
        }
        RepoRegistryError::RunAlreadyTerminal { run_id, status } => ToolError::InvalidInput(
            format!("ingestion run is already terminal: {run_id} ({status:?})"),
        ),
        RepoRegistryError::Database(error) => map_storage(error),
        RepoRegistryError::Storage(error) => ToolError::Other(error.to_string()),
    }
}

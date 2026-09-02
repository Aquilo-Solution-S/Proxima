//! `LocalGitSource` — pull-mode git ingest over a local repository.
//!
//! [`LocalGitSource::run_poll`] walks git since the supplied
//! [`proxima_core::Cursor`] and ingests **one commit at a time**.
//! Per doc 01 §"The contract", a commit is the natural
//! observational unit for git: one author's one logical change.
//! The poll itself is a delivery mechanism, not an observation —
//! its boundary is arbitrary cadence, while the commit is the
//! causal atom F→A consumes.
//!
//! Each commit contributes the `commit-v1` Fact plus
//! `file-revision-v1` Facts for that commit's tree diff against its
//! first parent (or the empty tree for root commits). Deterministic
//! chunk/call extraction is F→A operator work over those file Facts:
//! it emits `code-chunk-v1` code-slice Abstractions whose payload carries
//! the call sites; code slices carry provenance to file/commit Facts.
//! `indexed_commit_sha` is the commit's own sha, not HEAD.
//!
//! Cursor format (tagged binary bytes inside the opaque `Cursor` newtype):
//! ```ignore
//! b"PXC1" || opt_string(last_commit_sha) || opt_string(last_tree_sha)
//!         || opt_string(last_scope_hash)   // optional trailing field
//! ```
//! `None` for both shas means "from the beginning"; subsequent polls walk
//! only commits between `last_commit_sha` and `HEAD`. Missing
//! `last_scope_hash` means the snapshot cannot take the same-tree no-op.
//!
//! Typed sidecar inserts must run alongside Fact materialization, so this
//! surface is DB-aware rather than substrate-generic.
//!
//! Uses shell `git` via `std::process::Command`. The host must have
//! `git` on PATH. This trade-off keeps the dep surface minimal —
//! `gix` would more than double our build time.

mod git;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use proxima_core::{AuthzContext, Cursor, Engine, MemoryId, Owner, ToolError};
use sqlx::PgPool;
use uuid::Uuid;

use self::git::{CommitInfo, WalkPlan};

/// Blob bytes held in memory at once while ingesting a HEAD snapshot.
///
/// A batch always contains at least one entry, so a blob larger than this
/// still passes through whole — `MAX_BLOB_BYTES` (1 MiB) is the real per-file
/// ceiling. This only bounds how many are resident together.
const BLOB_BATCH_BYTES: u64 = 8 * 1024 * 1024;

use crate::calls::{ExtractedCall, ExtractedDefinition, extract_blob_callgraph};
use crate::chunker::{Chunk, chunk_blob};
use crate::ingest::{
    FileRevisionHead, IngestError, append_code_slices_with_handles, assign_code_chunk_handles,
    ingest_commit, ingest_file_revision,
};
use crate::payloads::{
    CodeCallSiteV1, CodeCallV1, CodeChunkV1, CommitV1, FileRevisionV1, FileState,
};
use crate::repos::ScopeMatcher;
use crate::store::CodeFlavorStore;

#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error("git: {0}")]
    Git(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("ingest: {0}")]
    Ingest(#[from] IngestError),
    #[error("invalid utf-8 in git output")]
    Utf8,
    #[error("cursor: {0}")]
    Cursor(String),
    /// The repo's stored ingest scope will not compile. Validated on
    /// write, so this means the row was edited outside the tool surface.
    /// Fails the ingest rather than silently indexing everything: a scope
    /// that cannot be applied is not the same as no scope.
    #[error("ingest scope: {0}")]
    Scope(#[from] crate::repos::ScopeError),
    /// The repository is not registered for this owner. Either it never
    /// was, or an erase of it committed while this ingest was running.
    /// Never a fallback: an ingest with no repository row has no scope to
    /// apply and nothing to file its rows under.
    #[error(transparent)]
    Repo(#[from] crate::repos::RepoRegistryError),
}

/// Counters returned by [`LocalGitSource::run_poll`]. Sums across
/// every commit the poll walked.
///
/// `chunks_reused` counts blob-analysis cache hits where deterministic
/// chunk/call extraction was reused for another path/commit. Cache hits
/// never suppress derived code-slice emission; they only avoid repeated
/// parsing over identical blob bytes.
#[derive(Debug, Default, Clone)]
pub struct IndexReport {
    pub commits_emitted: usize,
    pub commits_replayed: usize,
    pub files_present_emitted: usize,
    pub files_tombstoned: usize,
    pub chunks_emitted: usize,
    pub chunks_reused: usize,
    pub chunks_tombstoned: usize,
    /// Distinct caller→callee pairs the chunk payloads declared. One per
    /// callee, not one per call site: the index collapses multiplicity and
    /// this counts what the index gained.
    pub call_references_emitted: usize,
    /// Tracked blobs the repo's scope removed from this ingest. Reported
    /// rather than silent: a caller who cannot find a file in search
    /// should be able to see that scope, not a bug, is why.
    pub files_excluded: usize,
}

/// Per-commit progress event emitted between commit boundaries
/// during a poll. `commit_index` is 0-based; `total_commits` is the
/// pre-walked total for this poll.
#[derive(Debug, Clone)]
pub struct IngestProgress {
    pub commit_index: usize,
    pub total_commits: usize,
    pub commit_sha: String,
    // running totals from the IndexReport so far (post this commit)
    pub commits_emitted: usize,
    pub commits_replayed: usize,
    pub chunks_emitted: usize,
    pub chunks_reused: usize,
}

/// Result of a current-tree snapshot ingest.
#[derive(Debug, Clone)]
pub struct HeadSnapshotOutcome {
    pub report: IndexReport,
    pub cursor: Cursor,
    pub head_sha: String,
    pub head_tree_sha: String,
}

/// Authorized code-flavor ingest context. Public ingestion surfaces take this
/// instead of a raw pool; the store keeps the backend pool private to the
/// flavor and reads route through Engine authorization before use.
#[derive(Debug, Clone, Copy)]
pub struct CodeIngestContext<'a> {
    store: &'a CodeFlavorStore,
    engine: &'a Engine,
    authz: &'a AuthzContext,
}

impl<'a> CodeIngestContext<'a> {
    #[must_use]
    pub const fn new(
        engine: &'a Engine,
        authz: &'a AuthzContext,
        store: &'a CodeFlavorStore,
    ) -> Self {
        Self {
            store,
            engine,
            authz,
        }
    }

    fn pool(&self) -> &PgPool {
        self.store.pool()
    }

    fn engine(&self) -> &Engine {
        self.engine
    }

    fn authz(&self) -> &AuthzContext {
        self.authz
    }

    async fn file_revision_heads(
        &self,
        owner: Owner,
        repo_id: Uuid,
        file_paths: &[String],
    ) -> Result<Vec<FileRevisionHead>, IngestError> {
        // Owner-only `memory_head` of each named `(repo, path)` series —
        // the same series stateful-Fact NK ingest advances.
        self.store
            .owned_file_revision_heads(owner, repo_id, file_paths)
            .await
            .map_err(|err| read_error(&err))?
            .into_iter()
            .map(file_revision_head_from_row)
            .collect()
    }

    async fn present_file_revision_heads_except(
        &self,
        owner: Owner,
        repo_id: Uuid,
        keep_paths: &[String],
    ) -> Result<Vec<FileRevisionHead>, IngestError> {
        self.store
            .owned_present_file_revision_heads_except(owner, repo_id, keep_paths)
            .await
            .map_err(|err| read_error(&err))?
            .into_iter()
            .map(file_revision_head_from_row)
            .collect()
    }

    async fn chunk_series_heads(
        &self,
        owner: Owner,
        repo_id: Uuid,
        file_path: &str,
    ) -> Result<Vec<proxima_storage_pg::query::ChunkSeriesHead>, IngestError> {
        self.store
            .owned_chunk_series_heads(owner, repo_id, file_path)
            .await
            .map_err(|err| read_error(&err))
    }
}

fn read_error(err: &ToolError) -> IngestError {
    IngestError::Storage(format!("authorized code-flavor read: {err}"))
}

fn file_revision_head_from_row(
    row: proxima_storage_pg::query::FileRevisionHeadRow,
) -> Result<FileRevisionHead, IngestError> {
    let content_sha256: [u8; 32] = row.content_sha256.as_slice().try_into().map_err(|_| {
        IngestError::Storage(format!(
            "file revision sha256 length {}",
            row.content_sha256.len()
        ))
    })?;
    let state = match row.state.as_str() {
        "Present" => FileState::Present,
        "Tombstone" => FileState::Tombstone,
        other => {
            return Err(IngestError::Storage(format!("invalid file state {other}")));
        }
    };
    Ok(FileRevisionHead {
        memory_id: MemoryId::new(row.t),
        file_path: row.file_path,
        content_sha256,
        state,
    })
}

/// Pull-mode source. One instance per repo; `repo_id` is stable
/// across runs (provided by the caller, typically a CLI flag).
#[derive(Debug, Clone)]
pub struct LocalGitSource {
    pub repo_id: Uuid,
    pub repo_path: PathBuf,
    pub owner: Owner,
}

impl LocalGitSource {
    #[must_use]
    pub fn new(repo_id: Uuid, repo_path: PathBuf, owner: Owner) -> Self {
        Self {
            repo_id,
            repo_path,
            owner,
        }
    }

    /// The repo's stored ingest scope, compiled once per ingest.
    ///
    /// Loaded here rather than taken as a constructor argument on
    /// purpose: both ingest verbs must apply the same scope, and a
    /// snapshot that filtered while a poll did not would make the indexed
    /// set depend on which verb ran last. Reading it from the row is the
    /// only way a caller cannot forget.
    ///
    /// A repo row that has vanished REFUSES the ingest. It used to admit
    /// everything, on the reasoning that the ingest verbs resolve the repo
    /// themselves and report a missing one better than a scope lookup can.
    /// That reasoning holds only while the row cannot vanish underneath the
    /// verb, and it can: an erase of this repository commits between the
    /// verb's own lookup and this one, and the fallback then re-indexes a
    /// deleted repository under an allow-all scope — the widest possible
    /// scope, chosen precisely when there is no repository to scope. So the
    /// absence is typed, and the write paths that follow refuse it again
    /// under the repository fence.
    async fn load_scope(&self, pool: &sqlx::PgPool) -> Result<(ScopeMatcher, String), IndexError> {
        let record = crate::repos::get_repo(pool, &self.owner, self.repo_id)
            .await?
            .ok_or(crate::repos::RepoRegistryError::NotFound {
                repo_id: self.repo_id,
            })?;
        let fingerprint = record.scope.fingerprint();
        Ok((record.scope.compile()?, fingerprint))
    }

    /// Pure git walk under `cursor`. Pool-free; returns the commits
    /// to ingest plus head shas for cursor advance. Per-commit tree
    /// diffs are computed inside `run_poll` (one diff per commit).
    fn walk_git(&self, cursor: &CodeCursor) -> Result<WalkPlan, IndexError> {
        let head_sha = git::head_sha(&self.repo_path)?;
        let head_tree_sha = git::tree_sha(&self.repo_path, "HEAD")?;

        let commits = match cursor.last_commit_sha.as_deref() {
            Some(prev) if prev == head_sha => Vec::new(),
            Some(prev) => git::log_range(&self.repo_path, prev, "HEAD")?,
            None => git::log(&self.repo_path)?,
        };

        Ok(WalkPlan {
            head_sha,
            head_tree_sha,
            commits,
        })
    }

    /// DB-aware ingest. Walks each commit since the cursor, emits the
    /// commit Fact plus the file-revision Facts from that commit's tree
    /// diff, then derives code-slice Abstractions from those Facts. F→A
    /// consumes one commit's worth of causally-coherent Facts.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError`] when git cannot be walked, when a Fact fails
    /// to ingest, or when the cursor cannot be encoded.
    pub async fn run_poll(
        &self,
        ctx: &CodeIngestContext<'_>,
        cursor: &Cursor,
        progress: &mut impl FnMut(IngestProgress),
    ) -> Result<(IndexReport, Cursor), IndexError> {
        self.run_poll_limited(ctx, cursor, None, progress).await
    }

    /// DB-aware ingest with an optional commit cap. `None` ingests all
    /// commits since the cursor; `Some(n)` ingests at most `n` oldest
    /// pending commits and returns a cursor at the last ingested commit.
    ///
    /// # Errors
    /// Returns git, cursor, or typed ingest errors.
    pub async fn run_poll_limited(
        &self,
        ctx: &CodeIngestContext<'_>,
        cursor: &Cursor,
        max_commits: Option<usize>,
        progress: &mut impl FnMut(IngestProgress),
    ) -> Result<(IndexReport, Cursor), IndexError> {
        let parsed = decode_cursor(cursor)?;
        let plan = self.walk_git(&parsed)?;
        let (scope, scope_hash) = self.load_scope(ctx.pool()).await?;
        let mut report = IndexReport::default();
        let commit_limit = max_commits.unwrap_or(usize::MAX);
        let selected_total = plan.commits.len().min(commit_limit);

        // Per-poll cache: reuse deterministic chunk/call extraction
        // for identical blob bytes. This is a parse-work cache only;
        // every changed path/commit still emits derived code-slice
        // projection rows tied to its own file-revision Fact.
        let mut blob_analysis_cache: HashMap<BlobAnalysisKey, BlobAnalysis> = HashMap::new();

        // `git::log` returns newest-first; process oldest-first so each
        // commit's tree diff against its first parent reflects the
        // historical order, and the NK head advances monotonically.
        let mut last_ingested_sha: Option<String> = None;
        for (i, commit_info) in plan.commits.iter().rev().take(selected_total).enumerate() {
            self.ingest_one_commit(
                ctx,
                commit_info,
                &scope,
                &mut report,
                &mut blob_analysis_cache,
            )
            .await?;
            last_ingested_sha = Some(commit_info.sha.clone());
            progress(IngestProgress {
                commit_index: i,
                total_commits: selected_total,
                commit_sha: commit_info.sha.clone(),
                commits_emitted: report.commits_emitted,
                commits_replayed: report.commits_replayed,
                chunks_emitted: report.chunks_emitted,
                chunks_reused: report.chunks_reused,
            });
        }

        let (last_commit_sha, last_tree_sha) = if selected_total == plan.commits.len() {
            (Some(plan.head_sha), Some(plan.head_tree_sha))
        } else if let Some(sha) = last_ingested_sha {
            let tree_sha = git::tree_sha(&self.repo_path, &sha)?;
            (Some(sha), Some(tree_sha))
        } else {
            (parsed.last_commit_sha, parsed.last_tree_sha)
        };
        let next = CodeCursor {
            last_commit_sha,
            last_tree_sha,
            last_scope_hash: Some(scope_hash),
        };
        Ok((report, encode_cursor(&next)?))
    }

    /// DB-aware current-state ingest. Reads the repository's HEAD tree
    /// directly, emits file/chunk heads that differ from the current
    /// indexed heads, tombstones indexed files that disappeared from
    /// HEAD, and returns a cursor advanced to HEAD. It intentionally
    /// emits no commit Facts and does not walk history.
    ///
    /// `cursor` is the previous snapshot/poll cursor (empty on first run).
    /// Same HEAD tree and same scope hash is a no-op. Same scope and a
    /// new tree uses `git diff` and does not load file-revision heads.
    /// Missing tree, missing scope hash, or a scope change reconciles
    /// against admitted HEAD paths only — never every head of the repo.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError`] when HEAD cannot be read, when a batch fails to
    /// open, ingest or close, or when the cursor cannot be encoded.
    pub async fn run_head_snapshot(
        &self,
        ctx: &CodeIngestContext<'_>,
        cursor: &Cursor,
    ) -> Result<HeadSnapshotOutcome, IndexError> {
        let pool = ctx.pool();
        let head_sha = git::head_sha(&self.repo_path)?;
        let head_tree_sha = git::tree_sha(&self.repo_path, "HEAD")?;
        let (scope, scope_hash) = self.load_scope(pool).await?;
        let parsed = decode_cursor(cursor)?;
        let same_tree = parsed.last_tree_sha.as_deref() == Some(head_tree_sha.as_str());
        let same_scope = parsed.last_scope_hash.as_deref() == Some(scope_hash.as_str());

        let report = if same_tree && same_scope {
            IndexReport::default()
        } else if same_scope {
            if let Some(last_tree) = parsed.last_tree_sha.as_deref() {
                match git::diff_paths(&self.repo_path, last_tree, &head_tree_sha) {
                    Ok((changed, deleted)) => {
                        self.snapshot_git_delta(ctx, &scope, &head_sha, changed, deleted)
                            .await?
                    }
                    Err(_) => self.snapshot_reconcile(ctx, &scope, &head_sha).await?,
                }
            } else {
                self.snapshot_reconcile(ctx, &scope, &head_sha).await?
            }
        } else {
            self.snapshot_reconcile(ctx, &scope, &head_sha).await?
        };

        let cursor = encode_cursor(&CodeCursor {
            last_commit_sha: Some(head_sha.clone()),
            last_tree_sha: Some(head_tree_sha.clone()),
            last_scope_hash: Some(scope_hash),
        })?;
        Ok(HeadSnapshotOutcome {
            report,
            cursor,
            head_sha,
            head_tree_sha,
        })
    }

    async fn snapshot_git_delta(
        &self,
        ctx: &CodeIngestContext<'_>,
        scope: &ScopeMatcher,
        head_sha: &str,
        changed: Vec<String>,
        deleted: Vec<String>,
    ) -> Result<IndexReport, IndexError> {
        let (changed, deleted, files_excluded) = if scope.admits_everything() {
            (changed, deleted, 0)
        } else {
            let before = changed.len();
            let changed: Vec<String> = changed.into_iter().filter(|p| scope.admits(p)).collect();
            let files_excluded = before.saturating_sub(changed.len());
            let deleted: Vec<String> = deleted.into_iter().filter(|p| scope.admits(p)).collect();
            (changed, deleted, files_excluded)
        };
        let entries = git::ls_tree_paths(&self.repo_path, "HEAD", &changed)?;
        self.snapshot_apply(ctx, head_sha, &entries, &deleted, files_excluded, None)
            .await
    }

    async fn snapshot_reconcile(
        &self,
        ctx: &CodeIngestContext<'_>,
        scope: &ScopeMatcher,
        head_sha: &str,
    ) -> Result<IndexReport, IndexError> {
        // Listing first, contents in bounded batches, so the whole tree's
        // file contents are never resident at once.
        let within_cap: Vec<git::TreeEntry> = git::ls_tree(&self.repo_path, "HEAD")?
            .into_iter()
            .filter(|entry| entry.size <= crate::chunker::MAX_BLOB_BYTES as u64)
            .collect();
        let within_cap_count = within_cap.len();
        // Everything the scope removes stays out of `admitted` below, so a
        // path that has just left scope is tombstoned by the Present-except
        // query. Changing a scope is therefore one re-ingest away.
        let head_entries: Vec<git::TreeEntry> = within_cap
            .into_iter()
            .filter(|entry| scope.admits(&entry.path))
            .collect();
        let files_excluded = within_cap_count.saturating_sub(head_entries.len());
        let admitted: Vec<String> = head_entries
            .iter()
            .map(|entry| entry.path.clone())
            .collect();
        let prior_heads: HashMap<_, _> = ctx
            .file_revision_heads(self.owner, self.repo_id, &admitted)
            .await?
            .into_iter()
            .map(|head| (head.file_path.clone(), head))
            .collect();
        let gone: Vec<String> = ctx
            .present_file_revision_heads_except(self.owner, self.repo_id, &admitted)
            .await?
            .into_iter()
            .map(|head| head.file_path)
            .collect();
        self.snapshot_apply(
            ctx,
            head_sha,
            &head_entries,
            &gone,
            files_excluded,
            Some(&prior_heads),
        )
        .await
    }

    async fn snapshot_apply(
        &self,
        ctx: &CodeIngestContext<'_>,
        head_sha: &str,
        head_entries: &[git::TreeEntry],
        deleted_paths: &[String],
        files_excluded: usize,
        prior_heads: Option<&HashMap<String, FileRevisionHead>>,
    ) -> Result<IndexReport, IndexError> {
        let empty_heads = HashMap::new();
        let skip_heads = prior_heads.unwrap_or(&empty_heads);
        let (indexable, oversized): (Vec<_>, Vec<_>) = head_entries
            .iter()
            .cloned()
            .partition(|entry| entry.size <= crate::chunker::MAX_BLOB_BYTES as u64);
        if indexable.is_empty() && deleted_paths.is_empty() && oversized.is_empty() {
            return Ok(IndexReport {
                files_excluded,
                ..IndexReport::default()
            });
        }
        let now = time::OffsetDateTime::now_utc();
        let mut report = IndexReport {
            files_excluded,
            ..IndexReport::default()
        };
        let mut blob_analysis_cache = HashMap::new();
        let mut pending_present = Vec::new();
        let mut pending_deleted = Vec::new();
        self.ingest_head_entries(
            ctx,
            head_sha,
            now,
            &indexable,
            skip_heads,
            &mut pending_present,
            &mut report,
            &mut blob_analysis_cache,
        )
        .await?;
        for path in oversized
            .into_iter()
            .map(|entry| entry.path)
            .chain(deleted_paths.iter().cloned())
        {
            pending_deleted.push(
                self.tombstone_deleted_path(ctx, head_sha, now, &path, None, &mut report)
                    .await?,
            );
        }
        for pending in pending_present {
            self.derive_present_blob(ctx, pending, &mut report).await?;
        }
        for pending in pending_deleted {
            self.derive_deleted_path(ctx, pending, &mut report).await?;
        }
        Ok(report)
    }

    /// Single-commit ingest: one commit Fact, the commit's own tree diff
    /// materialised as file-revision Facts plus derived code-slice/call
    /// projections. Each call is the unit of observation per doc 01
    /// §"The contract".
    async fn ingest_one_commit(
        &self,
        ctx: &CodeIngestContext<'_>,
        commit_info: &CommitInfo,
        scope: &ScopeMatcher,
        report: &mut IndexReport,
        blob_analysis_cache: &mut HashMap<BlobAnalysisKey, BlobAnalysis>,
    ) -> Result<(), IndexError> {
        let now = time::OffsetDateTime::now_utc();

        // Diff this commit against its first parent (or against the
        // empty tree for a root commit, where `ls-tree` of the commit
        // itself enumerates every blob as "added").
        let commit_tree = git::tree_sha(&self.repo_path, &commit_info.sha)?;
        let (changed, deleted) = if let Some(parent_sha) = commit_info.parents.first() {
            let parent_tree = git::tree_sha(&self.repo_path, parent_sha)?;
            git::diff_paths(&self.repo_path, &parent_tree, &commit_tree)?
        } else {
            // Listing only: a root commit needs the *paths* it added, not
            // the blob contents.
            let added: Vec<String> = git::ls_tree(&self.repo_path, &commit_info.sha)?
                .into_iter()
                .map(|entry| entry.path)
                .collect();
            (added, Vec::new())
        };
        // Out-of-scope paths are dropped from BOTH lists. Dropping them
        // only from `changed` would let a delete of a never-indexed path
        // write a tombstone for a file the index has never heard of.
        let (changed, deleted) = if scope.admits_everything() {
            (changed, deleted)
        } else {
            let before = changed.len();
            let changed: Vec<String> = changed.into_iter().filter(|p| scope.admits(p)).collect();
            report.files_excluded += before.saturating_sub(changed.len());
            let deleted: Vec<String> = deleted.into_iter().filter(|p| scope.admits(p)).collect();
            (changed, deleted)
        };

        // The commit Fact itself.
        let commit_payload = CommitV1 {
            repo_id: self.repo_id,
            sha: commit_info.sha.clone(),
            parents: commit_info.parents.clone(),
            author_name: commit_info.author_name.clone(),
            author_email: commit_info.author_email.clone(),
            author_time: commit_info.author_time,
            committer_name: commit_info.committer_name.clone(),
            committer_email: commit_info.committer_email.clone(),
            committer_time: commit_info.committer_time,
            message: commit_info.message.clone(),
        };
        let outcome = ingest_commit(ctx.engine(), ctx.authz(), &commit_payload, now).await?;
        if outcome.idempotent_replay {
            report.commits_replayed += 1;
        } else {
            report.commits_emitted += 1;
        }
        let commit_memory_id = outcome.memory_id;

        // Every changed file-revision Fact for this commit. Oversized blobs
        // are represented as tombstones so prior chunk heads are closed
        // instead of left stale.
        let mut pending_present = Vec::with_capacity(changed.len());
        let mut pending_deleted = Vec::with_capacity(deleted.len());
        for path in &changed {
            match self
                .ingest_changed_path(
                    ctx,
                    commit_info,
                    now,
                    path,
                    Some(commit_memory_id),
                    report,
                    blob_analysis_cache,
                )
                .await?
            {
                ChangedPathIngest::Present(pending) => pending_present.push(pending),
                ChangedPathIngest::Tombstone(pending) => pending_deleted.push(pending),
            }
        }

        // Deletion Facts for this commit's diff.
        for path in &deleted {
            pending_deleted.push(
                self.tombstone_deleted_path(
                    ctx,
                    &commit_info.sha,
                    now,
                    path,
                    Some(commit_memory_id),
                    report,
                )
                .await?,
            );
        }

        for pending in pending_present {
            self.derive_present_blob(ctx, pending, report).await?;
        }
        for pending in pending_deleted {
            self.derive_deleted_path(ctx, pending, report).await?;
        }
        Ok(())
    }

    /// Emit one file's `file-revision-v1` Fact and
    /// cache the deterministic blob analysis to derive from afterwards.
    #[allow(clippy::too_many_arguments)]
    async fn ingest_changed_path(
        &self,
        ctx: &CodeIngestContext<'_>,
        commit_info: &CommitInfo,
        now: time::OffsetDateTime,
        path: &str,
        source_commit: Option<MemoryId>,
        report: &mut IndexReport,
        blob_analysis_cache: &mut HashMap<BlobAnalysisKey, BlobAnalysis>,
    ) -> Result<ChangedPathIngest, IndexError> {
        let Some(blob) = git::cat_blob(&self.repo_path, &commit_info.sha, path)? else {
            return self
                .tombstone_deleted_path(ctx, &commit_info.sha, now, path, source_commit, report)
                .await
                .map(ChangedPathIngest::Tombstone);
        };
        self.ingest_present_blob(
            ctx,
            &commit_info.sha,
            now,
            path,
            &blob,
            source_commit,
            report,
            blob_analysis_cache,
        )
        .await
        .map(ChangedPathIngest::Present)
    }

    /// Read a HEAD listing's blobs in byte-bounded batches and ingest each.
    ///
    /// One `git cat-file --batch` per batch, with at most
    /// [`BLOB_BATCH_BYTES`] of file contents resident at a time.
    #[allow(clippy::too_many_arguments)]
    async fn ingest_head_entries(
        &self,
        ctx: &CodeIngestContext<'_>,
        head_sha: &str,
        now: time::OffsetDateTime,
        entries: &[git::TreeEntry],
        prior_heads: &HashMap<String, FileRevisionHead>,
        pending_present: &mut Vec<PendingPresentBlob>,
        report: &mut IndexReport,
        blob_analysis_cache: &mut HashMap<BlobAnalysisKey, BlobAnalysis>,
    ) -> Result<(), IndexError> {
        let mut cursor = 0usize;
        while cursor < entries.len() {
            let mut end = cursor;
            let mut budget = 0u64;
            // `end == cursor` keeps a batch non-empty even when the first
            // entry alone exceeds the budget, so no file is ever skipped.
            while end < entries.len()
                && (end == cursor || budget + entries[end].size <= BLOB_BATCH_BYTES)
            {
                budget += entries[end].size;
                end += 1;
            }
            let batch = &entries[cursor..end];
            let oids: Vec<String> = batch.iter().map(|entry| entry.oid.clone()).collect();
            let blobs = git::cat_blobs(&self.repo_path, &oids)?;
            for (entry, blob) in batch.iter().zip(blobs) {
                let content_sha256: [u8; 32] = blake3::hash(&blob).into();
                let already_current = prior_heads.get(&entry.path).is_some_and(|head| {
                    head.state == FileState::Present && head.content_sha256 == content_sha256
                });
                if already_current {
                    continue;
                }
                pending_present.push(
                    self.ingest_present_blob(
                        ctx,
                        head_sha,
                        now,
                        &entry.path,
                        &blob,
                        None,
                        report,
                        blob_analysis_cache,
                    )
                    .await?,
                );
            }
            cursor = end;
        }
        Ok(())
    }

    /// Emit one Present file revision Fact from an already-loaded blob.
    /// Shared by commit replay and HEAD snapshot ingestion.
    #[allow(clippy::too_many_arguments)]
    async fn ingest_present_blob(
        &self,
        ctx: &CodeIngestContext<'_>,
        indexed_commit_sha: &str,
        now: time::OffsetDateTime,
        path: &str,
        blob: &[u8],
        source_commit: Option<MemoryId>,
        report: &mut IndexReport,
        blob_analysis_cache: &mut HashMap<BlobAnalysisKey, BlobAnalysis>,
    ) -> Result<PendingPresentBlob, IndexError> {
        let content_sha256: [u8; 32] = blake3::hash(blob).into();

        let language = crate::chunker::detect_language(path)
            .map(str::to_string)
            .or_else(|| crate::chunker::fallback_language(path).map(str::to_string));
        let rev_payload = FileRevisionV1 {
            repo_id: self.repo_id,
            file_path: path.to_string(),
            language: language.clone(),
            content_sha256,
            size_bytes: blob.len() as u64,
            indexed_commit_sha: indexed_commit_sha.to_string(),
            state: FileState::Present,
        };
        let file_revision =
            ingest_file_revision(ctx.engine(), ctx.authz(), &rev_payload, now).await?;
        if !file_revision.idempotent_replay {
            report.files_present_emitted += 1;
        }

        let analysis_key = (content_sha256, language.clone());
        let analysis = if let Some(cached) = blob_analysis_cache.get(&analysis_key) {
            report.chunks_reused += cached.chunks.len();
            cached.clone()
        } else {
            let lang_static = crate::chunker::detect_language(path);
            let chunks = chunk_blob(path, blob);
            let (definitions, calls) = extract_blob_callgraph(lang_static, blob);
            let analysis = BlobAnalysis {
                chunks,
                definitions,
                calls,
            };
            blob_analysis_cache.insert(analysis_key, analysis.clone());
            analysis
        };

        Ok(PendingPresentBlob {
            path: path.to_string(),
            language,
            file_revision: file_revision.memory_id,
            source_commit,
            analysis,
            replayed: file_revision.idempotent_replay,
        })
    }

    /// Derive code-slice Abstractions and call edges after every Fact
    /// this pass observed has been materialized.
    async fn derive_present_blob(
        &self,
        ctx: &CodeIngestContext<'_>,
        pending: PendingPresentBlob,
        report: &mut IndexReport,
    ) -> Result<(), IndexError> {
        // A receipt-replayed Fact was observed by an earlier pass, and its
        // code slices were derived then. Re-deriving here would re-emit the
        // same slices for a revision this pass did not observe, so the pass
        // skips it and leaves the earlier derivation standing.
        //
        // Ordinary branch work reaches this:
        // index `main`, index a branch that touches the same path, check
        // `main` out again. The `already_current` skip does not catch it,
        // because the current head is by then the *branch's* revision, so
        // `main`'s revision is re-offered and replays.
        if pending.replayed {
            report.chunks_reused += pending.analysis.chunks.len();
            return Ok(());
        }
        let heads = ctx
            .chunk_series_heads(self.owner, self.repo_id, &pending.path)
            .await?;
        self.tombstone_vanished_slices(ctx, &pending, &heads, report)
            .await?;
        let mut file_chunks = self.plan_file_chunks(&pending, &heads)?;
        resolve_intra_file_calls(&pending.analysis.calls, &mut file_chunks);
        self.append_file_chunks(ctx, &pending, &file_chunks, report)
            .await
    }

    /// Tombstone the prior slice indexes that don't appear in the new slice
    /// batch (file content shrank). These tombstones are projection rows
    /// tied to this file-revision Fact, not external observations.
    async fn tombstone_vanished_slices(
        &self,
        ctx: &CodeIngestContext<'_>,
        pending: &PendingPresentBlob,
        heads: &[proxima_storage_pg::query::ChunkSeriesHead],
        report: &mut IndexReport,
    ) -> Result<(), IndexError> {
        let new_indexes: HashSet<u32> = (0..pending.analysis.chunks.len())
            .map(|i| u32::try_from(i).unwrap_or(u32::MAX))
            .collect();
        let mut tomb_payloads = Vec::new();
        let mut tomb_handles = Vec::new();
        for head in heads {
            if head.state != FileState::Present.as_str() {
                continue;
            }
            let prior = u32::try_from(head.chunk_index).map_err(|err| {
                IngestError::Storage(format!(
                    "invalid code chunk index {}: {err}",
                    head.chunk_index
                ))
            })?;
            if !new_indexes.contains(&prior) {
                tomb_payloads.push(tombstone_chunk(
                    self.repo_id,
                    &pending.path,
                    prior,
                    pending.language.clone(),
                ));
                tomb_handles.push(head.handle);
            }
        }
        if !tomb_payloads.is_empty() {
            append_code_slices_with_handles(
                ctx.engine,
                ctx.authz,
                self.owner,
                &tomb_payloads,
                pending.file_revision,
                pending.source_commit,
                &tomb_handles,
            )
            .await?;
            report.chunks_tombstoned += tomb_payloads.len();
        }
        Ok(())
    }

    /// Build this file's slice payloads and pair each with the series handle
    /// it will be written under.
    ///
    /// Reuse listed series handles so intra-file calls can name callees
    /// before insert. Mint only on miss.
    fn plan_file_chunks(
        &self,
        pending: &PendingPresentBlob,
        heads: &[proxima_storage_pg::query::ChunkSeriesHead],
    ) -> Result<Vec<ChunkInfo>, IndexError> {
        let mut bare_payloads: Vec<CodeChunkV1> = Vec::new();
        for (idx, chunk) in pending.analysis.chunks.iter().enumerate() {
            let chunk_index = u32::try_from(idx).unwrap_or(u32::MAX);
            bare_payloads.push(CodeChunkV1 {
                repo_id: self.repo_id,
                file_path: pending.path.clone(),
                chunk_index,
                text: chunk.text.clone(),
                language: chunk.language.map(str::to_string),
                chunk_type: chunk.chunk_type.to_string(),
                byte_range_start: chunk.byte_range_start,
                byte_range_end: chunk.byte_range_end,
                line_range_start: chunk.line_range_start,
                line_range_end: chunk.line_range_end,
                state: FileState::Present,
                calls: Vec::new(),
            });
        }
        let handles = assign_code_chunk_handles(heads, &bare_payloads)?;
        let mut file_chunks: Vec<ChunkInfo> = Vec::new();
        for (payload, handle) in bare_payloads.into_iter().zip(handles) {
            let memory_id = MemoryId::new(handle);
            let item_names: Vec<String> = pending
                .analysis
                .definitions
                .iter()
                .filter(|d| {
                    d.byte_start >= payload.byte_range_start && d.byte_end <= payload.byte_range_end
                })
                .map(|d| d.name.clone())
                .collect();
            file_chunks.push(ChunkInfo {
                memory_id,
                payload,
                item_names,
            });
        }
        Ok(file_chunks)
    }

    /// Write the file's slices in one transaction: the chunks reference each
    /// other, so they are written as a group and their index rows land after
    /// every member exists.
    async fn append_file_chunks(
        &self,
        ctx: &CodeIngestContext<'_>,
        pending: &PendingPresentBlob,
        file_chunks: &[ChunkInfo],
        report: &mut IndexReport,
    ) -> Result<(), IndexError> {
        let payloads = file_chunks
            .iter()
            .map(|chunk| chunk.payload.clone())
            .collect::<Vec<_>>();
        let handles: Vec<Uuid> = file_chunks
            .iter()
            .map(|chunk| chunk.memory_id.into_inner())
            .collect();
        let outcomes = append_code_slices_with_handles(
            ctx.engine,
            ctx.authz,
            self.owner,
            &payloads,
            pending.file_revision,
            pending.source_commit,
            &handles,
        )
        .await?;
        for (chunk, outcome) in file_chunks.iter().zip(&outcomes) {
            if !outcome.idempotent_replay {
                report.chunks_emitted += 1;
            }
            report.call_references_emitted += chunk.payload.calls.len();
        }
        Ok(())
    }

    /// Emit a `file-revision-v1` tombstone Fact
    /// for the deleted path. Code-slice tombstones derive afterwards.
    async fn tombstone_deleted_path(
        &self,
        ctx: &CodeIngestContext<'_>,
        commit_sha: &str,
        now: time::OffsetDateTime,
        path: &str,
        source_commit: Option<MemoryId>,
        report: &mut IndexReport,
    ) -> Result<PendingDeletedPath, IndexError> {
        let rev_payload = FileRevisionV1 {
            repo_id: self.repo_id,
            file_path: path.to_string(),
            language: None,
            content_sha256: [0u8; 32],
            size_bytes: 0,
            indexed_commit_sha: commit_sha.to_string(),
            state: FileState::Tombstone,
        };
        let file_revision =
            ingest_file_revision(ctx.engine(), ctx.authz(), &rev_payload, now).await?;
        report.files_tombstoned += 1;

        Ok(PendingDeletedPath {
            path: path.to_string(),
            file_revision: file_revision.memory_id,
            source_commit,
        })
    }

    /// Derive code-slice tombstones for a deleted path.
    async fn derive_deleted_path(
        &self,
        ctx: &CodeIngestContext<'_>,
        pending: PendingDeletedPath,
        report: &mut IndexReport,
    ) -> Result<(), IndexError> {
        let heads = ctx
            .chunk_series_heads(self.owner, self.repo_id, &pending.path)
            .await?;
        let mut tomb_payloads = Vec::new();
        let mut tomb_handles = Vec::new();
        for head in heads {
            if head.state != FileState::Present.as_str() {
                continue;
            }
            let prior = u32::try_from(head.chunk_index).map_err(|err| {
                IngestError::Storage(format!(
                    "invalid code chunk index {}: {err}",
                    head.chunk_index
                ))
            })?;
            tomb_payloads.push(tombstone_chunk(self.repo_id, &pending.path, prior, None));
            tomb_handles.push(head.handle);
        }
        if !tomb_payloads.is_empty() {
            append_code_slices_with_handles(
                ctx.engine,
                ctx.authz,
                self.owner,
                &tomb_payloads,
                pending.file_revision,
                pending.source_commit,
                &tomb_handles,
            )
            .await?;
            report.chunks_tombstoned += tomb_payloads.len();
        }
        Ok(())
    }
}

/// Resolve each call into the caller/callee chunk pair and record it in the
/// *caller's payload*. Resolution is intra-file v1; cross-file calls wait for
/// an indexed name table. Ten sites into the same callee are ten entries here
/// and one index row — the multiplicity belongs to the node
/// (docs/16 §The Model).
fn resolve_intra_file_calls(calls: &[ExtractedCall], file_chunks: &mut [ChunkInfo]) {
    for call in calls {
        let Some(caller_index) = file_chunks
            .iter()
            .enumerate()
            .filter(|(_, c)| {
                c.payload.byte_range_start <= call.byte_start
                    && c.payload.byte_range_end >= call.byte_end
            })
            .max_by_key(|(_, c)| c.payload.byte_range_start)
            .map(|(index, _)| index)
        else {
            continue;
        };
        let Some(callee_memory_id) = file_chunks
            .iter()
            .find(|c| c.item_names.iter().any(|n| n == &call.callee_name))
            .map(|c| c.memory_id)
        else {
            continue;
        };
        // A chunk that calls itself is not a connection between two
        // things, and the index refuses the row outright.
        if file_chunks[caller_index].memory_id == callee_memory_id {
            continue;
        }
        let site = CodeCallSiteV1 {
            byte_start: call.byte_start,
            byte_end: call.byte_end,
            callee_name: call.callee_name.clone(),
            is_dynamic: call.is_dynamic,
        };
        let calls = &mut file_chunks[caller_index].payload.calls;
        match calls
            .iter_mut()
            .find(|existing| existing.callee_memory_id == callee_memory_id.into_inner())
        {
            Some(existing) => existing.sites.push(site),
            None => calls.push(CodeCallV1 {
                callee_memory_id: callee_memory_id.into_inner(),
                sites: vec![site],
            }),
        }
    }
}

/// Build a tombstone `CodeChunkV1` payload for a `(repo, path, idx)`.
/// `language` is `None` when the file itself was deleted; for shrink
/// tombstones the file's current language is preserved so the head
/// view stays self-consistent.
fn tombstone_chunk(
    repo_id: Uuid,
    path: &str,
    chunk_index: u32,
    language: Option<String>,
) -> CodeChunkV1 {
    CodeChunkV1 {
        repo_id,
        file_path: path.to_string(),
        chunk_index,
        text: String::new(),
        language,
        chunk_type: "block".into(),
        byte_range_start: 0,
        byte_range_end: 0,
        line_range_start: 0,
        line_range_end: 0,
        state: FileState::Tombstone,
        // A tombstone slice asserts that the position is gone. It calls
        // nothing, so it declares nothing and its index rows disappear
        // with it.
        calls: Vec::new(),
    }
}

// ---------------------------------------------------------------------
// Cursor codec.
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[allow(clippy::struct_field_names)]
struct CodeCursor {
    last_commit_sha: Option<String>,
    last_tree_sha: Option<String>,
    last_scope_hash: Option<String>,
}

const CODE_CURSOR_MAGIC: &[u8; 4] = b"PXC1";
const CODE_CURSOR_NONE: u32 = u32::MAX;

fn decode_cursor(c: &Cursor) -> Result<CodeCursor, IndexError> {
    if c.is_empty() {
        return Ok(CodeCursor::default());
    }
    let bytes = c.as_bytes();
    let Some(rest) = bytes.strip_prefix(CODE_CURSOR_MAGIC) else {
        return Err(IndexError::Cursor("invalid cursor magic".into()));
    };
    let mut offset = 0;
    let last_commit_sha = decode_cursor_string(rest, &mut offset)?;
    let last_tree_sha = decode_cursor_string(rest, &mut offset)?;
    let last_scope_hash = if offset == rest.len() {
        None
    } else {
        decode_cursor_string(rest, &mut offset)?
    };
    if offset != rest.len() {
        return Err(IndexError::Cursor("trailing cursor bytes".into()));
    }
    Ok(CodeCursor {
        last_commit_sha,
        last_tree_sha,
        last_scope_hash,
    })
}

fn encode_cursor(c: &CodeCursor) -> Result<Cursor, IndexError> {
    let mut bytes = Vec::with_capacity(4 + 4 + 40 + 4 + 40 + 4 + 64);
    bytes.extend_from_slice(CODE_CURSOR_MAGIC);
    encode_cursor_string(&mut bytes, c.last_commit_sha.as_deref())?;
    encode_cursor_string(&mut bytes, c.last_tree_sha.as_deref())?;
    encode_cursor_string(&mut bytes, c.last_scope_hash.as_deref())?;
    Ok(Cursor::from_bytes(bytes))
}

fn encode_cursor_string(out: &mut Vec<u8>, value: Option<&str>) -> Result<(), IndexError> {
    let Some(value) = value else {
        out.extend_from_slice(&CODE_CURSOR_NONE.to_le_bytes());
        return Ok(());
    };
    let len = u32::try_from(value.len())
        .map_err(|err| IndexError::Cursor(format!("cursor string too long: {err}")))?;
    if len == CODE_CURSOR_NONE {
        return Err(IndexError::Cursor(
            "cursor string length is reserved".into(),
        ));
    }
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(value.as_bytes());
    Ok(())
}

fn decode_cursor_string(bytes: &[u8], offset: &mut usize) -> Result<Option<String>, IndexError> {
    let len_bytes = bytes
        .get(*offset..*offset + 4)
        .ok_or_else(|| IndexError::Cursor("truncated cursor string length".into()))?;
    let len = u32::from_le_bytes(
        len_bytes
            .try_into()
            .map_err(|_| IndexError::Cursor("invalid cursor string length".into()))?,
    );
    *offset += 4;
    if len == CODE_CURSOR_NONE {
        return Ok(None);
    }
    let len = usize::try_from(len)
        .map_err(|err| IndexError::Cursor(format!("invalid cursor length: {err}")))?;
    let value_bytes = bytes
        .get(*offset..*offset + len)
        .ok_or_else(|| IndexError::Cursor("truncated cursor string".into()))?;
    *offset += len;
    String::from_utf8(value_bytes.to_vec())
        .map(Some)
        .map_err(|err| IndexError::Cursor(format!("invalid cursor utf8: {err}")))
}

#[cfg(test)]
mod cursor_tests {
    use super::{CodeCursor, decode_cursor, encode_cursor};

    #[test]
    fn new_cursor_round_trips_scope_hash() {
        let encoded = encode_cursor(&CodeCursor {
            last_commit_sha: Some("c".into()),
            last_tree_sha: Some("t".into()),
            last_scope_hash: Some("s".into()),
        })
        .expect("encode");
        let decoded = decode_cursor(&encoded).expect("decode");
        assert_eq!(decoded.last_commit_sha.as_deref(), Some("c"));
        assert_eq!(decoded.last_tree_sha.as_deref(), Some("t"));
        assert_eq!(decoded.last_scope_hash.as_deref(), Some("s"));
    }

    #[test]
    fn old_cursor_without_scope_hash_still_decodes() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(super::CODE_CURSOR_MAGIC);
        super::encode_cursor_string(&mut bytes, Some("c")).expect("commit");
        super::encode_cursor_string(&mut bytes, Some("t")).expect("tree");
        let decoded = decode_cursor(&proxima_core::Cursor::from_bytes(bytes)).expect("decode");
        assert_eq!(decoded.last_tree_sha.as_deref(), Some("t"));
        assert_eq!(decoded.last_scope_hash, None);
    }
}

type BlobAnalysisKey = ([u8; 32], Option<String>);

#[derive(Debug, Clone)]
struct BlobAnalysis {
    chunks: Vec<Chunk>,
    definitions: Vec<ExtractedDefinition>,
    calls: Vec<ExtractedCall>,
}

#[derive(Debug, Clone)]
enum ChangedPathIngest {
    Present(PendingPresentBlob),
    Tombstone(PendingDeletedPath),
}

#[derive(Debug, Clone)]
struct PendingPresentBlob {
    path: String,
    language: Option<String>,
    file_revision: MemoryId,
    source_commit: Option<MemoryId>,
    analysis: BlobAnalysis,
    /// The Fact was receipt-replayed rather than newly written, so it still
    /// belongs to the batch that first observed it and its chunks were
    /// derived then. See [`LocalGitSource::derive_present_blob`].
    replayed: bool,
}

#[derive(Debug, Clone)]
struct PendingDeletedPath {
    path: String,
    file_revision: MemoryId,
    source_commit: Option<MemoryId>,
}

/// Chunk info for call resolution.
#[derive(Debug, Clone)]
struct ChunkInfo {
    memory_id: MemoryId,
    payload: CodeChunkV1,
    item_names: Vec<String>,
}

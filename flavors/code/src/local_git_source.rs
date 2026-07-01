#![allow(
    clippy::missing_errors_doc,
    clippy::doc_markdown,
    clippy::similar_names
)]
//! `LocalGitSource` — pull-mode git ingest over a local repository.
//!
//! [`LocalGitSource::run_poll`] walks git since the supplied
//! [`proxima_core::Cursor`] and ingests **one `source_batch` per
//! commit**. Per doc 01 §"The contract", a commit is the natural
//! observational unit for git: one author's one logical change.
//! The poll itself is a delivery mechanism, not an observation —
//! its boundary is arbitrary cadence, while the commit is the
//! causal atom F→A consumes per batch.
//!
//! Each commit's batch contains the `commit-v1` Fact plus
//! `file-revision-v1` Facts for that commit's tree diff against its
//! first parent (or the empty tree for root commits). Deterministic
//! chunk/call extraction is F→A operator work over those file Facts:
//! it emits `code-chunk-v1` code-slice Abstractions plus Engine-authored
//! `proxima-code/calls` structural edges; code slices carry provenance to
//! file/commit Facts. `indexed_commit_sha` is the commit's own sha, not HEAD.
//!
//! Cursor format (tagged binary bytes inside the opaque `Cursor` newtype):
//! ```ignore
//! b"PXC1" || opt_string(last_commit_sha) || opt_string(last_tree_sha)
//! ```
//! `None` for both means "from the beginning"; subsequent polls walk
//! only commits between `last_commit_sha` and `HEAD`.
//!
//! Typed sidecar inserts must run alongside Fact materialization
//! (AGENTS.md invariant 15), so this surface is intentionally
//! DB-aware rather than substrate-generic.
//!
//! Uses shell `git` via `std::process::Command`. The host must have
//! `git` on PATH. This trade-off keeps the dep surface minimal —
//! `gix` would more than double our build time.

mod git;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use proxima_core::{AuthzContext, Cursor, Engine, MemoryId, Owner, SourceBatchId, ToolError};
use sqlx::PgPool;
use uuid::Uuid;

use self::git::{CommitInfo, WalkPlan};
use crate::calls::{ExtractedCall, ExtractedDefinition, extract_blob_callgraph};
use crate::chunker::{Chunk, chunk_blob};
use crate::ingest::{
    CallEdgeDraft, FileRevisionHead, IngestError, append_code_slice, ingest_calls_edge,
    ingest_commit, ingest_file_revision,
};
use crate::payloads::{CodeChunkV1, CommitV1, FileRevisionV1, FileState};
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
}

/// Counters returned by [`LocalGitSource::run_poll`]. Sums across
/// every commit-batch the poll opened.
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

    async fn file_revision_heads(
        &self,
        owner: Owner,
        repo_id: Uuid,
    ) -> Result<Vec<FileRevisionHead>, IngestError> {
        let (owner_kind, owner_id) = owner.columns();
        let candidate_ids: Vec<Uuid> = sqlx::query_scalar(
            "SELECT fr.memory_id
               FROM proxima_code.file_revision_v1 fr
               JOIN proxima_core.memories m USING (memory_id)
               JOIN proxima_core.fact_receipts r USING (receipt_id)
              WHERE m.owner_kind = $1
                AND m.owner_id IS NOT DISTINCT FROM $2
                AND fr.repo_id = $3
                AND NOT EXISTS (
                    SELECT 1
                      FROM proxima_code.file_revision_v1 fr2
                      JOIN proxima_core.memories m2 USING (memory_id)
                      JOIN proxima_core.fact_receipts r2 USING (receipt_id)
                     WHERE m2.owner_kind = m.owner_kind
                       AND m2.owner_id IS NOT DISTINCT FROM m.owner_id
                       AND fr2.repo_id = fr.repo_id
                       AND fr2.file_path = fr.file_path
                       AND r2.source_batch_id > r.source_batch_id
                )
              ORDER BY fr.file_path ASC
              LIMIT 100000",
        )
        .bind(owner_kind)
        .bind(owner_id)
        .bind(repo_id)
        .fetch_all(self.pool())
        .await?;
        let mut payloads = Vec::new();
        for chunk in candidate_ids.chunks(2_000) {
            payloads.extend(
                self.store
                    .authorized_fact_payloads_include_tombstones::<FileRevisionV1>(
                        self.engine,
                        self.authz,
                        owner,
                        chunk,
                        chunk.len(),
                    )
                    .await
                    .map_err(|err| read_error(&err))?,
            );
        }
        Ok(payloads
            .into_iter()
            .filter(|(_, payload)| payload.repo_id == repo_id)
            .map(|(memory_id, payload)| FileRevisionHead {
                memory_id,
                file_path: payload.file_path,
                content_sha256: payload.content_sha256,
                state: payload.state,
            })
            .collect())
    }

    async fn present_chunk_indexes(
        &self,
        owner: Owner,
        repo_id: Uuid,
        file_path: &str,
    ) -> Result<Vec<u32>, IngestError> {
        let (owner_kind, owner_id) = owner.columns();
        let indexes = sqlx::query_scalar::<_, i32>(
            "SELECT DISTINCT s.chunk_index
               FROM proxima_core.memories m
               JOIN proxima_code.code_chunk_v1 s USING (memory_id)
               JOIN (SELECT memory_id AS entity_id, owner_kind, owner_id FROM proxima_core.memories UNION ALL SELECT goal_id AS entity_id, owner_kind, owner_id FROM proxima_core.goals) eo
                 ON eo.entity_id = m.memory_id
              WHERE eo.owner_kind = $1
                AND eo.owner_id IS NOT DISTINCT FROM $2
                AND s.repo_id = $3
                AND s.file_path = $4
                AND s.state = 'Present'
                AND NOT EXISTS (
                    SELECT 1
                      FROM proxima_core.memories m2
                      JOIN proxima_code.code_chunk_v1 s2 USING (memory_id)
                      JOIN (SELECT memory_id AS entity_id, owner_kind, owner_id FROM proxima_core.memories UNION ALL SELECT goal_id AS entity_id, owner_kind, owner_id FROM proxima_core.goals) eo2
                        ON eo2.entity_id = m2.memory_id
                     WHERE m2.schema_id = m.schema_id
                       AND eo2.owner_kind = eo.owner_kind
                       AND eo2.owner_id IS NOT DISTINCT FROM eo.owner_id
                       AND s2.repo_id = s.repo_id
                       AND s2.file_path = s.file_path
                       AND s2.chunk_index = s.chunk_index
                       AND m2.source_batch_id > m.source_batch_id
                )
              ORDER BY s.chunk_index ASC
              LIMIT 100000",
        )
        .bind(owner_kind)
        .bind(owner_id)
        .bind(repo_id)
        .bind(file_path)
        .fetch_all(self.pool())
        .await?;
        indexes
            .into_iter()
            .map(|idx| {
                u32::try_from(idx).map_err(|err| {
                    IngestError::Storage(format!("invalid code chunk index {idx}: {err}"))
                })
            })
            .collect()
    }
}

fn read_error(err: &ToolError) -> IngestError {
    IngestError::Storage(format!("authorized code-flavor read: {err}"))
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

    /// DB-aware ingest. Walks each commit since the cursor, opens a
    /// `source_batch` per commit, emits the commit Fact plus the
    /// file-revision Facts from that commit's tree diff, derives
    /// code-slice Abstractions/call edges from those Facts, then
    /// closes the batch. F→A in M5+ consumes one batch = one commit's
    /// worth of causally-coherent Facts.
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
        let mut report = IndexReport::default();
        let commit_limit = max_commits.unwrap_or(usize::MAX);
        let selected_total = plan.commits.len().min(commit_limit);

        // Per-poll cache: reuse deterministic chunk/call extraction
        // for identical blob bytes. This is a parse-work cache only;
        // every changed path/commit still emits derived code-slice
        // projection rows tied to its own file-revision Fact.
        let mut blob_analysis_cache: HashMap<BlobAnalysisKey, BlobAnalysis> = HashMap::new();

        // git_log returns newest-first; process oldest-first so each
        // commit's tree diff against its first parent reflects the
        // historical order, and the NK head advances monotonically.
        let mut last_ingested_sha: Option<String> = None;
        for (i, commit_info) in plan.commits.iter().rev().take(selected_total).enumerate() {
            self.ingest_one_commit(ctx, commit_info, &mut report, &mut blob_analysis_cache)
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
        };
        Ok((report, encode_cursor(&next)?))
    }

    /// DB-aware current-state ingest. Reads the repository's HEAD tree
    /// directly, emits file/chunk heads that differ from the current
    /// indexed heads, tombstones indexed files that disappeared from
    /// HEAD, and returns a cursor advanced to HEAD. It intentionally
    /// emits no commit Facts and does not walk history.
    pub async fn run_head_snapshot(
        &self,
        ctx: &CodeIngestContext<'_>,
    ) -> Result<HeadSnapshotOutcome, IndexError> {
        let pool = ctx.pool();
        let head_sha = git::head_sha(&self.repo_path)?;
        let head_tree_sha = git::tree_sha(&self.repo_path, "HEAD")?;
        let now = time::OffsetDateTime::now_utc();
        let batch_id = SourceBatchId::new(Uuid::now_v7());
        let head_files = git::ls_files(&self.repo_path, "HEAD")?;
        let present_paths: HashSet<String> =
            head_files.iter().map(|(path, _)| path.clone()).collect();
        let prior_heads: HashMap<_, _> = ctx
            .file_revision_heads(self.owner, self.repo_id)
            .await?
            .into_iter()
            .map(|head| (head.file_path.clone(), head))
            .collect();

        let mut report = IndexReport::default();
        let mut blob_analysis_cache = HashMap::new();
        let mut pending_present = Vec::new();
        let mut pending_deleted = Vec::new();
        for (path, blob) in head_files {
            let content_sha256: [u8; 32] = blake3::hash(&blob).into();
            let already_current = prior_heads.get(&path).is_some_and(|head| {
                head.state == FileState::Present && head.content_sha256 == content_sha256
            });
            if already_current {
                continue;
            }
            pending_present.push(
                self.ingest_present_blob(
                    pool,
                    &head_sha,
                    batch_id,
                    now,
                    &path,
                    &blob,
                    None,
                    &mut report,
                    &mut blob_analysis_cache,
                )
                .await?,
            );
        }

        for (path, prior) in prior_heads {
            if prior.state == FileState::Present && !present_paths.contains(&path) {
                pending_deleted.push(
                    self.tombstone_deleted_path(
                        pool,
                        &head_sha,
                        batch_id,
                        now,
                        &path,
                        None,
                        &mut report,
                    )
                    .await?,
                );
            }
        }

        crate::ingest::close_local_git_batch(pool, &self.owner, batch_id).await?;
        for pending in pending_present {
            self.derive_present_blob(ctx, batch_id, pending, &mut report)
                .await?;
        }
        for pending in pending_deleted {
            self.derive_deleted_path(ctx, batch_id, pending, &mut report)
                .await?;
        }
        let cursor = encode_cursor(&CodeCursor {
            last_commit_sha: Some(head_sha.clone()),
            last_tree_sha: Some(head_tree_sha.clone()),
        })?;

        Ok(HeadSnapshotOutcome {
            report,
            cursor,
            head_sha,
            head_tree_sha,
        })
    }

    /// Single-commit ingest: one `source_batch_id`, one commit Fact,
    /// the commit's own tree diff materialised as file-revision Facts
    /// plus derived code-slice/call projections. Each call is the unit
    /// of observation per doc 01 §"The contract".
    async fn ingest_one_commit(
        &self,
        ctx: &CodeIngestContext<'_>,
        commit_info: &CommitInfo,
        report: &mut IndexReport,
        blob_analysis_cache: &mut HashMap<BlobAnalysisKey, BlobAnalysis>,
    ) -> Result<(), IndexError> {
        let pool = ctx.pool();
        let now = time::OffsetDateTime::now_utc();
        let batch_id = SourceBatchId::new(Uuid::now_v7());

        // Diff this commit against its first parent (or against the
        // empty tree for a root commit, where `ls-tree` of the commit
        // itself enumerates every blob as "added").
        let commit_tree = git::tree_sha(&self.repo_path, &commit_info.sha)?;
        let (changed, deleted) = if let Some(parent_sha) = commit_info.parents.first() {
            let parent_tree = git::tree_sha(&self.repo_path, parent_sha)?;
            git::diff_paths(&self.repo_path, &parent_tree, &commit_tree)?
        } else {
            let added: Vec<String> = git::ls_files(&self.repo_path, &commit_info.sha)?
                .into_iter()
                .map(|(p, _)| p)
                .collect();
            (added, Vec::new())
        };

        // Phase 1 — the commit Fact itself.
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
        let outcome = ingest_commit(pool, &self.owner, batch_id, &commit_payload, now).await?;
        if outcome.idempotent_replay {
            report.commits_replayed += 1;
        } else {
            report.commits_emitted += 1;
        }
        let commit_memory_id = outcome.memory_id;

        // Phase 2 — materialize every changed file-revision Fact for this
        // commit. Oversized blobs are intentionally represented as
        // tombstones so prior chunk heads are closed instead of left stale.
        let mut pending_present = Vec::with_capacity(changed.len());
        let mut pending_deleted = Vec::with_capacity(deleted.len());
        for path in &changed {
            match self
                .ingest_changed_path(
                    pool,
                    commit_info,
                    batch_id,
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

        // Phase 3 — materialize deletion Facts for this commit's diff.
        for path in &deleted {
            pending_deleted.push(
                self.tombstone_deleted_path(
                    pool,
                    &commit_info.sha,
                    batch_id,
                    now,
                    path,
                    Some(commit_memory_id),
                    report,
                )
                .await?,
            );
        }

        // Close this commit's batch before any F→A derivation consumes it.
        crate::ingest::close_local_git_batch(pool, &self.owner, batch_id).await?;
        for pending in pending_present {
            self.derive_present_blob(ctx, batch_id, pending, report)
                .await?;
        }
        for pending in pending_deleted {
            self.derive_deleted_path(ctx, batch_id, pending, report)
                .await?;
        }
        Ok(())
    }

    /// Phase-2 inner loop: emit one file's `file-revision-v1` Fact and
    /// cache the deterministic blob analysis to derive after batch close.
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    async fn ingest_changed_path(
        &self,
        pool: &PgPool,
        commit_info: &CommitInfo,
        batch_id: SourceBatchId,
        now: time::OffsetDateTime,
        path: &str,
        source_commit: Option<MemoryId>,
        report: &mut IndexReport,
        blob_analysis_cache: &mut HashMap<BlobAnalysisKey, BlobAnalysis>,
    ) -> Result<ChangedPathIngest, IndexError> {
        let Some(blob) = git::cat_blob(&self.repo_path, &commit_info.sha, path)? else {
            return self
                .tombstone_deleted_path(
                    pool,
                    &commit_info.sha,
                    batch_id,
                    now,
                    path,
                    source_commit,
                    report,
                )
                .await
                .map(ChangedPathIngest::Tombstone);
        };
        self.ingest_present_blob(
            pool,
            &commit_info.sha,
            batch_id,
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

    /// Emit one Present file revision Fact from an already-loaded blob.
    /// Shared by commit replay and HEAD snapshot ingestion.
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    async fn ingest_present_blob(
        &self,
        pool: &PgPool,
        indexed_commit_sha: &str,
        batch_id: SourceBatchId,
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
            ingest_file_revision(pool, &self.owner, batch_id, &rev_payload, now).await?;
        report.files_present_emitted += 1;

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
        })
    }

    /// Derive code-slice Abstractions and call edges after all Facts in
    /// the source batch have been materialized and the batch is closed.
    #[allow(clippy::too_many_lines)]
    async fn derive_present_blob(
        &self,
        ctx: &CodeIngestContext<'_>,
        batch_id: SourceBatchId,
        pending: PendingPresentBlob,
        report: &mut IndexReport,
    ) -> Result<(), IndexError> {
        let pool = ctx.pool();
        // Re-derive and tombstone any prior slice indexes that don't
        // appear in the new slice batch (file content shrunk). These
        // tombstones are projection rows tied to this file-revision
        // Fact, not external observations.
        let new_indexes: HashSet<u32> = (0..pending.analysis.chunks.len())
            .map(|i| u32::try_from(i).unwrap_or(u32::MAX))
            .collect();
        let prior_indexes = ctx
            .present_chunk_indexes(self.owner, self.repo_id, &pending.path)
            .await?;
        for prior in prior_indexes {
            if !new_indexes.contains(&prior) {
                let tomb =
                    tombstone_chunk(self.repo_id, &pending.path, prior, pending.language.clone());
                append_code_slice(
                    pool,
                    &self.owner,
                    batch_id,
                    &tomb,
                    pending.file_revision,
                    pending.source_commit,
                )
                .await?;
                report.chunks_tombstoned += 1;
            }
        }

        let mut file_chunks: Vec<ChunkInfo> = Vec::new();
        for (idx, chunk) in pending.analysis.chunks.iter().enumerate() {
            let chunk_index = u32::try_from(idx).unwrap_or(u32::MAX);
            let payload = CodeChunkV1 {
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
            };
            let outcome = append_code_slice(
                pool,
                &self.owner,
                batch_id,
                &payload,
                pending.file_revision,
                pending.source_commit,
            )
            .await?;
            if !outcome.idempotent_replay {
                report.chunks_emitted += 1;
            }

            let item_names: Vec<String> = pending
                .analysis
                .definitions
                .iter()
                .filter(|d| {
                    d.byte_start >= chunk.byte_range_start && d.byte_end <= chunk.byte_range_end
                })
                .map(|d| d.name.clone())
                .collect();

            file_chunks.push(ChunkInfo {
                memory_id: outcome.memory_id,
                byte_range_start: chunk.byte_range_start,
                byte_range_end: chunk.byte_range_end,
                item_names,
            });
        }

        // After all slices for this file, resolve calls into the
        // caller/callee code-slice pair and emit one Engine-authored
        // typed edge each. Resolution is intra-file v1; cross-file
        // calls wait for an indexed name table.
        for call in pending.analysis.calls {
            let caller_chunk = file_chunks
                .iter()
                .filter(|c| {
                    c.byte_range_start <= call.byte_start && c.byte_range_end >= call.byte_end
                })
                .max_by_key(|c| c.byte_range_start);
            let Some(caller) = caller_chunk else { continue };

            let callee_chunk = file_chunks
                .iter()
                .find(|c| c.item_names.iter().any(|n| n == &call.callee_name));
            let Some(callee) = callee_chunk else { continue };

            if caller.memory_id == callee.memory_id {
                continue;
            }

            let callsite_byte_start_in_source_chunk =
                call.byte_start.saturating_sub(caller.byte_range_start);

            ingest_calls_edge(
                pool,
                &self.owner,
                &CallEdgeDraft {
                    source_memory_id: caller.memory_id.into_inner(),
                    target_memory_id: callee.memory_id.into_inner(),
                    callsite_byte_start: call.byte_start,
                    callsite_byte_end: call.byte_end,
                    callsite_byte_start_in_source_chunk,
                    callee_name: call.callee_name,
                    is_dynamic: call.is_dynamic,
                },
            )
            .await?;
        }
        Ok(())
    }

    /// Phase-3 inner loop: emit a `file-revision-v1` tombstone Fact
    /// for the deleted path. Code-slice tombstones derive after batch close.
    #[allow(clippy::too_many_arguments)]
    async fn tombstone_deleted_path(
        &self,
        pool: &PgPool,
        commit_sha: &str,
        batch_id: SourceBatchId,
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
            ingest_file_revision(pool, &self.owner, batch_id, &rev_payload, now).await?;
        report.files_tombstoned += 1;

        Ok(PendingDeletedPath {
            path: path.to_string(),
            file_revision: file_revision.memory_id,
            source_commit,
        })
    }

    /// Derive code-slice tombstones for a deleted path after batch close.
    async fn derive_deleted_path(
        &self,
        ctx: &CodeIngestContext<'_>,
        batch_id: SourceBatchId,
        pending: PendingDeletedPath,
        report: &mut IndexReport,
    ) -> Result<(), IndexError> {
        let pool = ctx.pool();
        let prior_indexes = ctx
            .present_chunk_indexes(self.owner, self.repo_id, &pending.path)
            .await?;
        for prior in prior_indexes {
            let tomb = tombstone_chunk(self.repo_id, &pending.path, prior, None);
            append_code_slice(
                pool,
                &self.owner,
                batch_id,
                &tomb,
                pending.file_revision,
                pending.source_commit,
            )
            .await?;
            report.chunks_tombstoned += 1;
        }
        Ok(())
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
    }
}

// ---------------------------------------------------------------------
// Cursor codec.
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct CodeCursor {
    last_commit_sha: Option<String>,
    last_tree_sha: Option<String>,
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
    if offset != rest.len() {
        return Err(IndexError::Cursor("trailing cursor bytes".into()));
    }
    Ok(CodeCursor {
        last_commit_sha,
        last_tree_sha,
    })
}

fn encode_cursor(c: &CodeCursor) -> Result<Cursor, IndexError> {
    let mut bytes = Vec::with_capacity(4 + 4 + 40 + 4 + 40);
    bytes.extend_from_slice(CODE_CURSOR_MAGIC);
    encode_cursor_string(&mut bytes, c.last_commit_sha.as_deref())?;
    encode_cursor_string(&mut bytes, c.last_tree_sha.as_deref())?;
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
    byte_range_start: u32,
    byte_range_end: u32,
    item_names: Vec<String>,
}

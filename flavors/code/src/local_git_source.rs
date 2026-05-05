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
//! Each commit's batch contains: the `commit-v1` Fact, plus the
//! `file-revision-v1` and `code-chunk-v1` Facts derived from that
//! commit's tree diff against its first parent (or against the
//! empty tree, for root commits). `indexed_commit_sha` is the
//! commit's own sha, not HEAD.
//!
//! Cursor format (json bytes inside the opaque `Cursor` newtype):
//! ```ignore
//! { "last_commit_sha": "...", "last_tree_sha": "..." }
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

use std::collections::HashSet;
use std::path::PathBuf;

use proxima_core::{Cursor, MemoryId, Owner, SourceBatchId};
use sqlx::PgPool;
use uuid::Uuid;

use self::git::{CommitInfo, WalkPlan};
use crate::calls::extract_blob_callgraph;
use crate::chunker::chunk_blob;
use crate::ingest::{
    CallEdgeDraft, IngestError, ingest_calls_edge, ingest_code_chunk, ingest_commit,
    ingest_file_revision, lookup_present_chunk_memory_id_by_text, present_chunk_indexes,
};
use crate::payloads::{CodeChunkV1, CommitV1, FileRevisionV1, FileState};

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
/// `chunks_reused` counts chunks where Layer-A dedup found a Present
/// head with matching text at the same NK and skipped re-emission of
/// a fresh Fact. It's a separate counter from `chunks_emitted` so
/// callers can observe how much commit-replay churn the dedup
/// actually absorbs (one of the M5.5 done-when criteria).
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
    /// file-revision and chunk Facts derived from that commit's tree
    /// diff (against its first parent, or against the empty tree for
    /// root commits), then closes the batch. F→A in M5+ consumes one
    /// batch = one commit's worth of causally-coherent Facts.
    pub async fn run_poll(
        &self,
        pool: &PgPool,
        cursor: &Cursor,
        progress: &mut impl FnMut(IngestProgress),
    ) -> Result<(IndexReport, Cursor), IndexError> {
        let parsed = decode_cursor(cursor)?;
        let plan = self.walk_git(&parsed)?;
        let mut report = IndexReport::default();

        // Per-poll cache: skip re-running tree-sitter on a blob whose
        // content_sha256 we've already chunked this poll. Substrate
        // event_id dedup keeps correctness; this just spares the
        // chunker work for blobs reused across commits (refactors,
        // reverts, copies). Resets per poll because cross-poll
        // dedup is the substrate's job.
        let mut chunked_this_poll: HashSet<[u8; 32]> = HashSet::new();

        // git_log returns newest-first; process oldest-first so each
        // commit's tree diff against its first parent reflects the
        // historical order, and the NK head advances monotonically.
        for (i, commit_info) in plan.commits.iter().rev().enumerate() {
            self.ingest_one_commit(pool, commit_info, &mut report, &mut chunked_this_poll)
                .await?;
            progress(IngestProgress {
                commit_index: i,
                total_commits: plan.commits.len(),
                commit_sha: commit_info.sha.clone(),
                commits_emitted: report.commits_emitted,
                commits_replayed: report.commits_replayed,
                chunks_emitted: report.chunks_emitted,
                chunks_reused: report.chunks_reused,
            });
        }

        let next = CodeCursor {
            last_commit_sha: Some(plan.head_sha),
            last_tree_sha: Some(plan.head_tree_sha),
        };
        Ok((report, encode_cursor(&next)?))
    }

    /// Single-commit ingest: one `source_batch_id`, one commit Fact,
    /// the commit's own tree diff materialised as file-revisions and
    /// chunks (or tombstones for deletions). Each call is the unit of
    /// observation per doc 01 §"The contract".
    async fn ingest_one_commit(
        &self,
        pool: &PgPool,
        commit_info: &CommitInfo,
        report: &mut IndexReport,
        chunked_this_poll: &mut HashSet<[u8; 32]>,
    ) -> Result<(), IndexError> {
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

        // Phase 2 — file revisions + chunks for this commit's changes.
        for path in &changed {
            self.ingest_changed_path(
                pool,
                commit_info,
                batch_id,
                now,
                path,
                report,
                chunked_this_poll,
            )
            .await?;
        }

        // Phase 3 — deletions reported by this commit's diff.
        for path in &deleted {
            self.tombstone_deleted_path(pool, &commit_info.sha, batch_id, now, path, report)
                .await?;
        }

        // Close this commit's batch.
        crate::ingest::close_local_git_batch(pool, &self.owner, batch_id).await?;
        Ok(())
    }

    /// Phase-2 inner loop: emit one file's `file-revision-v1` Fact, the
    /// chunk batch (or a tombstone burst if the chunk count shrank), and
    /// resolve intra-file calls into typed `code/calls` edges.
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    async fn ingest_changed_path(
        &self,
        pool: &PgPool,
        commit_info: &CommitInfo,
        batch_id: SourceBatchId,
        now: time::OffsetDateTime,
        path: &str,
        report: &mut IndexReport,
        chunked_this_poll: &mut HashSet<[u8; 32]>,
    ) -> Result<(), IndexError> {
        let blob = git::cat_blob(&self.repo_path, &commit_info.sha, path)?;
        let content_sha256: [u8; 32] = blake3::hash(&blob).into();

        let language = crate::chunker::detect_language(path)
            .map(str::to_string)
            .or_else(|| crate::chunker::fallback_language(path).map(str::to_string));
        let rev_payload = FileRevisionV1 {
            repo_id: self.repo_id,
            file_path: path.to_string(),
            language: language.clone(),
            content_sha256,
            size_bytes: blob.len() as u64,
            indexed_commit_sha: commit_info.sha.clone(),
            state: FileState::Present,
        };
        ingest_file_revision(pool, &self.owner, batch_id, &rev_payload, now).await?;
        report.files_present_emitted += 1;

        // Skip re-chunking blobs we've already chunked this poll
        // (perf only; the chunk Facts are content-keyed via event_id
        // so repeated calls would just be no-ops at the substrate).
        if !chunked_this_poll.insert(content_sha256) {
            return Ok(());
        }

        // Re-chunk and tombstone any prior indexes that don't appear
        // in the new chunk batch (file content shrunk).
        let chunks = chunk_blob(path, &blob);
        let new_indexes: HashSet<u32> = (0..chunks.len())
            .map(|i| u32::try_from(i).unwrap_or(u32::MAX))
            .collect();
        let prior_indexes = present_chunk_indexes(pool, &self.owner, self.repo_id, path).await?;
        for prior in prior_indexes {
            if !new_indexes.contains(&prior) {
                let tomb = tombstone_chunk(self.repo_id, path, prior, language.clone());
                ingest_code_chunk(pool, &self.owner, batch_id, &tomb, [0u8; 32], now).await?;
                report.chunks_tombstoned += 1;
            }
        }

        // Single tree-sitter parse of the blob: defs + calls come from
        // the same Tree, mapped through cached Query patterns (see
        // `calls.rs`). Each definition's byte range is then assigned
        // to whichever chunk contains it.
        let lang_static = crate::chunker::detect_language(path);
        let (definitions, calls) = extract_blob_callgraph(lang_static, &blob);

        let mut file_chunks: Vec<ChunkInfo> = Vec::new();
        for (idx, chunk) in chunks.into_iter().enumerate() {
            let chunk_index = u32::try_from(idx).unwrap_or(u32::MAX);

            // Layer-A dedup: if the Present head at this NK already
            // has identical text, reuse its memory_id. Skipping the
            // ingest_code_chunk call here means no fresh memory_id,
            // no duplicate substrate event, and — combined with
            // Layer B's deterministic edge_id — typed call edges
            // collapse to one row per logical call site across
            // arbitrary commit replay.
            let memory_id = if let Some(existing) = lookup_present_chunk_memory_id_by_text(
                pool,
                &self.owner,
                self.repo_id,
                path,
                chunk_index,
                &chunk.text,
            )
            .await?
            {
                report.chunks_reused += 1;
                existing
            } else {
                let payload = CodeChunkV1 {
                    repo_id: self.repo_id,
                    file_path: path.to_string(),
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
                let outcome =
                    ingest_code_chunk(pool, &self.owner, batch_id, &payload, content_sha256, now)
                        .await?;
                report.chunks_emitted += 1;
                outcome.memory_id
            };

            let item_names: Vec<String> = definitions
                .iter()
                .filter(|d| {
                    d.byte_start >= chunk.byte_range_start && d.byte_end <= chunk.byte_range_end
                })
                .map(|d| d.name.clone())
                .collect();

            file_chunks.push(ChunkInfo {
                memory_id,
                byte_range_start: chunk.byte_range_start,
                byte_range_end: chunk.byte_range_end,
                item_names,
            });
        }

        // After all chunks for this file, resolve calls into the
        // caller/callee chunk pair and emit one typed edge each.
        // Resolution is purely intra-file v1; cross-file calls wait
        // for an indexed name table (M6).
        for call in calls {
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

            // Chunk-relative offset is the dedup-stable component of
            // the deterministic edge_id (Layer B). When the source
            // chunk's text is unchanged but its position in the file
            // has shifted, file-level `call.byte_start` shifts but
            // `call.byte_start - caller.byte_range_start` does not.
            // saturating_sub keeps the cast well-defined even on a
            // pathological mismatch (caller resolved by `max_by_key`,
            // so call.byte_start should always be >= byte_range_start
            // in practice).
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
    /// for the deleted path and burst-tombstone every chunk index
    /// whose head was still `Present`.
    async fn tombstone_deleted_path(
        &self,
        pool: &PgPool,
        commit_sha: &str,
        batch_id: SourceBatchId,
        now: time::OffsetDateTime,
        path: &str,
        report: &mut IndexReport,
    ) -> Result<(), IndexError> {
        let rev_payload = FileRevisionV1 {
            repo_id: self.repo_id,
            file_path: path.to_string(),
            language: None,
            content_sha256: [0u8; 32],
            size_bytes: 0,
            indexed_commit_sha: commit_sha.to_string(),
            state: FileState::Tombstone,
        };
        ingest_file_revision(pool, &self.owner, batch_id, &rev_payload, now).await?;
        report.files_tombstoned += 1;

        let prior_indexes = present_chunk_indexes(pool, &self.owner, self.repo_id, path).await?;
        for prior in prior_indexes {
            let tomb = tombstone_chunk(self.repo_id, path, prior, None);
            ingest_code_chunk(pool, &self.owner, batch_id, &tomb, [0u8; 32], now).await?;
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

fn decode_cursor(c: &Cursor) -> Result<CodeCursor, IndexError> {
    if c.is_empty() {
        return Ok(CodeCursor::default());
    }
    serde_json::from_slice(c.as_bytes()).map_err(|e| IndexError::Cursor(e.to_string()))
}

fn encode_cursor(c: &CodeCursor) -> Result<Cursor, IndexError> {
    let bytes = serde_json::to_vec(c).map_err(|e| IndexError::Cursor(e.to_string()))?;
    Ok(Cursor::from_bytes(bytes))
}

/// Chunk info for call resolution.
#[derive(Debug, Clone)]
struct ChunkInfo {
    memory_id: MemoryId,
    byte_range_start: u32,
    byte_range_end: u32,
    item_names: Vec<String>,
}

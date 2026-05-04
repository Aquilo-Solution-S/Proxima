#![allow(
    clippy::missing_errors_doc,
    clippy::doc_markdown,
    clippy::too_many_lines
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

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use proxima_core::{Cursor, Owner, SourceBatchId};
use sqlx::PgPool;
use uuid::Uuid;

use crate::chunker::chunk_blob;
use crate::ingest::{
    IngestError, ingest_code_chunk, ingest_commit, ingest_file_revision, present_chunk_indexes,
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
#[derive(Debug, Default, Clone)]
pub struct IndexReport {
    pub commits_emitted: usize,
    pub commits_replayed: usize,
    pub files_present_emitted: usize,
    pub files_tombstoned: usize,
    pub chunks_emitted: usize,
    pub chunks_tombstoned: usize,
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
        let head_sha = git_head_sha(&self.repo_path)?;
        let head_tree_sha = git_tree_sha(&self.repo_path, "HEAD")?;

        let commits = match cursor.last_commit_sha.as_deref() {
            Some(prev) if prev == head_sha => Vec::new(),
            Some(prev) => git_log_range(&self.repo_path, prev, "HEAD")?,
            None => git_log(&self.repo_path)?,
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
        for commit_info in plan.commits.iter().rev() {
            self.ingest_one_commit(pool, commit_info, &mut report, &mut chunked_this_poll)
                .await?;
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
        let commit_tree = git_tree_sha(&self.repo_path, &commit_info.sha)?;
        let (changed, deleted) = if let Some(parent_sha) = commit_info.parents.first() {
            let parent_tree = git_tree_sha(&self.repo_path, parent_sha)?;
            git_diff_paths(&self.repo_path, &parent_tree, &commit_tree)?
        } else {
            let added: Vec<String> = git_ls_files(&self.repo_path, &commit_info.sha)?
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
            let blob = git_cat_blob(&self.repo_path, &commit_info.sha, path)?;
            let content_sha256: [u8; 32] = blake3::hash(&blob).into();

            let language = crate::chunker::detect_language(path)
                .map(str::to_string)
                .or_else(|| crate::chunker::fallback_language(path).map(str::to_string));
            let rev_payload = FileRevisionV1 {
                repo_id: self.repo_id,
                file_path: path.clone(),
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
                continue;
            }

            // Re-chunk and tombstone any prior indexes that don't
            // appear in the new chunk batch (file content shrunk).
            let chunks = chunk_blob(path, &blob);
            let new_indexes: HashSet<u32> = (0..chunks.len())
                .map(|i| u32::try_from(i).unwrap_or(u32::MAX))
                .collect();
            let prior_indexes =
                present_chunk_indexes(pool, &self.owner, self.repo_id, path).await?;
            for prior in prior_indexes {
                if !new_indexes.contains(&prior) {
                    let tomb = CodeChunkV1 {
                        repo_id: self.repo_id,
                        file_path: path.clone(),
                        chunk_index: prior,
                        text: String::new(),
                        language: language.clone(),
                        chunk_type: "block".into(),
                        byte_range_start: 0,
                        byte_range_end: 0,
                        line_range_start: 0,
                        line_range_end: 0,
                        state: FileState::Tombstone,
                    };
                    ingest_code_chunk(pool, &self.owner, batch_id, &tomb, [0u8; 32], now).await?;
                    report.chunks_tombstoned += 1;
                }
            }

            for (idx, chunk) in chunks.into_iter().enumerate() {
                let payload = CodeChunkV1 {
                    repo_id: self.repo_id,
                    file_path: path.clone(),
                    chunk_index: u32::try_from(idx).unwrap_or(u32::MAX),
                    text: chunk.text,
                    language: chunk.language.map(str::to_string),
                    chunk_type: chunk.chunk_type.to_string(),
                    byte_range_start: chunk.byte_range_start,
                    byte_range_end: chunk.byte_range_end,
                    line_range_start: chunk.line_range_start,
                    line_range_end: chunk.line_range_end,
                    state: FileState::Present,
                };
                ingest_code_chunk(pool, &self.owner, batch_id, &payload, content_sha256, now)
                    .await?;
                report.chunks_emitted += 1;
            }
        }

        // Phase 3 — deletions reported by this commit's diff.
        for path in &deleted {
            let rev_payload = FileRevisionV1 {
                repo_id: self.repo_id,
                file_path: path.clone(),
                language: None,
                content_sha256: [0u8; 32],
                size_bytes: 0,
                indexed_commit_sha: commit_info.sha.clone(),
                state: FileState::Tombstone,
            };
            ingest_file_revision(pool, &self.owner, batch_id, &rev_payload, now).await?;
            report.files_tombstoned += 1;

            let prior_indexes =
                present_chunk_indexes(pool, &self.owner, self.repo_id, path).await?;
            for prior in prior_indexes {
                let tomb = CodeChunkV1 {
                    repo_id: self.repo_id,
                    file_path: path.clone(),
                    chunk_index: prior,
                    text: String::new(),
                    language: None,
                    chunk_type: "block".into(),
                    byte_range_start: 0,
                    byte_range_end: 0,
                    line_range_start: 0,
                    line_range_end: 0,
                    state: FileState::Tombstone,
                };
                ingest_code_chunk(pool, &self.owner, batch_id, &tomb, [0u8; 32], now).await?;
                report.chunks_tombstoned += 1;
            }
        }

        // Close this commit's batch.
        crate::ingest::close_local_git_batch(pool, &self.owner, batch_id).await?;
        Ok(())
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

// ---------------------------------------------------------------------
// Walk plan + git helpers.
// ---------------------------------------------------------------------

#[derive(Debug)]
struct WalkPlan {
    head_sha: String,
    head_tree_sha: String,
    commits: Vec<CommitInfo>,
}

#[derive(Debug, Clone)]
struct CommitInfo {
    sha: String,
    parents: Vec<String>,
    author_name: String,
    author_email: String,
    author_time: time::OffsetDateTime,
    committer_name: String,
    committer_email: String,
    committer_time: time::OffsetDateTime,
    message: String,
}

fn run_git(repo: &Path, args: &[&str]) -> Result<Vec<u8>, IndexError> {
    let out = Command::new("git").arg("-C").arg(repo).args(args).output()?;
    if !out.status.success() {
        return Err(IndexError::Git(format!(
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(out.stdout)
}

fn git_head_sha(repo: &Path) -> Result<String, IndexError> {
    let bytes = run_git(repo, &["rev-parse", "HEAD"])?;
    String::from_utf8(bytes)
        .map(|s| s.trim().to_string())
        .map_err(|_| IndexError::Utf8)
}

fn git_tree_sha(repo: &Path, rev: &str) -> Result<String, IndexError> {
    let bytes = run_git(repo, &["rev-parse", &format!("{rev}^{{tree}}")])?;
    String::from_utf8(bytes)
        .map(|s| s.trim().to_string())
        .map_err(|_| IndexError::Utf8)
}

fn git_log(repo: &Path) -> Result<Vec<CommitInfo>, IndexError> {
    git_log_args(repo, &["log", "--first-parent"])
}

fn git_log_range(repo: &Path, from: &str, to: &str) -> Result<Vec<CommitInfo>, IndexError> {
    git_log_args(
        repo,
        &["log", "--first-parent", &format!("{from}..{to}")],
    )
}

fn git_log_args(repo: &Path, base_args: &[&str]) -> Result<Vec<CommitInfo>, IndexError> {
    let fmt = "%H%x1f%P%x1f%an%x1f%ae%x1f%aI%x1f%cn%x1f%ce%x1f%cI%x1f%B%x1e";
    let fmt_arg = format!("--format={fmt}");
    let mut args: Vec<&str> = base_args.to_vec();
    args.push(&fmt_arg);
    let bytes = run_git(repo, &args)?;
    let text = String::from_utf8(bytes).map_err(|_| IndexError::Utf8)?;
    let mut out = Vec::new();
    for record in text.split('\x1e') {
        let r = record.trim_start_matches('\n');
        if r.is_empty() {
            continue;
        }
        let parts: Vec<&str> = r.splitn(9, '\x1f').collect();
        if parts.len() != 9 {
            return Err(IndexError::Git(format!("malformed log record: {r:?}")));
        }
        let parents: Vec<String> = parts[1].split_whitespace().map(str::to_string).collect();
        let author_time = time::OffsetDateTime::parse(
            parts[4],
            &time::format_description::well_known::Iso8601::DEFAULT,
        )
        .map_err(|e| IndexError::Git(format!("author_time {:?}: {e}", parts[4])))?;
        let committer_time = time::OffsetDateTime::parse(
            parts[7],
            &time::format_description::well_known::Iso8601::DEFAULT,
        )
        .map_err(|e| IndexError::Git(format!("committer_time {:?}: {e}", parts[7])))?;
        out.push(CommitInfo {
            sha: parts[0].to_string(),
            parents,
            author_name: parts[2].to_string(),
            author_email: parts[3].to_string(),
            author_time,
            committer_name: parts[5].to_string(),
            committer_email: parts[6].to_string(),
            committer_time,
            message: parts[8].to_string(),
        });
    }
    Ok(out)
}

fn git_ls_files(repo: &Path, rev: &str) -> Result<Vec<(String, Vec<u8>)>, IndexError> {
    let listing = run_git(repo, &["ls-tree", "-r", "-z", rev])?;
    let listing_str = String::from_utf8(listing).map_err(|_| IndexError::Utf8)?;

    let mut paths = Vec::new();
    for entry in listing_str.split('\0') {
        if entry.is_empty() {
            continue;
        }
        let (header, path) = entry
            .split_once('\t')
            .ok_or_else(|| IndexError::Git(format!("malformed ls-tree entry: {entry:?}")))?;
        let cols: Vec<&str> = header.split_whitespace().collect();
        if cols.len() != 3 || cols[1] != "blob" {
            continue;
        }
        paths.push((path.to_string(), cols[2].to_string()));
    }

    let mut out = Vec::with_capacity(paths.len());
    for (path, oid) in paths {
        let bytes = run_git(repo, &["cat-file", "blob", &oid])?;
        out.push((path, bytes));
    }
    Ok(out)
}

fn git_cat_blob(repo: &Path, rev: &str, path: &str) -> Result<Vec<u8>, IndexError> {
    run_git(repo, &["show", &format!("{rev}:{path}")])
}

/// Returns `(changed_or_added_paths, deleted_paths)` between two
/// tree shas. Uses `--name-status -z`. Renames are reported as
/// delete + add.
fn git_diff_paths(
    repo: &Path,
    from: &str,
    to: &str,
) -> Result<(Vec<String>, Vec<String>), IndexError> {
    let bytes = run_git(repo, &["diff", "--name-status", "-z", from, to])?;
    let text = String::from_utf8(bytes).map_err(|_| IndexError::Utf8)?;
    // -z emits: "<status>\0<path>\0[<path2>\0 if rename/copy]"
    // Status codes: A/M/D for add/modify/delete; R<num>/C<num> for
    // rename/copy with two paths.
    let mut tokens = text.split('\0').filter(|s| !s.is_empty());
    let mut changed = Vec::new();
    let mut deleted = Vec::new();
    while let Some(status) = tokens.next() {
        let primary = tokens
            .next()
            .ok_or_else(|| IndexError::Git(format!("diff missing path after status {status:?}")))?;
        let first_char = status.chars().next().unwrap_or(' ');
        match first_char {
            'A' | 'M' | 'T' => changed.push(primary.to_string()),
            'D' => deleted.push(primary.to_string()),
            'R' | 'C' => {
                let dest = tokens.next().ok_or_else(|| {
                    IndexError::Git(format!("rename {status:?} missing dest path"))
                })?;
                deleted.push(primary.to_string());
                changed.push(dest.to_string());
            }
            _ => {}
        }
    }
    Ok((changed, deleted))
}

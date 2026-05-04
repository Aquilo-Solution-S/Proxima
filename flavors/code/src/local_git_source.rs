#![allow(
    clippy::missing_errors_doc,
    clippy::doc_markdown,
    clippy::too_many_lines
)]
//! `LocalGitSource` — pull-mode indexer over a local git repository.
//!
//! Three phases per `index()`:
//! 1. **Commits.** Walks `git log --first-parent` and emits one
//!    `commit-v1` Fact per commit. Idempotent on `event_id` (BLAKE3 of
//!    `(source_id, owner, payload)`); replaying the same commit is a
//!    no-op.
//! 2. **HEAD index.** For each file at HEAD, computes
//!    `content_sha256` (BLAKE3-32). If a Present `file-revision-v1`
//!    head already carries the same hash, skips. Otherwise emits a new
//!    Present revision and a fresh batch of `code-chunk-v1` Facts (one
//!    per chunk produced by the cAST chunker).
//! 3. **Deletions.** Any file_path that has a Present head but is no
//!    longer in the working tree at HEAD gets a Tombstone revision
//!    plus a Tombstone for every chunk_index it previously published.
//!
//! Uses shell `git` via `std::process::Command`. The host must have
//! `git` on PATH. This trade-off keeps the dep surface minimal —
//! `gix` would more than double our build time. Revisit if/when we
//! need fine-grained programmatic git access (M5+ live polling).

use std::path::{Path, PathBuf};
use std::process::Command;

use proxima_core::{Owner, SourceBatchId};
use sqlx::PgPool;
use uuid::Uuid;

use crate::chunker::chunk_blob;
use crate::ingest::{
    FileRevisionHead, IngestError, file_revision_heads, ingest_code_chunk, ingest_commit,
    ingest_file_revision, present_chunk_indexes,
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
}

/// Counters returned by [`LocalGitSource::index`]. Useful for tests
/// and for the composite binary's terminal report.
#[derive(Debug, Default, Clone)]
pub struct IndexReport {
    pub commits_emitted: usize,
    pub commits_replayed: usize,
    pub files_unchanged: usize,
    pub files_present_emitted: usize,
    pub files_tombstoned: usize,
    pub chunks_emitted: usize,
    pub chunks_tombstoned: usize,
}

/// Pull-mode indexer. One instance per repo; `repo_id` is stable
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
        Self { repo_id, repo_path, owner }
    }

    /// Run all three phases against `pool`. Returns counters.
    pub async fn index(&self, pool: &PgPool) -> Result<IndexReport, IndexError> {
        let mut report = IndexReport::default();
        let head_sha = git_head_sha(&self.repo_path)?;

        // Phase 1 — commits.
        let commits = git_log(&self.repo_path)?;
        let now = time::OffsetDateTime::now_utc();
        let commit_batch = SourceBatchId::new(Uuid::now_v7());
        for c in commits {
            let payload = CommitV1 {
                repo_id: self.repo_id,
                sha: c.sha,
                parents: c.parents,
                author_name: c.author_name,
                author_email: c.author_email,
                author_time: c.author_time,
                committer_name: c.committer_name,
                committer_email: c.committer_email,
                committer_time: c.committer_time,
                message: c.message,
            };
            let outcome = ingest_commit(pool, &self.owner, commit_batch, &payload, now).await?;
            if outcome.idempotent_replay {
                report.commits_replayed += 1;
            } else {
                report.commits_emitted += 1;
            }
        }

        // Phase 2 + 3 — files.
        let on_disk = git_ls_files(&self.repo_path, &head_sha)?;
        let heads = file_revision_heads(pool, &self.owner, self.repo_id).await?;

        let on_disk_paths: std::collections::HashSet<&str> =
            on_disk.iter().map(|(p, _)| p.as_str()).collect();
        let head_by_path: std::collections::HashMap<String, FileRevisionHead> =
            heads.into_iter().map(|h| (h.file_path.clone(), h)).collect();

        let file_batch = SourceBatchId::new(Uuid::now_v7());

        for (path, blob) in &on_disk {
            let content_sha256: [u8; 32] = blake3::hash(blob).into();

            // Skip if unchanged Present head.
            if let Some(h) = head_by_path.get(path)
                && matches!(h.state, FileState::Present)
                && h.content_sha256 == content_sha256
            {
                report.files_unchanged += 1;
                continue;
            }

            // Emit Present file-revision.
            let language = crate::chunker::detect_language(path).map(str::to_string).or_else(|| {
                crate::chunker::fallback_language(path).map(str::to_string)
            });
            let rev_payload = FileRevisionV1 {
                repo_id: self.repo_id,
                file_path: path.clone(),
                language: language.clone(),
                content_sha256,
                size_bytes: blob.len() as u64,
                indexed_commit_sha: head_sha.clone(),
                state: FileState::Present,
            };
            let rev_outcome =
                ingest_file_revision(pool, &self.owner, file_batch, &rev_payload, now).await?;
            report.files_present_emitted += 1;

            // Emit chunks. Tombstone any prior chunk_indexes that don't
            // appear in this fresh chunk batch (file content shrunk).
            let chunks = chunk_blob(path, blob);
            let chunk_batch = SourceBatchId::new(Uuid::now_v7());
            let new_indexes: std::collections::HashSet<u32> =
                (0..chunks.len()).map(|i| u32::try_from(i).unwrap_or(u32::MAX)).collect();

            let prior_indexes = present_chunk_indexes(pool, &self.owner, self.repo_id, path).await?;
            for prior in prior_indexes {
                if !new_indexes.contains(&prior) {
                    let tomb = CodeChunkV1 {
                        repo_id: self.repo_id,
                        file_path: path.clone(),
                        chunk_index: prior,
                        parent_file_revision_id: rev_outcome.memory_id,
                        text: String::new(),
                        language: language.clone(),
                        chunk_type: "block".into(),
                        byte_range_start: 0,
                        byte_range_end: 0,
                        line_range_start: 0,
                        line_range_end: 0,
                        state: FileState::Tombstone,
                    };
                    ingest_code_chunk(pool, &self.owner, chunk_batch, &tomb, now).await?;
                    report.chunks_tombstoned += 1;
                }
            }

            for (idx, chunk) in chunks.into_iter().enumerate() {
                let payload = CodeChunkV1 {
                    repo_id: self.repo_id,
                    file_path: path.clone(),
                    chunk_index: u32::try_from(idx).unwrap_or(u32::MAX),
                    parent_file_revision_id: rev_outcome.memory_id,
                    text: chunk.text,
                    language: chunk.language.map(str::to_string),
                    chunk_type: chunk.chunk_type.to_string(),
                    byte_range_start: chunk.byte_range_start,
                    byte_range_end: chunk.byte_range_end,
                    line_range_start: chunk.line_range_start,
                    line_range_end: chunk.line_range_end,
                    state: FileState::Present,
                };
                ingest_code_chunk(pool, &self.owner, chunk_batch, &payload, now).await?;
                report.chunks_emitted += 1;
            }
        }

        // Phase 3 — deletions: paths that have a Present head but are
        // no longer on disk.
        for (path, h) in &head_by_path {
            if !matches!(h.state, FileState::Present) {
                continue;
            }
            if on_disk_paths.contains(path.as_str()) {
                continue;
            }
            let rev_payload = FileRevisionV1 {
                repo_id: self.repo_id,
                file_path: path.clone(),
                language: None,
                content_sha256: [0u8; 32],
                size_bytes: 0,
                indexed_commit_sha: head_sha.clone(),
                state: FileState::Tombstone,
            };
            let rev_outcome =
                ingest_file_revision(pool, &self.owner, file_batch, &rev_payload, now).await?;
            report.files_tombstoned += 1;

            let prior_indexes = present_chunk_indexes(pool, &self.owner, self.repo_id, path).await?;
            let chunk_batch = SourceBatchId::new(Uuid::now_v7());
            for prior in prior_indexes {
                let tomb = CodeChunkV1 {
                    repo_id: self.repo_id,
                    file_path: path.clone(),
                    chunk_index: prior,
                    parent_file_revision_id: rev_outcome.memory_id,
                    text: String::new(),
                    language: None,
                    chunk_type: "block".into(),
                    byte_range_start: 0,
                    byte_range_end: 0,
                    line_range_start: 0,
                    line_range_end: 0,
                    state: FileState::Tombstone,
                };
                ingest_code_chunk(pool, &self.owner, chunk_batch, &tomb, now).await?;
                report.chunks_tombstoned += 1;
            }
        }

        Ok(report)
    }
}

// ---------------------------------------------------------------------
// Shell-git helpers. Reasonable v1 substitute for a programmatic git
// library; see module docs for the trade-off.
// ---------------------------------------------------------------------

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
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()?;
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

fn git_log(repo: &Path) -> Result<Vec<CommitInfo>, IndexError> {
    // %x1f = ASCII unit separator (per-field), %x1e = record separator.
    let fmt = "%H%x1f%P%x1f%an%x1f%ae%x1f%aI%x1f%cn%x1f%ce%x1f%cI%x1f%B%x1e";
    let bytes = run_git(repo, &["log", "--first-parent", &format!("--format={fmt}")])?;
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
        let parents: Vec<String> = parts[1]
            .split_whitespace()
            .map(str::to_string)
            .collect();
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

fn git_ls_files(repo: &Path, _head_sha: &str) -> Result<Vec<(String, Vec<u8>)>, IndexError> {
    // Snapshot the tree at HEAD: paths + blob bytes. We use ls-tree
    // (NUL-delimited) for the path/oid pairs and `cat-file --batch` to
    // stream the bytes.
    let listing = run_git(repo, &["ls-tree", "-r", "-z", "HEAD"])?;
    let listing_str = String::from_utf8(listing).map_err(|_| IndexError::Utf8)?;

    let mut paths = Vec::new();
    for entry in listing_str.split('\0') {
        if entry.is_empty() {
            continue;
        }
        // "<mode> <type> <oid>\t<path>"
        let (header, path) = entry.split_once('\t').ok_or_else(|| {
            IndexError::Git(format!("malformed ls-tree entry: {entry:?}"))
        })?;
        let cols: Vec<&str> = header.split_whitespace().collect();
        if cols.len() != 3 || cols[1] != "blob" {
            continue; // skip submodules, symlinks, etc.
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

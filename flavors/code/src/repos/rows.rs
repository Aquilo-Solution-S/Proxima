use super::records::{RepoIngestionRun, RepoRecord, RunStage, RunStatus};
use super::scope::RepoScope;
use crate::file_class::FileClassCounts;
use uuid::Uuid;

fn u32_from_i32(v: i32) -> u32 {
    u32::try_from(v).unwrap_or(0)
}

fn usize_from_i32(v: i32) -> usize {
    usize::try_from(u32_from_i32(v)).unwrap_or(usize::MAX)
}

fn class_counts(source: i32, generated: i32, vendored: i32, lockfile: i32) -> FileClassCounts {
    FileClassCounts {
        source: usize_from_i32(source),
        generated: usize_from_i32(generated),
        vendored: usize_from_i32(vendored),
        lockfile: usize_from_i32(lockfile),
    }
}

#[derive(Debug, sqlx::FromRow)]
pub(super) struct RepoRow {
    repo_id: Uuid,
    canonical_path: String,
    display_name: String,
    target_branch: Option<String>,
    last_cursor: Option<Vec<u8>>,
    last_polled_at: Option<time::OffsetDateTime>,
    created_at: time::OffsetDateTime,
    include_globs: Vec<String>,
    exclude_globs: Vec<String>,
}

#[derive(Debug, sqlx::FromRow)]
pub(super) struct RunRow {
    run_id: Uuid,
    repo_id: Uuid,
    status: RunStatus,
    stage: RunStage,
    commits_emitted: i32,
    files_emitted: i32,
    chunks_emitted: i32,
    chunks_reused: i32,
    chunks_tombstoned: i32,
    ast_edges_emitted: i32,
    abstractions_emitted: i32,
    embeddings_landed: i32,
    citations_emitted: i32,
    files_source: i32,
    files_generated: i32,
    files_vendored: i32,
    files_lockfile: i32,
    chunks_source: i32,
    chunks_generated: i32,
    chunks_vendored: i32,
    chunks_lockfile: i32,
    error_message: Option<String>,
    started_at: time::OffsetDateTime,
    updated_at: time::OffsetDateTime,
    finished_at: Option<time::OffsetDateTime>,
}

impl From<RunRow> for RepoIngestionRun {
    fn from(row: RunRow) -> Self {
        Self {
            run_id: row.run_id,
            repo_id: row.repo_id,
            status: row.status,
            stage: row.stage,
            commits_emitted: u32_from_i32(row.commits_emitted),
            files_emitted: u32_from_i32(row.files_emitted),
            chunks_emitted: u32_from_i32(row.chunks_emitted),
            chunks_reused: u32_from_i32(row.chunks_reused),
            chunks_tombstoned: u32_from_i32(row.chunks_tombstoned),
            ast_edges_emitted: u32_from_i32(row.ast_edges_emitted),
            abstractions_emitted: u32_from_i32(row.abstractions_emitted),
            embeddings_landed: u32_from_i32(row.embeddings_landed),
            citations_emitted: u32_from_i32(row.citations_emitted),
            files_by_class: class_counts(
                row.files_source,
                row.files_generated,
                row.files_vendored,
                row.files_lockfile,
            ),
            chunks_by_class: class_counts(
                row.chunks_source,
                row.chunks_generated,
                row.chunks_vendored,
                row.chunks_lockfile,
            ),
            error_message: row.error_message,
            started_at: row.started_at,
            updated_at: row.updated_at,
            finished_at: row.finished_at,
        }
    }
}

impl From<RepoRow> for RepoRecord {
    fn from(row: RepoRow) -> Self {
        Self {
            repo_id: row.repo_id,
            canonical_path: row.canonical_path,
            display_name: row.display_name,
            target_branch: row.target_branch,
            last_cursor: row.last_cursor,
            last_polled_at: row.last_polled_at,
            created_at: row.created_at,
            scope: RepoScope {
                include: row.include_globs,
                exclude: row.exclude_globs,
            },
        }
    }
}

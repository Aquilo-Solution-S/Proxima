use proxima_core::{MemoryId, Owner};
use sqlx::PgPool;

use crate::payloads::FileState;

use super::IngestError;

/// One head row from a NK-scoped sidecar query.
#[derive(Debug, Clone)]
pub struct FileRevisionHead {
    pub memory_id: MemoryId,
    pub file_path: String,
    pub content_sha256: [u8; 32],
    pub state: FileState,
}

/// Read all current file-revision-v1 heads for a `(owner, repo_id)`
/// scope. Used by `LocalGitSource` to detect deletions and skip
/// re-ingestion of unchanged files.
pub async fn file_revision_heads(
    pool: &PgPool,
    owner: &Owner,
    repo_id: uuid::Uuid,
) -> Result<Vec<FileRevisionHead>, IngestError> {
    let (kind, principal_id) = owner.columns();

    // DISTINCT ON over (file_path) ordered by created_at DESC picks
    // the latest revision per NK in a single index scan. Replaces a
    // correlated NOT EXISTS whose inner anti-join cost grew linearly
    // with versions-per-NK and went quadratic on long replay sessions
    // (see perf-logs/2026-05-16_14-31-40: 9 sqlx slow-statement
    // warnings, 1.5s → 26s as history accumulated).
    let rows: Vec<(uuid::Uuid, String, Vec<u8>, FileState)> = sqlx::query_as(
        "SELECT memory_id, file_path, content_sha256, state \
         FROM ( \
             SELECT DISTINCT ON (s.file_path) \
                 m.memory_id, s.file_path, s.content_sha256, s.state \
             FROM proxima_core.memories m \
             JOIN proxima_code.file_revision_v1 s USING (memory_id) \
             WHERE m.owner_principal_kind = $1 \
               AND m.owner_principal_id = $2 \
               AND s.repo_id = $3 \
             ORDER BY s.file_path, m.created_at DESC \
         ) latest",
    )
    .bind(kind)
    .bind(principal_id)
    .bind(repo_id)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(mid, fp, hash, state)| {
            let mut h = [0u8; 32];
            let n = hash.len().min(32);
            h[..n].copy_from_slice(&hash[..n]);
            FileRevisionHead {
                memory_id: MemoryId::new(mid),
                file_path: fp,
                content_sha256: h,
                state,
            }
        })
        .collect())
}

/// Read all chunk_indexes that currently have a Present head under
/// `(owner, repo_id, file_path)`. Used by `LocalGitSource` to emit
/// per-chunk Tombstones when a file is deleted.
pub async fn present_chunk_indexes(
    pool: &PgPool,
    owner: &Owner,
    repo_id: uuid::Uuid,
    file_path: &str,
) -> Result<Vec<u32>, IngestError> {
    let (kind, principal_id) = owner.columns();

    // DISTINCT ON per (chunk_index) finds the latest head per NK in a
    // single pass; we then filter to Present so tombstoned-latest
    // chunks fall away. The previous NOT EXISTS variant ran a nested
    // anti-join per candidate row and was the dominant slow statement
    // in long replay sessions (94s / 4145 calls in
    // perf-logs/2026-05-16_14-31-40).
    let rows: Vec<(i32,)> = sqlx::query_as(
        "SELECT chunk_index \
         FROM ( \
             SELECT DISTINCT ON (s.chunk_index) \
                 s.chunk_index, s.state \
             FROM proxima_core.memories m \
             JOIN proxima_code.code_chunk_v1 s USING (memory_id) \
             WHERE m.owner_principal_kind = $1 \
               AND m.owner_principal_id = $2 \
               AND s.repo_id = $3 \
               AND s.file_path = $4 \
             ORDER BY s.chunk_index, m.created_at DESC \
         ) latest \
         WHERE latest.state = 'Present'",
    )
    .bind(kind)
    .bind(principal_id)
    .bind(repo_id)
    .bind(file_path)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(i,)| u32::try_from(i).unwrap_or(0))
        .collect())
}

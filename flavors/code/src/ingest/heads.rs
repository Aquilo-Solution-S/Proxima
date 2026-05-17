use proxima_core::{MemoryId, Owner, OwnerPrincipalKind};
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
    use proxima_core::Principal;
    let kind = OwnerPrincipalKind::of(&owner.principal);
    let principal_id = match &owner.principal {
        Principal::User(u) => u.into_inner(),
        Principal::Group(g) => g.into_inner(),
    };
    let org_id = owner.org_id.into_inner();

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
               AND m.owner_org_id = $3 \
               AND s.repo_id = $4 \
             ORDER BY s.file_path, m.created_at DESC \
         ) latest",
    )
    .bind(kind)
    .bind(principal_id)
    .bind(org_id)
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
    use proxima_core::Principal;
    let kind = OwnerPrincipalKind::of(&owner.principal);
    let principal_id = match &owner.principal {
        Principal::User(u) => u.into_inner(),
        Principal::Group(g) => g.into_inner(),
    };
    let org_id = owner.org_id.into_inner();

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
               AND m.owner_org_id = $3 \
               AND s.repo_id = $4 \
               AND s.file_path = $5 \
             ORDER BY s.chunk_index, m.created_at DESC \
         ) latest \
         WHERE latest.state = 'Present'",
    )
    .bind(kind)
    .bind(principal_id)
    .bind(org_id)
    .bind(repo_id)
    .bind(file_path)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(i,)| u32::try_from(i).unwrap_or(0))
        .collect())
}

/// Look up the current Present `code-chunk-v1` head at
/// `(owner, repo_id, file_path, chunk_index)` and return its
/// `memory_id` iff its stored `text` matches `text_to_match`.
///
/// Used by `LocalGitSource` to skip re-emission of a chunk Fact when
/// the same logical chunk re-appears at a later commit with the same
/// content but at a shifted file byte range. The substrate's
/// content-derived `event_id` would otherwise mint a fresh
/// `memory_id` (the chunk Fact payload includes byte/line ranges,
/// which shift even when the text is identical), causing typed
/// `code/calls` edges to fan out one-per-commit instead of
/// one-per-call-site. Reusing the existing memory_id keeps both the
/// chunk-Fact set and the edge set stable across commits.
///
/// Position-drift trade-off: when a chunk's text is unchanged but
/// it's now at a different `chunk_index` (something added above),
/// the lookup at the new index misses → a fresh Fact is emitted.
/// That's correct: chunk_index is part of the NK and `index drift`
/// is a real change to the chunk's identity within the file.
pub async fn lookup_present_chunk_memory_id_by_text(
    pool: &PgPool,
    owner: &Owner,
    repo_id: uuid::Uuid,
    file_path: &str,
    chunk_index: u32,
    text_to_match: &str,
) -> Result<Option<MemoryId>, IngestError> {
    use proxima_core::Principal;
    let kind = OwnerPrincipalKind::of(&owner.principal);
    let principal_id = match &owner.principal {
        Principal::User(u) => u.into_inner(),
        Principal::Group(g) => g.into_inner(),
    };
    let org_id = owner.org_id.into_inner();

    // NK is fully constrained (repo_id + file_path + chunk_index),
    // so the latest row is just ORDER BY created_at DESC LIMIT 1. The
    // outer filter (state = Present AND text matches) rejects the
    // latest row if it doesn't match — that preserves the original
    // semantics: a tombstoned head shouldn't dedup to a Present chunk.
    let row: Option<(uuid::Uuid,)> = sqlx::query_as(
        "SELECT memory_id \
         FROM ( \
             SELECT m.memory_id, s.text, s.state \
             FROM proxima_core.memories m \
             JOIN proxima_code.code_chunk_v1 s USING (memory_id) \
             WHERE m.owner_principal_kind = $1 \
               AND m.owner_principal_id = $2 \
               AND m.owner_org_id = $3 \
               AND s.repo_id = $4 \
               AND s.file_path = $5 \
               AND s.chunk_index = $6 \
             ORDER BY m.created_at DESC \
             LIMIT 1 \
         ) latest \
         WHERE latest.state = 'Present' \
           AND latest.text = $7",
    )
    .bind(kind)
    .bind(principal_id)
    .bind(org_id)
    .bind(repo_id)
    .bind(file_path)
    .bind(i32::try_from(chunk_index).unwrap_or(i32::MAX))
    .bind(text_to_match)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|(mid,)| MemoryId::new(mid)))
}

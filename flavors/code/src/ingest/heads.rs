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
    use proxima_core::Principal;
    let (kind, principal_id) = match &owner.principal {
        Principal::User(u) => ("User", u.into_inner()),
        Principal::Group(g) => ("Group", g.into_inner()),
    };
    let org_id = owner.org_id.into_inner();

    let rows: Vec<(uuid::Uuid, String, Vec<u8>, String)> = sqlx::query_as(
        "SELECT m.memory_id, s.file_path, s.content_sha256, s.state \
         FROM proxima_core.memories m \
         JOIN proxima_code.file_revision_v1 s USING (memory_id) \
         WHERE m.owner_principal_kind = $1 \
           AND m.owner_principal_id = $2 \
           AND m.owner_org_id = $3 \
           AND s.repo_id = $4 \
           AND NOT EXISTS ( \
                 SELECT 1 FROM proxima_core.memories m2 \
                 JOIN proxima_code.file_revision_v1 s2 USING (memory_id) \
                 WHERE m2.schema_id = m.schema_id \
                   AND m2.owner_principal_kind = m.owner_principal_kind \
                   AND m2.owner_principal_id = m.owner_principal_id \
                   AND m2.owner_org_id = m.owner_org_id \
                   AND s2.repo_id = s.repo_id \
                   AND s2.file_path = s.file_path \
                   AND m2.created_at > m.created_at \
           )",
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
                state: match state.as_str() {
                    "Tombstone" => FileState::Tombstone,
                    _ => FileState::Present,
                },
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
    let (kind, principal_id) = match &owner.principal {
        Principal::User(u) => ("User", u.into_inner()),
        Principal::Group(g) => ("Group", g.into_inner()),
    };
    let org_id = owner.org_id.into_inner();

    let rows: Vec<(i32,)> = sqlx::query_as(
        "SELECT s.chunk_index \
         FROM proxima_core.memories m \
         JOIN proxima_code.code_chunk_v1 s USING (memory_id) \
         WHERE m.owner_principal_kind = $1 \
           AND m.owner_principal_id = $2 \
           AND m.owner_org_id = $3 \
           AND s.repo_id = $4 \
           AND s.file_path = $5 \
           AND s.state = 'Present' \
           AND NOT EXISTS ( \
                 SELECT 1 FROM proxima_core.memories m2 \
                 JOIN proxima_code.code_chunk_v1 s2 USING (memory_id) \
                 WHERE m2.schema_id = m.schema_id \
                   AND m2.owner_principal_kind = m.owner_principal_kind \
                   AND m2.owner_principal_id = m.owner_principal_id \
                   AND m2.owner_org_id = m.owner_org_id \
                   AND s2.repo_id = s.repo_id \
                   AND s2.file_path = s.file_path \
                   AND s2.chunk_index = s.chunk_index \
                   AND m2.created_at > m.created_at \
           )",
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
    let (kind, principal_id) = match &owner.principal {
        Principal::User(u) => ("User", u.into_inner()),
        Principal::Group(g) => ("Group", g.into_inner()),
    };
    let org_id = owner.org_id.into_inner();

    let row: Option<(uuid::Uuid,)> = sqlx::query_as(
        "SELECT m.memory_id \
         FROM proxima_core.memories m \
         JOIN proxima_code.code_chunk_v1 s USING (memory_id) \
         WHERE m.owner_principal_kind = $1 \
           AND m.owner_principal_id = $2 \
           AND m.owner_org_id = $3 \
           AND s.repo_id = $4 \
           AND s.file_path = $5 \
           AND s.chunk_index = $6 \
           AND s.state = 'Present' \
           AND s.text = $7 \
           AND NOT EXISTS ( \
                 SELECT 1 FROM proxima_core.memories m2 \
                 JOIN proxima_code.code_chunk_v1 s2 USING (memory_id) \
                 WHERE m2.schema_id = m.schema_id \
                   AND m2.owner_principal_kind = m.owner_principal_kind \
                   AND m2.owner_principal_id = m.owner_principal_id \
                   AND m2.owner_org_id = m.owner_org_id \
                   AND s2.repo_id = s.repo_id \
                   AND s2.file_path = s.file_path \
                   AND s2.chunk_index = s.chunk_index \
                   AND m2.created_at > m.created_at \
           ) \
         LIMIT 1",
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

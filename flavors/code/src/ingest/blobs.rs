use proxima_core::verbs::event_ingest::EventIngestOutcome;
use proxima_core::{FactPayload, Owner, SourceBatchId};
use proxima_storage_pg::verbs::event_ingest::ingest_event_in_tx;
use sqlx::PgPool;

use crate::payloads::{CodeChunkV1, CommitV1, FileRevisionV1, FileState};

use super::IngestError;
use super::draft::{Citation, make_draft};
use super::schemas::{
    CODE_BLOB_BYTE_RANGE_SCHEMA, CODE_BLOB_SCHEMA, CODE_BLOB_WHOLE_SCHEMA,
    CODE_COMMIT_OBJECT_SCHEMA, CODE_COMMIT_WHOLE_SCHEMA,
};

/// Close a `source_batch` opened by the typed-ingest helpers under a
/// LocalGitSource poll. Idempotent. Maps `NotFound` to `Ok(())` so
/// callers can safely call this after no-op polls (no events → no
/// batch row was ever inserted).
pub async fn close_local_git_batch(
    pool: &PgPool,
    owner: &Owner,
    source_batch_id: SourceBatchId,
) -> Result<(), IngestError> {
    match proxima_storage_pg::verbs::close_batch::close_batch(pool, owner, source_batch_id).await {
        Ok(_) | Err(proxima_core::StorageError::NotFound) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Atomic Fact + sidecar write for `commit-v1`. Cites the commit
/// object (keyed by blake3 of the commit sha) with a "whole-commit"
/// CitationMapping.
pub async fn ingest_commit(
    pool: &PgPool,
    owner: &Owner,
    source_batch_id: SourceBatchId,
    payload: &CommitV1,
    observed_at: time::OffsetDateTime,
) -> Result<EventIngestOutcome, IngestError> {
    let draft = make_draft(
        owner,
        source_batch_id,
        payload,
        CommitV1::SCHEMA_ID,
        Citation {
            cited_object_schema: CODE_COMMIT_OBJECT_SCHEMA,
            content_hash: blake3::hash(payload.sha.as_bytes()).into(),
            mapping_schema: CODE_COMMIT_WHOLE_SCHEMA,
        },
        observed_at,
    )?;

    let mut tx = pool.begin().await?;
    let outcome = ingest_event_in_tx(&mut tx, &draft).await?;
    if !outcome.idempotent_replay {
        sqlx::query(
            "INSERT INTO proxima_code.commit_v1 \
                (memory_id, repo_id, sha, parents, author_name, author_email, \
                 author_time, committer_name, committer_email, committer_time, \
                 message) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
        )
        .bind(outcome.memory_id.into_inner())
        .bind(payload.repo_id)
        .bind(&payload.sha)
        .bind(&payload.parents)
        .bind(&payload.author_name)
        .bind(&payload.author_email)
        .bind(payload.author_time)
        .bind(&payload.committer_name)
        .bind(&payload.committer_email)
        .bind(payload.committer_time)
        .bind(&payload.message)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(outcome)
}

/// Atomic Fact + sidecar write for `file-revision-v1`. Cites the
/// file blob (keyed by `content_sha256`) with a "whole-blob"
/// CitationMapping. Tombstones cite the null blob (`[0u8; 32]`).
pub async fn ingest_file_revision(
    pool: &PgPool,
    owner: &Owner,
    source_batch_id: SourceBatchId,
    payload: &FileRevisionV1,
    observed_at: time::OffsetDateTime,
) -> Result<EventIngestOutcome, IngestError> {
    let draft = make_draft(
        owner,
        source_batch_id,
        payload,
        FileRevisionV1::SCHEMA_ID,
        Citation {
            cited_object_schema: CODE_BLOB_SCHEMA,
            content_hash: payload.content_sha256,
            mapping_schema: CODE_BLOB_WHOLE_SCHEMA,
        },
        observed_at,
    )?;

    let mut tx = pool.begin().await?;
    let outcome = ingest_event_in_tx(&mut tx, &draft).await?;
    if !outcome.idempotent_replay {
        let state_text = match payload.state {
            FileState::Present => "Present",
            FileState::Tombstone => "Tombstone",
        };
        sqlx::query(
            "INSERT INTO proxima_code.file_revision_v1 \
                (memory_id, repo_id, file_path, language, content_sha256, \
                 size_bytes, indexed_commit_sha, state) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
        )
        .bind(outcome.memory_id.into_inner())
        .bind(payload.repo_id)
        .bind(&payload.file_path)
        .bind(payload.language.as_deref())
        .bind(&payload.content_sha256[..])
        .bind(i64::try_from(payload.size_bytes).unwrap_or(i64::MAX))
        .bind(&payload.indexed_commit_sha)
        .bind(state_text)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(outcome)
}

/// Atomic Fact + sidecar write for `code-chunk-v1`. Cites the parent
/// blob (the same `cited_object_id` as the chunk's parent
/// `file-revision-v1`, by way of the substrate's UNIQUE on
/// `(owner, schema_id, content_hash)`) with a "byte-range"
/// CitationMapping. Tombstone chunks cite the null blob (`[0u8; 32]`).
///
/// `parent_blob_sha256` is the caller's responsibility — for Present
/// chunks it's the blob the chunker just operated on; for Tombstones
/// it's `[0u8; 32]`.
pub async fn ingest_code_chunk(
    pool: &PgPool,
    owner: &Owner,
    source_batch_id: SourceBatchId,
    payload: &CodeChunkV1,
    parent_blob_sha256: [u8; 32],
    observed_at: time::OffsetDateTime,
) -> Result<EventIngestOutcome, IngestError> {
    let draft = make_draft(
        owner,
        source_batch_id,
        payload,
        CodeChunkV1::SCHEMA_ID,
        Citation {
            cited_object_schema: CODE_BLOB_SCHEMA,
            content_hash: parent_blob_sha256,
            mapping_schema: CODE_BLOB_BYTE_RANGE_SCHEMA,
        },
        observed_at,
    )?;

    let mut tx = pool.begin().await?;
    let outcome = ingest_event_in_tx(&mut tx, &draft).await?;
    if !outcome.idempotent_replay {
        let state_text = match payload.state {
            FileState::Present => "Present",
            FileState::Tombstone => "Tombstone",
        };
        sqlx::query(
            "INSERT INTO proxima_code.code_chunk_v1 \
                (memory_id, repo_id, file_path, chunk_index, \
                 text, language, chunk_type, byte_range_start, byte_range_end, \
                 line_range_start, line_range_end, state) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)",
        )
        .bind(outcome.memory_id.into_inner())
        .bind(payload.repo_id)
        .bind(&payload.file_path)
        .bind(i32::try_from(payload.chunk_index).unwrap_or(i32::MAX))
        .bind(&payload.text)
        .bind(payload.language.as_deref())
        .bind(&payload.chunk_type)
        .bind(i64::from(payload.byte_range_start))
        .bind(i64::from(payload.byte_range_end))
        .bind(i64::from(payload.line_range_start))
        .bind(i64::from(payload.line_range_end))
        .bind(state_text)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(outcome)
}

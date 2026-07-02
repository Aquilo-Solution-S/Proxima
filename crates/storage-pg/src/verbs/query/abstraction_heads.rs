//! Backend-owned candidate narrowing for Abstraction-kind sidecar schemas
//! that opt out of the `memories.supersedes` chain (each derived row is
//! tied 1:1 to its exact source Fact rather than declaring an explicit
//! successor — e.g. `proxima-code/code-chunk-v1`, whose ingest always
//! inserts a fresh row per source file revision; see
//! `flavors/code/src/ingest/blobs.rs::append_code_slice`).
//!
//! [`authorized_code_chunk_head_candidates`] deduplicates a bounded
//! candidate id list to the most-recently-authored row
//! (`source_batch_id`-max) per `(repo_id, file_path, chunk_index)`. Rows
//! owned by [`proxima_core::OwnerRefKind::World`] are considered alongside
//! the caller's own owner scope; this call only narrows candidates before
//! visibility is decided by the caller's subsequent authorized read
//! (docs/14 §"Query"). It exists so `flavors/code` never needs to embed a
//! `proxima_core.*` join to get this narrowing itself.
//!
//! Deliberately not generalized to an arbitrary sidecar table / natural
//! key: `proxima-code/code-chunk-v1` is the only schema that needs this
//! today, and a compile-time query text keeps this call out of
//! `scripts/check-sql-policy.py`'s dynamic-SQL inventory (every value,
//! including `schema_id`, is still `$`-bound, never spliced). A second
//! consumer should get its own narrowly-scoped sibling function (or, if a
//! third shows up, it is worth generalizing and updating the ratchet).

use proxima_core::{Owner, SchemaId, StorageError};
use sqlx::PgPool;

/// Narrow `candidate_ids` (already known to be `schema_id`'s
/// `proxima_code.code_chunk_v1` sidecar rows, from a flavor's own
/// `proxima_code.*`-only query) to the subset not superseded, within the
/// same schema/owner-or-World scope, by a later `source_batch_id` row
/// sharing the same `(repo_id, file_path, chunk_index)`.
///
/// # Errors
///
/// Returns `StorageError::Internal` on query failure.
pub async fn authorized_code_chunk_head_candidates(
    pool: &PgPool,
    owner: Owner,
    schema_id: &SchemaId,
    candidate_ids: &[uuid::Uuid],
) -> Result<Vec<uuid::Uuid>, StorageError> {
    if candidate_ids.is_empty() {
        return Ok(Vec::new());
    }
    let (owner_kind, owner_id) = owner.columns();
    sqlx::query_scalar(
        "SELECT c.memory_id
           FROM proxima_code.code_chunk_v1 c
           JOIN proxima_core.memories m USING (memory_id)
          WHERE m.schema_id = $1
            AND m.memory_id = ANY($2::uuid[])
            AND (
                (m.owner_kind = $3 AND m.owner_id IS NOT DISTINCT FROM $4)
                OR m.owner_kind = 'world'::proxima_core.owner_ref_kind
            )
            AND NOT EXISTS (
                SELECT 1
                  FROM proxima_core.memories m2
                  JOIN proxima_code.code_chunk_v1 c2 USING (memory_id)
                 WHERE m2.schema_id = m.schema_id
                   AND m2.owner_kind = m.owner_kind
                   AND m2.owner_id IS NOT DISTINCT FROM m.owner_id
                   AND c2.repo_id = c.repo_id
                   AND c2.file_path = c.file_path
                   AND c2.chunk_index = c.chunk_index
                   AND m2.source_batch_id > m.source_batch_id
            )",
    )
    .bind(schema_id.as_str())
    .bind(candidate_ids)
    .bind(owner_kind)
    .bind(owner_id)
    .fetch_all(pool)
    .await
    .map_err(|err| StorageError::Internal(err.to_string()))
}

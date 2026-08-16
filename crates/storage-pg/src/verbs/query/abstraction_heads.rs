//! Backend-owned candidate narrowing for Abstraction-kind sidecar schemas
//! that opt out of the `memories.supersedes` chain (each derived row is
//! tied 1:1 to its exact source Fact rather than declaring an explicit
//! successor — e.g. `proxima-code/code-chunk-v1`, whose ingest always
//! inserts a fresh row per source file revision; see
//! `flavors/code/src/ingest/blobs.rs::append_code_slice`).
//!
//! [`authorized_code_chunk_head_candidates`] is phase 2 of code search:
//! admit a sidecar-only hit list to the current `memory_head` row per
//! `(repo_id, file_path, chunk_index)` in the caller-or-World owner
//! scope. Visibility is decided later by the authorized payload read.
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
    let owner_id = owner.stored_owner_id();
    let world_id = proxima_core::OwnerRef::World.stored_owner_id();
    sqlx::query_scalar(
        "SELECT c.t
           FROM proxima_code.code_chunk_v1 c
           JOIN proxima_core.memory m ON m.t = c.t
           JOIN proxima_core.memory_head h ON h.handle = m.handle AND h.t = m.t
          WHERE h.schema_id = $1
            AND c.t = ANY($2::uuid[])
            AND m.owner_id IN ($3, $4)
            AND NOT EXISTS (
                SELECT 1
                  FROM proxima_code.code_chunk_v1 c2
                  JOIN proxima_core.memory m2 ON m2.t = c2.t
                  JOIN proxima_core.memory_head h2
                    ON h2.handle = m2.handle AND h2.t = m2.t
                 WHERE h2.schema_id = h.schema_id
                   AND m2.owner_id = m.owner_id
                   AND c2.repo_id = c.repo_id
                   AND c2.file_path = c.file_path
                   AND c2.chunk_index = c.chunk_index
                   AND m2.t > m.t
            )",
    )
    .bind(schema_id.as_str())
    .bind(candidate_ids)
    .bind(owner_id)
    .bind(world_id)
    .fetch_all(pool)
    .await
    .map_err(crate::error::map_err)
}

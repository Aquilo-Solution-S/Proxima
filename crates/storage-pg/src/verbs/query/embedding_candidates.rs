//! Shared chunk candidate budget and per-memory cosine reranking.

use proxima_core::{SpaceVector, StorageError};
use sqlx::PgConnection;

use crate::error::map_err;
use crate::pgvector::Lane;

/// Maximum chunk rows considered by a semantic candidate scan.
pub(super) const SEMANTIC_SCAN_CAP: u32 = 1_000;

/// Expand a full chunk window when repeated memories spent its slots.
/// Exhaustion and the scan cap terminate even when fewer memories exist.
pub(super) fn next_chunk_window(
    window: u32,
    fetched: usize,
    distinct: usize,
    target: u32,
) -> Option<u32> {
    let fetched = u32::try_from(fetched).unwrap_or(u32::MAX);
    let distinct = u32::try_from(distinct).unwrap_or(u32::MAX);
    (fetched >= window && distinct < target && window < SEMANTIC_SCAN_CAP)
        .then(|| window.saturating_mul(2).min(SEMANTIC_SCAN_CAP))
}

#[derive(sqlx::FromRow)]
pub(super) struct EmbeddingScore {
    pub(super) memory_id: uuid::Uuid,
    pub(super) similarity_score: f32,
}

/// Score each candidate by every chunk in its current complete version.
/// HNSW's width lanes select candidates; full-precision stored vectors
/// determine the score, including for lanes indexed through `halfvec`.
/// Undefined zero-vector cosines do not contribute; the public score
/// retains its existing `[0, 1]` bounds.
pub(super) async fn best_embedding_scores_on_connection(
    connection: &mut PgConnection,
    owner_ids: &[uuid::Uuid],
    query: &SpaceVector,
    candidates: &[uuid::Uuid],
) -> Result<Vec<EmbeddingScore>, StorageError> {
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_as(
        "SELECT emb.entity_id AS memory_id,
                LEAST(1.0, GREATEST(0.0,
                    MAX(NULLIF(1 - (emb.vec <=> $5::vector), 'NaN'::float8))))::real
                    AS similarity_score
           FROM proxima_core.embeddings emb
           JOIN proxima_core.embedding_heads head
             ON head.entity_id = emb.entity_id
            AND head.model_id = emb.model_id
            AND head.dim = emb.dim
            AND head.embedding_version = emb.embedding_version
          WHERE emb.entity_id = ANY($1::uuid[])
            AND emb.owner_id = ANY($2::uuid[])
            AND emb.model_id = $3
            AND emb.dim = $4
          GROUP BY emb.entity_id
          ORDER BY similarity_score DESC, emb.entity_id DESC",
    )
    .bind(candidates)
    .bind(owner_ids)
    .bind(query.space().model_id())
    .bind(Lane::of(query.space().dim()).width)
    .bind(crate::pgvector::literal(query.values()))
    .fetch_all(&mut *connection)
    .await
    .map_err(map_err)
}

//! Backend-owned nearest-neighbour candidate scan for
//! `proxima-code/code-chunk-v1` chunks.
//!
//! Fixed compile-time query: nearest `code-chunk-v1` memories to a
//! query vector among rows matching the structural filters. Not a
//! generalized sidecar/NK helper.
//!
//! It exists because `flavors/code` may not embed a `proxima_core.*` join
//! (`scripts/check-architecture-guardrails.py`), and the embeddings a
//! semantic arm needs live in `proxima_core.embeddings`. The flavor keeps
//! the parts that are its own business (which repo, which language, how to
//! fuse the result with its lexical bands); the backend owns the vector
//! join.
//!
//! **This returns candidates, not results.** Every id it emits still
//! goes through the caller's authorized payload read (Query `HeadsOnly`),
//! exactly like the lexical candidates it is merged with. Nothing here
//! decides visibility. Owner scope is `embeddings.owner_id = $1`.

use proxima_core::{Owner, StorageError};
use sqlx::PgPool;

use crate::error::map_err;
use crate::pgvector::set_hnsw_search_sql;
use crate::tuning::PgTuning;

/// One vec per `(entity_id, model_id, embedding_version)`. The head join
/// already picks the current version; there is nothing to DISTINCT ON.
const NEAREST_CODE_CHUNK_SQL: &str = "SELECT emb.entity_id AS memory_id,
                            GREATEST(0.0, (1 - (emb.vec <=> $4::vector)))::real
                                AS similarity_score
                       FROM proxima_core.embeddings emb
                       JOIN proxima_core.embedding_heads head
                         ON head.entity_id = emb.entity_id
                        AND head.model_id = emb.model_id
                        AND head.embedding_version = emb.embedding_version
                       JOIN proxima_code.code_chunk_v1 c
                         ON c.t = emb.entity_id
                      WHERE emb.owner_id = $1
                        AND emb.model_id = $3
                        AND c.state = 'Present'
                        AND ($2::uuid IS NULL OR c.repo_id = $2)
                        AND ($5::text IS NULL OR c.language = $5)
                        AND ($6::text IS NULL OR c.chunk_type = $6)
                      ORDER BY emb.vec <=> $4::vector
                      LIMIT $7";

/// One chunk memory and its cosine similarity to the query vector.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct CodeChunkVectorCandidate {
    pub memory_id: uuid::Uuid,
    pub similarity_score: f32,
}

/// Nearest `limit` `code-chunk-v1` chunk memories to `query_embedding`,
/// restricted to `owner`'s own scope and to chunks matching the structural
/// filters, best-first.
///
/// `repo_id`, `language` and `chunk_type` are the same optional filters
/// `proxima-code_search_chunks` applies to its lexical scan; pushing them
/// into this query rather than filtering afterwards is what keeps the
/// nearest-neighbour budget spent on rows the caller can actually use. A
/// search scoped to one repository otherwise spends its whole `limit` on
/// the largest repository indexed and returns nothing.
///
/// One row per memory: one vec per head version. `ORDER BY distance LIMIT n`
/// is the shape the HNSW index can serve.
///
/// # Errors
///
/// Returns `StorageError::Internal` on query failure.
pub async fn nearest_code_chunk_candidates(
    pool: &PgPool,
    tuning: &PgTuning,
    owner: Owner,
    model_id: &str,
    query_embedding: &[f32],
    filters: CodeChunkVectorFilters<'_>,
    limit: i64,
) -> Result<Vec<CodeChunkVectorCandidate>, StorageError> {
    if limit <= 0 {
        return Ok(Vec::new());
    }
    if query_embedding.len() != proxima_core::llm::EMBEDDING_DIM {
        return Err(StorageError::ConstraintViolation(format!(
            "semantic chunk search embedding length must be {}",
            proxima_core::llm::EMBEDDING_DIM
        )));
    }
    let owner_id = owner.stored_owner_id();

    // Content scan: embeddings ⋈ flavor sidecar. Owner lives on embeddings.
    // Admit (memory_head / NK) happens after merge in search_chunks.
    let query = sqlx::query_as::<_, CodeChunkVectorCandidate>(NEAREST_CODE_CHUNK_SQL)
        .bind(owner_id)
        .bind(filters.repo_id)
        .bind(model_id)
        .bind(crate::pgvector::literal(query_embedding))
        .bind(filters.language)
        .bind(filters.chunk_type)
        .bind(limit);

    let mut tx = pool.begin().await.map_err(map_err)?;
    // SQL-POLICY: fixed-fragment
    sqlx::raw_sql(sqlx::AssertSqlSafe(set_hnsw_search_sql(tuning)))
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;
    let rows = query.fetch_all(&mut *tx).await.map_err(map_err)?;
    tx.commit().await.map_err(map_err)?;
    Ok(rows)
}

/// The structural filters a chunk search applies before ranking. Grouped
/// into one struct so the neighbour scan does not grow a fourth and fifth
/// bare `Option<&str>` parameter that call sites can silently transpose.
#[derive(Debug, Default, Clone, Copy)]
pub struct CodeChunkVectorFilters<'a> {
    pub repo_id: Option<uuid::Uuid>,
    pub language: Option<&'a str>,
    pub chunk_type: Option<&'a str>,
}

#[cfg(test)]
mod tests {
    #[test]
    fn nearest_chunk_sql_is_one_vec_per_head() {
        let sql = super::NEAREST_CODE_CHUNK_SQL;
        let distinct = format!("{} {}", "DISTINCT", "ON");
        assert!(
            !sql.contains(&distinct),
            "v008 embeddings have one vec per version"
        );
    }
}

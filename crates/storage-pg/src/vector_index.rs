//! Where a store keeps its chunk vectors: its own pgvector index, or a host
//! index behind [`VectorIndex`].

use std::sync::Arc;

use proxima_core::storage_ports::{VectorCandidate, VectorIndex};
use proxima_core::{EmbeddingDim, MemoryId, OwnerId, SpaceVector, StorageError};
use sqlx::{Postgres, Transaction};

use crate::error::map_err;
use crate::pgvector::{Lane, vector};
use crate::projection::Artifact;

/// Maximum chunk window used by adaptive ANN scans.
pub(crate) const SEMANTIC_SCAN_CAP: u32 = 1_000;

/// Where this store keeps its chunk vectors (#519): a closed choice every
/// vector path matches case by case.
#[derive(Clone)]
pub(crate) enum StoreVectors {
    /// pgvector in the store's own database, written and deleted in the
    /// transaction that changes their rows.
    Local,
    /// A host index; the store keeps the heads and pays index debt (see
    /// [`proxima_core::storage_ports::VectorIndex`]).
    Remote(Arc<dyn VectorIndex>),
}

impl std::fmt::Debug for StoreVectors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Local => f.write_str("Local"),
            Self::Remote(_) => f.write_str("Remote"),
        }
    }
}

/// Up to `window` memories nearest `query` from the local pgvector index, on
/// the caller's transaction.
pub(crate) async fn local_nearest(
    context: &mut Transaction<'static, Postgres>,
    owners: &[OwnerId],
    query: &SpaceVector,
    window: u32,
) -> Result<Vec<VectorCandidate>, StorageError> {
    if owners.is_empty() || window == 0 {
        return Ok(Vec::new());
    }
    let lane = Lane::of(query.space().dim());
    let owner_ids: Vec<_> = owners.iter().copied().map(OwnerId::into_inner).collect();
    // SQL-POLICY: fixed-fragment — the width comes from the closed Lane.
    let sql = format!(
        "SELECT memory_id, similarity_score
           FROM proxima_core.vector_candidates_d{}($1, $2, $3, $4)",
        lane.width
    );
    // SQL-POLICY: fixed-fragment — only the closed width is interpolated above.
    let rows: Vec<(uuid::Uuid, f32)> = sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str()))
        .bind(owner_ids)
        .bind(query.space().model_id())
        .bind(vector(query.values()))
        .bind(i64::from(window))
        .fetch_all(context.as_mut())
        .await
        .map_err(map_err)?;
    Ok(rows
        .into_iter()
        .map(|(memory_id, similarity_score)| VectorCandidate {
            memory_id: MemoryId::new(memory_id),
            similarity_score,
        })
        .collect())
}

/// Candidate functions for every supported width, with their inverses.
///
/// Provision `proxima_search` before installing these artifacts. Memory RLS remains enabled for that role. Runtime roles need
/// explicit execution grants; no candidate function is public.
/// # Errors
/// Propagates invalid generated SQL identifiers.
pub fn vector_candidate_artifacts() -> Result<Vec<Artifact>, StorageError> {
    EmbeddingDim::ALL
        .into_iter()
        .map(|dim| {
            let lane = Lane::of(dim);
            let name = format!("proxima_core.vector_candidates_d{}", lane.width);
            let signature = format!("{name}(uuid[], text, public.vector, bigint)");
            let body = nearest_sql(lane);
            // SQL-POLICY: fixed-fragment — names and body derive from closed width lanes.
            let forward = format!(
                "CREATE OR REPLACE FUNCTION {name}(uuid[], text, public.vector, bigint)
                 RETURNS TABLE(memory_id uuid, similarity_score real)
                 LANGUAGE plpgsql STABLE SECURITY DEFINER
                 SET search_path = pg_catalog, pg_temp
                 AS $body${body}$body$;"
            );
            // SQL-POLICY: fixed-fragment — the signature derives from a closed width lane.
            let inverse = format!("DROP FUNCTION IF EXISTS {signature};");
            let forward = crate::search_candidates::installation_sql(
                "proxima_core",
                &forward,
                &signature,
                &[
                    "proxima_core.embeddings",
                    "proxima_core.embedding_heads",
                    "proxima_core.memory",
                ],
                &["proxima_core.embeddings", "proxima_core.embedding_heads"],
            )?;
            Ok(Artifact { forward, inverse })
        })
        .collect()
}

/// A fixed-width ANN scan, with an exact owner scan when ANN underfills.
pub(crate) fn nearest_sql(lane: Lane) -> String {
    // SQL-POLICY: fixed-fragment — Lane contains only fixed width/index expressions.
    format!(
        "
DECLARE
    target bigint := LEAST(GREATEST($4, 0), {cap});
    chunk_window bigint := GREATEST(target, 1);
    candidate_ids uuid[] := '{{}}'::uuid[];
    scanned_ids uuid[];
    fetched bigint;
BEGIN
    PERFORM proxima_core.assert_search_scope($1);
    IF public.vector_dims($3) IS DISTINCT FROM {width} THEN
        RAISE EXCEPTION 'query vector does not match index width'
            USING ERRCODE = '22023';
    END IF;
    IF target = 0 THEN
        RETURN;
    END IF;
    LOOP
        WITH scanned AS MATERIALIZED (
{scan}
        )
        SELECT COALESCE(array_agg(DISTINCT s.entity_id), '{{}}'::uuid[]), count(*)
          INTO scanned_ids, fetched FROM scanned s;
        SELECT ARRAY(SELECT DISTINCT id FROM unnest(candidate_ids || scanned_ids) id)
          INTO candidate_ids;
        EXIT WHEN fetched < chunk_window
               OR cardinality(candidate_ids) >= target
               OR chunk_window >= {cap};
        chunk_window := LEAST(chunk_window * 2, {cap});
    END LOOP;
    IF cardinality(candidate_ids) < target THEN
        RETURN QUERY
{fallback}
        RETURN;
    END IF;
    RETURN QUERY
{rerank}
END;
",
        cap = SEMANTIC_SCAN_CAP,
        width = lane.width,
        scan = candidate_scan_sql(lane, "chunk_window"),
        fallback = best_chunk_scores_sql(lane, false),
        rerank = best_chunk_scores_sql(lane, true),
    )
}

fn best_chunk_scores_sql(lane: Lane, candidates_only: bool) -> String {
    let candidate_filter = if candidates_only {
        "
               AND emb.entity_id = ANY(candidate_ids)"
    } else {
        ""
    };
    let chunk_limit = if candidates_only {
        String::new()
    } else {
        format!(
            "\n         ORDER BY emb.chunk_score DESC NULLS LAST, emb.entity_id DESC\n         LIMIT {SEMANTIC_SCAN_CAP}"
        )
    };
    // SQL-POLICY: fixed-fragment — a closed width and optional fixed candidate filter.
    format!(
        "        WITH owned_embeddings AS MATERIALIZED (
            SELECT emb.entity_id, emb.model_id, emb.dim, emb.embedding_version,
                   NULLIF(1 - (emb.vec OPERATOR(public.<=>) $3), 'NaN'::float8) AS chunk_score
              FROM proxima_core.embeddings emb
             WHERE emb.owner_id = ANY($1)
               AND emb.model_id = $2
               AND {predicate}{candidate_filter}
        ), eligible_chunks AS (
        SELECT emb.entity_id, emb.chunk_score
          FROM owned_embeddings emb
          JOIN proxima_core.embedding_heads head
            ON head.entity_id = emb.entity_id
           AND head.model_id = emb.model_id
           AND head.dim = emb.dim
           AND head.embedding_version = emb.embedding_version
          JOIN proxima_core.memory m /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */ ON m.t = emb.entity_id
         WHERE m.owner_id = ANY($1)
           AND m.forgotten_at IS NULL /* FORGET-POLICY: live-only (semantic candidates under owner RLS) */{chunk_limit}
        )
        SELECT emb.entity_id,
               LEAST(1.0, GREATEST(0.0, MAX(emb.chunk_score)))::real AS score
          FROM eligible_chunks emb
         GROUP BY emb.entity_id
         ORDER BY score DESC, emb.entity_id DESC
         LIMIT target;",
        predicate = lane.predicate,
    )
}

fn candidate_scan_sql(lane: Lane, limit: &str) -> String {
    // SQL-POLICY: fixed-fragment — only closed lane fields and a fixed limit expression.
    format!(
        r"            SELECT emb.entity_id
              FROM proxima_core.embeddings emb
             WHERE {predicate}
               AND emb.owner_id = ANY($1)
               AND emb.model_id = $2
               AND EXISTS (
                   SELECT 1 FROM proxima_core.embedding_heads head
                    WHERE head.entity_id = emb.entity_id
                      AND head.model_id = emb.model_id
                      AND head.dim = emb.dim
                      AND head.embedding_version = emb.embedding_version
               )
               AND EXISTS (
                   SELECT 1 FROM proxima_core.memory m /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */
                    WHERE m.t = emb.entity_id
                      AND m.owner_id = ANY($1)
                      AND m.forgotten_at IS NULL /* FORGET-POLICY: live-only (semantic candidates under owner RLS) */
               )
             ORDER BY {vec} OPERATOR(public.<=>) $3{cast}
             LIMIT {limit}",
        predicate = lane.predicate,
        vec = lane.vec.replace("::", "::public."),
        cast = lane.cast.replace("::", "::public."),
    )
}

/// The production candidate scan with its local chunk budget as bind four.
#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
#[doc(hidden)]
#[must_use]
pub fn candidate_scan_sql_for_tests(dim: EmbeddingDim) -> String {
    candidate_scan_sql(Lane::of(dim), "$4")
}

mod debt;
pub(crate) use debt::{drain_debt, settle_owner_debt, sweep_index};

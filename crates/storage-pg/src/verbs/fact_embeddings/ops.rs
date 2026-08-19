use std::collections::HashSet;

use proxima_core::{
    EmbeddingAnnObservability, EmbeddingJobBacklog, EmbeddingOrphanCounts,
    EmbeddingOrphanSweepOutcome, EmbeddingRecallCanary, StorageError,
};
use sqlx::PgPool;

use crate::error::map_err;
/// Session settings for the ANN leg of the recall canary: the HNSW search
/// settings plus a seqscan ban, so the canary measures the index rather than
/// the planner's opinion of it. One statement, one round trip.
const ANN_CANARY_SESSION_SQL: &str = concat!(
    "SET LOCAL enable_seqscan = off; ",
    "SET LOCAL hnsw.ef_search = 100; ",
    "SET LOCAL hnsw.iterative_scan = relaxed_order"
);

use super::{nonnegative_count, ratio_count, usize_count};

#[derive(sqlx::FromRow)]
struct EmbeddingAnnObservabilityRow {
    embedding_rows: i64,
    embedding_head_rows: i64,
    embedding_job_rows: i64,
    embedding_table_bytes: i64,
    embedding_total_relation_bytes: i64,
    hnsw_index_bytes: i64,
    pending_jobs: i64,
    processing_jobs: i64,
    failed_jobs: i64,
    stale_processing_jobs: i64,
    orphan_embeddings: i64,
    orphan_heads: i64,
    orphan_jobs: i64,
}

#[derive(sqlx::FromRow)]
struct RecallSampleRow {
    owner_id: uuid::Uuid,
    model_id: String,
    vec: String,
}

#[derive(sqlx::FromRow)]
struct EmbeddingOrphanSweepRow {
    embeddings: i64,
    heads: i64,
    jobs: i64,
}

/// Owner-agnostic embedding ANN health signals for operator surfaces.
///
/// Authorization is intentionally outside storage; callers must gate this
/// read through `Engine::embedding_ann_observability`.
///
/// # Errors
///
/// Maps SQL failures through the shared mapper.
pub(crate) async fn embedding_ann_observability(
    pool: &PgPool,
    stale_claim_timeout_seconds: i64,
) -> Result<EmbeddingAnnObservability, StorageError> {
    let row = sqlx::query_as::<_, EmbeddingAnnObservabilityRow>(
        "WITH source_entities AS MATERIALIZED (
             SELECT 'goal'::text AS entity_kind,
                    t AS entity_id
               FROM proxima_core.goal
             UNION ALL
             SELECT kind::text AS entity_kind,
                    t AS entity_id
               FROM proxima_core.memory
         ),
         orphan_embeddings AS (
             SELECT count(*)::bigint AS count
               FROM proxima_core.embeddings emb
              WHERE NOT EXISTS (
                    SELECT 1
                      FROM source_entities src
                     WHERE src.entity_id = emb.entity_id
              )
         ),
         orphan_heads AS (
             SELECT count(*)::bigint AS count
               FROM proxima_core.embedding_heads head
              WHERE NOT EXISTS (
                    SELECT 1
                      FROM source_entities src
                     WHERE src.entity_id = head.entity_id
              )
         ),
         orphan_jobs AS (
             SELECT count(*)::bigint AS count
               FROM proxima_core.embedding_jobs job
              WHERE NOT EXISTS (
                    SELECT 1
                      FROM source_entities src
                     WHERE src.entity_id = job.entity_id
              )
         )
         SELECT
             (SELECT count(*)::bigint FROM proxima_core.embeddings)
                 AS embedding_rows,
             (SELECT count(*)::bigint FROM proxima_core.embedding_heads)
                 AS embedding_head_rows,
             (SELECT count(*)::bigint FROM proxima_core.embedding_jobs)
                 AS embedding_job_rows,
             pg_relation_size('proxima_core.embeddings'::regclass)::bigint
                 AS embedding_table_bytes,
             pg_total_relation_size('proxima_core.embeddings'::regclass)::bigint
                 AS embedding_total_relation_bytes,
             pg_relation_size('proxima_core.idx_embeddings_vec_hnsw'::regclass)::bigint
                 AS hnsw_index_bytes,
             (SELECT count(*)::bigint
                FROM proxima_core.embedding_jobs
               WHERE status = 'pending')
                 AS pending_jobs,
             (SELECT count(*)::bigint
                FROM proxima_core.embedding_jobs
               WHERE status = 'processing')
                 AS processing_jobs,
             (SELECT count(*)::bigint
                FROM proxima_core.embedding_jobs
               WHERE status IN ('failed', 'failed_permanent'))
                 AS failed_jobs,
             (SELECT count(*)::bigint
                FROM proxima_core.embedding_jobs
               WHERE status = 'processing'
                 AND (
                     claimed_at IS NULL
                     OR claimed_at < now()
                         - make_interval(secs => ($1::bigint)::double precision)
                 ))
                 AS stale_processing_jobs,
             (SELECT count FROM orphan_embeddings) AS orphan_embeddings,
             (SELECT count FROM orphan_heads) AS orphan_heads,
             (SELECT count FROM orphan_jobs) AS orphan_jobs",
    )
    .bind(stale_claim_timeout_seconds)
    .fetch_one(pool)
    .await
    .map_err(map_err)?;

    observability_from_row(&row, embedding_recall_canary(pool, 10).await?)
}

fn observability_from_row(
    row: &EmbeddingAnnObservabilityRow,
    recall_canary: Option<EmbeddingRecallCanary>,
) -> Result<EmbeddingAnnObservability, StorageError> {
    Ok(EmbeddingAnnObservability {
        embedding_rows: nonnegative_count(row.embedding_rows, "embedding rows")?,
        embedding_head_rows: nonnegative_count(row.embedding_head_rows, "embedding head rows")?,
        embedding_job_rows: nonnegative_count(row.embedding_job_rows, "embedding job rows")?,
        embedding_table_bytes: nonnegative_count(
            row.embedding_table_bytes,
            "embedding table bytes",
        )?,
        embedding_total_relation_bytes: nonnegative_count(
            row.embedding_total_relation_bytes,
            "embedding total relation bytes",
        )?,
        hnsw_index_bytes: nonnegative_count(row.hnsw_index_bytes, "hnsw index bytes")?,
        backlog: EmbeddingJobBacklog {
            pending: nonnegative_count(row.pending_jobs, "pending embedding jobs")?,
            processing: nonnegative_count(row.processing_jobs, "processing embedding jobs")?,
            failed: nonnegative_count(row.failed_jobs, "failed embedding jobs")?,
        },
        stale_processing_jobs: nonnegative_count(
            row.stale_processing_jobs,
            "stale processing embedding jobs",
        )?,
        orphan_rows: EmbeddingOrphanCounts {
            embeddings: nonnegative_count(row.orphan_embeddings, "orphan embeddings")?,
            heads: nonnegative_count(row.orphan_heads, "orphan embedding heads")?,
            jobs: nonnegative_count(row.orphan_jobs, "orphan embedding jobs")?,
        },
        recall_canary,
    })
}

async fn embedding_recall_canary(
    pool: &PgPool,
    k: i64,
) -> Result<Option<EmbeddingRecallCanary>, StorageError> {
    let Some(sample) = sqlx::query_as::<_, RecallSampleRow>(
        "SELECT emb.owner_id, emb.model_id, emb.vec::text AS vec
           FROM proxima_core.embeddings emb
           JOIN proxima_core.embedding_heads head
             ON head.entity_id = emb.entity_id
            AND head.model_id = emb.model_id
            AND head.embedding_version = emb.embedding_version
          ORDER BY emb.entity_id DESC
          LIMIT 1",
    )
    .fetch_optional(pool)
    .await
    .map_err(map_err)?
    else {
        return Ok(None);
    };

    let exact_ids = current_embedding_ids_by_distance(
        pool,
        sample.owner_id,
        &sample.model_id,
        &sample.vec,
        k,
        DistancePlan::Exact,
    )
    .await?;
    let ann_ids = current_embedding_ids_by_distance(
        pool,
        sample.owner_id,
        &sample.model_id,
        &sample.vec,
        k,
        DistancePlan::Ann,
    )
    .await?;
    let exact_set: HashSet<_> = exact_ids.iter().copied().collect();
    let overlap_count = ann_ids
        .iter()
        .filter(|entity_id| exact_set.contains(entity_id))
        .count();
    let exact_count = usize_count(exact_ids.len(), "exact recall")?;
    let ann_count = usize_count(ann_ids.len(), "ANN recall")?;
    let overlap_count = usize_count(overlap_count, "recall overlap")?;
    let recall_at_k = if exact_count == 0 {
        1.0
    } else {
        f64::from(ratio_count(overlap_count, "recall overlap")?)
            / f64::from(ratio_count(exact_count, "exact recall")?)
    };

    Ok(Some(EmbeddingRecallCanary {
        model_id: sample.model_id,
        k: nonnegative_count(k, "recall canary k")?,
        exact_count,
        ann_count,
        overlap_count,
        recall_at_k,
    }))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DistancePlan {
    Exact,
    Ann,
}

async fn current_embedding_ids_by_distance(
    pool: &PgPool,
    owner_id: uuid::Uuid,
    model_id: &str,
    vec: &str,
    k: i64,
    plan: DistancePlan,
) -> Result<Vec<uuid::Uuid>, StorageError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|err| StorageError::Internal(format!("begin recall canary tx: {err}")))?;
    match plan {
        DistancePlan::Exact => {
            sqlx::query("SET LOCAL enable_indexscan = off")
                .execute(tx.as_mut())
                .await
                .map_err(map_err)?;
            sqlx::query("SET LOCAL enable_indexonlyscan = off")
                .execute(tx.as_mut())
                .await
                .map_err(map_err)?;
            sqlx::query("SET LOCAL enable_bitmapscan = off")
                .execute(tx.as_mut())
                .await
                .map_err(map_err)?;
        }
        DistancePlan::Ann => {
            // SQL-POLICY: fixed-fragment
            sqlx::raw_sql(ANN_CANARY_SESSION_SQL)
                .execute(tx.as_mut())
                .await
                .map_err(map_err)?;
        }
    }
    // One vec per (entity_id, model_id, embedding_version). Head join
    // already picks the current version; do not DISTINCT ON a dropped
    // entity_kind column.
    let rows = sqlx::query_scalar::<_, uuid::Uuid>(
        "SELECT emb.entity_id
           FROM proxima_core.embeddings emb
           JOIN proxima_core.embedding_heads head
             ON head.entity_id = emb.entity_id
            AND head.model_id = emb.model_id
            AND head.embedding_version = emb.embedding_version
          WHERE emb.model_id = $1
            AND emb.owner_id = $2
          ORDER BY emb.vec <=> $3::vector, emb.entity_id
          LIMIT $4",
    )
    .bind(model_id)
    .bind(owner_id)
    .bind(vec)
    .bind(k)
    .fetch_all(tx.as_mut())
    .await
    .map_err(map_err)?;
    tx.commit().await.map_err(map_err)?;
    Ok(rows)
}

/// Delete embedding infrastructure rows whose source entity no longer exists.
///
/// Compliance erase performs synchronous cascade deletes and must not rely on
/// this crash-residue maintenance path for lawful wipe semantics.
///
/// # Errors
///
/// Maps SQL failures through the shared mapper.
pub(crate) async fn sweep_orphan_embedding_rows(
    pool: &PgPool,
) -> Result<EmbeddingOrphanSweepOutcome, StorageError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|err| StorageError::Internal(format!("begin embedding orphan sweep tx: {err}")))?;

    let row = sqlx::query_as::<_, EmbeddingOrphanSweepRow>(
        "WITH source_entities AS MATERIALIZED (
             SELECT 'goal'::text AS entity_kind,
                    t AS entity_id
               FROM proxima_core.goal
             UNION ALL
             SELECT kind::text AS entity_kind,
                    t AS entity_id
               FROM proxima_core.memory
         ),
         deleted_jobs AS (
             DELETE FROM proxima_core.embedding_jobs job
              WHERE NOT EXISTS (
                    SELECT 1
                      FROM source_entities src
                     WHERE src.entity_id = job.entity_id
              )
              RETURNING 1
         ),
         deleted_heads AS (
             DELETE FROM proxima_core.embedding_heads head
              WHERE NOT EXISTS (
                    SELECT 1
                      FROM source_entities src
                     WHERE src.entity_id = head.entity_id
              )
              RETURNING 1
         ),
         deleted_embeddings AS (
             DELETE FROM proxima_core.embeddings emb
              WHERE NOT EXISTS (
                    SELECT 1
                      FROM source_entities src
                     WHERE src.entity_id = emb.entity_id
              )
              RETURNING 1
         )
         SELECT
          (SELECT count(*)::bigint FROM deleted_embeddings) AS embeddings,
          (SELECT count(*)::bigint FROM deleted_heads) AS heads,
          (SELECT count(*)::bigint FROM deleted_jobs) AS jobs",
    )
    .fetch_one(tx.as_mut())
    .await
    .map_err(map_err)?;

    tx.commit().await.map_err(map_err)?;
    Ok(EmbeddingOrphanSweepOutcome {
        embeddings_deleted: nonnegative_count(row.embeddings, "deleted embeddings")?,
        heads_deleted: nonnegative_count(row.heads, "deleted embedding heads")?,
        jobs_deleted: nonnegative_count(row.jobs, "deleted embedding jobs")?,
    })
}

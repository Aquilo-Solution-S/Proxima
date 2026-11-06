use std::collections::{BTreeMap, HashSet};

use proxiima_core::{
    EmbeddingAnnObservability, EmbeddingJobBacklog, EmbeddingOrphanCounts,
    EmbeddingOrphanSweepOutcome, EmbeddingRecallCanary, EmbeddingRecallStratum, StorageError,
};
use sqlx::{Connection, PgConnection, PgPool};

use crate::error::map_err;

/// Bound the exact/ANN comparisons performed by one operator observation.
const RECALL_CANARY_SAMPLE_CAP: u16 = 32;
const RECALL_CANARY_NEIGHBORS: u16 = 10;
/// Session settings for the ANN leg of the recall canary: the HNSW search
/// settings plus a seqscan ban, so the canary measures the index rather than
/// the planner's opinion of it. One statement, one round trip.
const ANN_CANARY_SESSION_SQL: &str = concat!(
    "SET LOCAL enable_seqscan = off; ",
    "SET LOCAL hnsw.ef_search = 100; ",
    "SET LOCAL hnsw.iterative_scan = relaxed_order"
);

use super::usize_count;
use crate::sidecars::int_to_u64;

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
    dim: i16,
    vec: pgvector::Vector,
    available_strata: i64,
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
    platform: &crate::PgPlatformScope,
    stale_claim_timeout_seconds: i64,
    vectors: crate::VectorStorage,
) -> Result<EmbeddingAnnObservability, StorageError> {
    let mut tx = crate::platform_scope::begin_platform_transaction(pool, platform).await?;
    let result = embedding_ann_observability_on_connection(
        tx.as_mut(),
        stale_claim_timeout_seconds,
        vectors,
    )
    .await;
    crate::owner_scope::finish_transaction(tx, result).await
}

async fn embedding_ann_observability_on_connection(
    pool: &mut PgConnection,
    stale_claim_timeout_seconds: i64,
    vectors: crate::VectorStorage,
) -> Result<EmbeddingAnnObservability, StorageError> {
    let row = sqlx::query_as::<_, EmbeddingAnnObservabilityRow>(
        "WITH source_entities AS MATERIALIZED (
             SELECT 'goal'::text AS entity_kind,
                    t AS entity_id
               FROM proxiima_core.goal
             UNION ALL
             SELECT kind::text AS entity_kind,
                    t AS entity_id
               FROM proxiima_core.memory WHERE forgotten_at IS NULL /* FORGET-POLICY: live-only (embedding drain and reconcile act on live memories) */
         ),
         orphan_embeddings AS (
             SELECT count(*)::bigint AS count
               FROM proxiima_core.embeddings emb
              WHERE NOT EXISTS (
                    SELECT 1
                      FROM source_entities src
                     WHERE src.entity_id = emb.entity_id
              )
         ),
         orphan_heads AS (
             SELECT count(*)::bigint AS count
               FROM proxiima_core.embedding_heads head
              WHERE NOT EXISTS (
                    SELECT 1
                      FROM source_entities src
                     WHERE src.entity_id = head.entity_id
              )
         ),
         orphan_jobs AS (
             SELECT count(*)::bigint AS count
               FROM proxiima_core.embedding_jobs job
              WHERE NOT EXISTS (
                    SELECT 1
                      FROM source_entities src
                     WHERE src.entity_id = job.entity_id
              )
         )
         SELECT
             (SELECT count(*)::bigint FROM proxiima_core.embeddings)
                 AS embedding_rows,
             (SELECT count(*)::bigint FROM proxiima_core.embedding_heads)
                 AS embedding_head_rows,
             (SELECT count(*)::bigint FROM proxiima_core.embedding_jobs)
                 AS embedding_job_rows,
             pg_relation_size('proxiima_core.embeddings'::regclass)::bigint
                 AS embedding_table_bytes,
             pg_total_relation_size('proxiima_core.embeddings'::regclass)::bigint
                 AS embedding_total_relation_bytes,
             (SELECT COALESCE(sum(pg_relation_size(i.indexrelid)), 0)::bigint
                FROM pg_index i
                JOIN pg_class c ON c.oid = i.indexrelid
                JOIN pg_am am ON am.oid = c.relam
               WHERE i.indrelid = 'proxiima_core.embeddings'::regclass
                 AND am.amname = 'hnsw')
                 AS hnsw_index_bytes,
             (SELECT count(*)::bigint
                FROM proxiima_core.embedding_jobs
               WHERE status = 'pending')
                 AS pending_jobs,
             (SELECT count(*)::bigint
                FROM proxiima_core.embedding_jobs
               WHERE status = 'processing')
                 AS processing_jobs,
             (SELECT count(*)::bigint
                FROM proxiima_core.embedding_jobs
               WHERE status IN ('failed', 'failed_permanent'))
                 AS failed_jobs,
             (SELECT count(*)::bigint
                FROM proxiima_core.embedding_jobs
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
    .fetch_one(&mut *pool)
    .await
    .map_err(map_err)?;

    // A host-index store keeps no vectors here, so there is nothing to sample.
    let canary = match vectors {
        crate::VectorStorage::Local => embedding_recall_canary(&mut *pool).await?,
        crate::VectorStorage::HostIndex => None,
    };
    observability_from_row(&row, canary)
}

fn observability_from_row(
    row: &EmbeddingAnnObservabilityRow,
    recall_canary: Option<EmbeddingRecallCanary>,
) -> Result<EmbeddingAnnObservability, StorageError> {
    Ok(EmbeddingAnnObservability {
        embedding_rows: int_to_u64(row.embedding_rows, "embedding rows")?,
        embedding_head_rows: int_to_u64(row.embedding_head_rows, "embedding head rows")?,
        embedding_job_rows: int_to_u64(row.embedding_job_rows, "embedding job rows")?,
        embedding_table_bytes: int_to_u64(row.embedding_table_bytes, "embedding table bytes")?,
        embedding_total_relation_bytes: int_to_u64(
            row.embedding_total_relation_bytes,
            "embedding total relation bytes",
        )?,
        hnsw_index_bytes: int_to_u64(row.hnsw_index_bytes, "hnsw index bytes")?,
        backlog: EmbeddingJobBacklog {
            pending: int_to_u64(row.pending_jobs, "pending embedding jobs")?,
            processing: int_to_u64(row.processing_jobs, "processing embedding jobs")?,
            failed: int_to_u64(row.failed_jobs, "failed embedding jobs")?,
        },
        stale_processing_jobs: int_to_u64(
            row.stale_processing_jobs,
            "stale processing embedding jobs",
        )?,
        orphan_rows: EmbeddingOrphanCounts {
            embeddings: int_to_u64(row.orphan_embeddings, "orphan embeddings")?,
            heads: int_to_u64(row.orphan_heads, "orphan embedding heads")?,
            jobs: int_to_u64(row.orphan_jobs, "orphan embedding jobs")?,
        },
        recall_canary,
    })
}

async fn embedding_recall_canary(
    pool: &mut PgConnection,
) -> Result<Option<EmbeddingRecallCanary>, StorageError> {
    let samples = sqlx::query_as::<_, RecallSampleRow>(
        "WITH ranked AS (
             SELECT emb.owner_id, emb.model_id, emb.dim, emb.vec,
                    row_number() OVER (
                        PARTITION BY emb.owner_id, emb.model_id, emb.dim
                        ORDER BY emb.entity_id DESC, emb.chunk_ordinal
                    ) AS sample_rank
               FROM proxiima_core.embeddings emb
               JOIN proxiima_core.embedding_heads head
                 ON head.entity_id = emb.entity_id
                AND head.model_id = emb.model_id
                AND head.dim = emb.dim
                AND head.embedding_version = emb.embedding_version
          )
          SELECT owner_id, model_id, dim, vec,
                 count(*) OVER () AS available_strata
            FROM ranked
           WHERE sample_rank = 1
           ORDER BY owner_id, model_id, dim
           LIMIT $1",
    )
    .bind(i64::from(RECALL_CANARY_SAMPLE_CAP))
    .fetch_all(&mut *pool)
    .await
    .map_err(map_err)?;
    let Some(first) = samples.first() else {
        return Ok(None);
    };
    let available_strata = int_to_u64(first.available_strata, "recall canary strata")?;
    let mut measurements = Vec::with_capacity(samples.len());
    for sample in samples {
        measurements.push(measure_recall_sample(pool, sample).await?);
    }
    Ok(summarize_recalls(available_strata, measurements))
}

async fn measure_recall_sample(
    pool: &mut PgConnection,
    sample: RecallSampleRow,
) -> Result<EmbeddingRecallStratum, StorageError> {
    let lane = crate::pgvector::Lane::from_stored(sample.dim)?;
    let query = lane.query_vector(sample.vec.as_slice());
    let k = i64::from(RECALL_CANARY_NEIGHBORS);
    let exact_ids = current_embedding_ids_by_distance(
        pool,
        sample.owner_id,
        &sample.model_id,
        lane,
        &query,
        k,
        DistancePlan::Exact,
    )
    .await?;
    let ann_ids = current_embedding_ids_by_distance(
        pool,
        sample.owner_id,
        &sample.model_id,
        lane,
        &query,
        k,
        DistancePlan::Ann,
    )
    .await?;
    let exact_set: HashSet<_> = exact_ids.iter().copied().collect();
    let overlap = ann_ids.iter().filter(|id| exact_set.contains(id)).count();
    let exact_count = usize_count(exact_ids.len(), "exact recall")?;
    let ann_count = usize_count(ann_ids.len(), "ANN recall")?;
    let overlap_count = usize_count(overlap, "recall overlap")?;
    let recall_at_k = recall_ratio(overlap_count, exact_count);
    Ok(EmbeddingRecallStratum {
        model_id: sample.model_id,
        dim: crate::pgvector::stored_dim(sample.dim)?.width(),
        sample_count: 1,
        exact_count,
        ann_count,
        overlap_count,
        recall_at_k,
        minimum_recall_at_k: recall_at_k,
    })
}

// At most u16::MAX samples of u16::MAX neighbors: each total fits u32.
fn recall_ratio(overlap: u64, exact: u64) -> f64 {
    if exact == 0 {
        return 0.0;
    }
    let overlap = u32::try_from(overlap).expect("bounded recall overlap fits u32");
    let exact = u32::try_from(exact).expect("bounded recall population fits u32");
    f64::from(overlap) / f64::from(exact)
}

fn summarize_recalls(
    available_strata: u64,
    samples: Vec<EmbeddingRecallStratum>,
) -> Option<EmbeddingRecallCanary> {
    if samples.is_empty() {
        return None;
    }
    let mut strata_by_space = BTreeMap::<(String, usize), EmbeddingRecallStratum>::new();
    let mut summary = EmbeddingRecallCanary {
        available_strata,
        sample_count: 0,
        k: u64::from(RECALL_CANARY_NEIGHBORS),
        exact_count: 0,
        ann_count: 0,
        overlap_count: 0,
        recall_at_k: 0.0,
        minimum_recall_at_k: 1.0,
        strata: Vec::new(),
    };
    for sample in samples {
        summary.sample_count += sample.sample_count;
        summary.exact_count += sample.exact_count;
        summary.ann_count += sample.ann_count;
        summary.overlap_count += sample.overlap_count;
        summary.minimum_recall_at_k = summary.minimum_recall_at_k.min(sample.minimum_recall_at_k);
        let key = (sample.model_id.clone(), sample.dim);
        if let Some(stratum) = strata_by_space.get_mut(&key) {
            stratum.sample_count += sample.sample_count;
            stratum.exact_count += sample.exact_count;
            stratum.ann_count += sample.ann_count;
            stratum.overlap_count += sample.overlap_count;
            stratum.minimum_recall_at_k =
                stratum.minimum_recall_at_k.min(sample.minimum_recall_at_k);
        } else {
            strata_by_space.insert(key, sample);
        }
    }
    summary.recall_at_k = recall_ratio(summary.overlap_count, summary.exact_count);
    summary.strata = strata_by_space
        .into_values()
        .map(|mut stratum| {
            stratum.recall_at_k = recall_ratio(stratum.overlap_count, stratum.exact_count);
            stratum
        })
        .collect();
    Some(summary)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DistancePlan {
    Exact,
    Ann,
}

async fn current_embedding_ids_by_distance(
    pool: &mut PgConnection,
    owner_id: uuid::Uuid,
    model_id: &str,
    lane: crate::pgvector::Lane,
    vec: &crate::pgvector::QueryVector,
    k: i64,
    plan: DistancePlan,
) -> Result<Vec<(uuid::Uuid, i32, i32)>, StorageError> {
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
    // The canary measures ANN recall over vector rows. Distinguish chunks
    // so another chunk of the same memory cannot count as an exact match.
    let sql = format!(
        "SELECT emb.entity_id, emb.embedding_version, emb.chunk_ordinal
           FROM proxiima_core.embeddings emb
           JOIN proxiima_core.embedding_heads head
             ON head.entity_id = emb.entity_id
            AND head.model_id = emb.model_id
            AND head.dim = emb.dim
            AND head.embedding_version = emb.embedding_version
          WHERE emb.model_id = $1
            AND emb.owner_id = $2
            AND {predicate}
          ORDER BY {vec} <=> $3{cast}, emb.entity_id, emb.chunk_ordinal
          LIMIT $4",
        predicate = lane.predicate,
        vec = lane.vec,
        cast = lane.cast,
    );
    // SQL-POLICY: fixed-fragment — the lane's compile-time predicate and
    // casts, chosen by a closed enum; every value is bound.
    let rows = sqlx::query_as::<_, (uuid::Uuid, i32, i32)>(sqlx::AssertSqlSafe(sql))
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
/// Owner erase performs synchronous cascade deletes and must not rely on
/// this crash-residue maintenance path for lawful wipe semantics.
///
/// # Errors
///
/// Maps SQL failures through the shared mapper.
pub(crate) async fn sweep_orphan_embedding_rows(
    pool: &PgPool,
    platform: &crate::PgPlatformScope,
) -> Result<EmbeddingOrphanSweepOutcome, StorageError> {
    let mut tx = crate::platform_scope::begin_platform_transaction(pool, platform).await?;

    let row = sqlx::query_as::<_, EmbeddingOrphanSweepRow>(
        "WITH source_entities AS MATERIALIZED (
             SELECT 'goal'::text AS entity_kind,
                    t AS entity_id
               FROM proxiima_core.goal
             UNION ALL
             SELECT kind::text AS entity_kind,
                    t AS entity_id
               FROM proxiima_core.memory WHERE forgotten_at IS NULL /* FORGET-POLICY: live-only (embedding drain and reconcile act on live memories) */
         ),
         deleted_jobs AS (
             DELETE FROM proxiima_core.embedding_jobs job
              WHERE NOT EXISTS (
                    SELECT 1
                      FROM source_entities src
                     WHERE src.entity_id = job.entity_id
              )
              RETURNING 1
         ),
         deleted_heads AS (
             DELETE FROM proxiima_core.embedding_heads head
              WHERE NOT EXISTS (
                    SELECT 1
                      FROM source_entities src
                     WHERE src.entity_id = head.entity_id
              )
              RETURNING 1
         ),
         deleted_embeddings AS (
             DELETE FROM proxiima_core.embeddings emb
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
        embeddings_deleted: int_to_u64(row.embeddings, "deleted embeddings")?,
        heads_deleted: int_to_u64(row.heads, "deleted embedding heads")?,
        jobs_deleted: int_to_u64(row.jobs, "deleted embedding jobs")?,
    })
}

#[cfg(test)]
mod recall_tests {
    use super::{EmbeddingRecallStratum, summarize_recalls};

    fn sample(model: &str, exact: u64, overlap: u64, recall: f64) -> EmbeddingRecallStratum {
        EmbeddingRecallStratum {
            model_id: model.into(),
            dim: 384,
            sample_count: 1,
            exact_count: exact,
            ann_count: overlap,
            overlap_count: overlap,
            recall_at_k: recall,
            minimum_recall_at_k: recall,
        }
    }

    #[test]
    fn degraded_owner_remains_visible_inside_a_space_and_across_spaces() {
        let summary = summarize_recalls(
            8,
            vec![
                sample("a", 10, 10, 1.0),
                sample("a", 10, 2, 0.2),
                sample("b", 10, 10, 1.0),
            ],
        )
        .expect("nonempty samples");
        assert_eq!((summary.sample_count, summary.available_strata), (3, 8));
        assert_eq!((summary.exact_count, summary.overlap_count), (30, 22));
        assert!((summary.recall_at_k - 22.0 / 30.0).abs() < f64::EPSILON);
        assert!((summary.minimum_recall_at_k - 0.2).abs() < f64::EPSILON);
        assert_eq!(summary.strata[0].sample_count, 2);
        assert!((summary.strata[0].recall_at_k - 0.6).abs() < f64::EPSILON);
        assert!((summary.strata[0].minimum_recall_at_k - 0.2).abs() < f64::EPSILON);
        assert_eq!(summary.strata[1].sample_count, 1);
    }

    #[test]
    fn missing_samples_are_absent_and_an_empty_comparison_is_not_perfect() {
        assert!(summarize_recalls(0, vec![]).is_none());
        let summary = summarize_recalls(1, vec![sample("empty", 0, 0, 0.0)]).unwrap();
        assert!(summary.recall_at_k.abs() < f64::EPSILON);
        assert!(summary.minimum_recall_at_k.abs() < f64::EPSILON);
    }
}

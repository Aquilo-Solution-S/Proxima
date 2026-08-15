//! Job claim discipline: single-claim, stale reclaim, and the maintenance lock.

use super::{engine_for, fact_draft, load_embedding_job};

use proxima_core::storage_ports::*;
use std::sync::Arc;

use proxima_core::test_fixtures::ConstantEmbedding;
use proxima_core::{AuthPath, AuthzContext};

use crate::common::{drop_db, fresh_pg, owner_fixture};

#[tokio::test]
async fn claimed_embedding_job_is_not_claimed_again() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner_fixture();
        let engine = engine_for(
            pg.clone(),
            Some(Arc::new(ConstantEmbedding::prefixed(
                "stub-fact-embed",
                &[0.25, 0.5, 0.75],
            ))),
        );
        let outcome = engine
            .fact_ingest(
                &AuthzContext::single_owner(&owner, AuthPath::HostBearer),
                fact_draft(&owner, "skip locked fact"),
            )
            .await?;

        let claims = pg
            .claim_pending_embedding_jobs("stub-fact-embed", 1)
            .await?;
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0].entity_id, outcome.memory_id);
        assert_eq!(claims[0].attempts, 0);
        let second_claims = pg
            .claim_pending_embedding_jobs("stub-fact-embed", 1)
            .await?;
        assert!(second_claims.is_empty());
        assert_eq!(
            load_embedding_job(pg.pool_for_tests(), outcome.memory_id)
                .await?
                .map(|(status, _, _)| status),
            Some("processing".to_string()),
        );
        Ok(())
    }
    .await;
    drop(pg);
    drop_db(&db_name).await?;
    result
}

#[tokio::test]
async fn stale_processing_embedding_job_is_reclaimed() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner_fixture();
        let engine = engine_for(
            pg.clone(),
            Some(Arc::new(ConstantEmbedding::prefixed(
                "stub-fact-embed",
                &[0.25, 0.5, 0.75],
            ))),
        );
        let outcome = engine
            .fact_ingest(
                &AuthzContext::single_owner(&owner, AuthPath::HostBearer),
                fact_draft(&owner, "stale processing fact"),
            )
            .await?;

        let claims = pg
            .claim_pending_embedding_jobs("stub-fact-embed", 1)
            .await?;
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0].entity_id, outcome.memory_id);
        let second_claims = pg
            .claim_pending_embedding_jobs("stub-fact-embed", 1)
            .await?;
        assert!(second_claims.is_empty());

        sqlx::query(
            "UPDATE proxima_core.embedding_jobs
                SET updated_at = now() - interval '20 minutes'
              WHERE entity_id = $1",
        )
        .bind(outcome.memory_id.into_inner())
        .execute(pg.pool_for_tests())
        .await?;

        let reclaimed = pg
            .claim_pending_embedding_jobs("stub-fact-embed", 1)
            .await?;
        assert_eq!(reclaimed.len(), 1);
        assert_eq!(reclaimed[0].entity_id, outcome.memory_id);
        Ok(())
    }
    .await;
    drop(pg);
    drop_db(&db_name).await?;
    result
}

/// The arm-split claim must select exactly the jobs the default claim
/// selects — oldest claimable first across both status arms — while riding
/// the two arm-matched partial indexes instead of sorting the whole
/// backlog. The crowd of another model's jobs makes the plan assertion
/// run under DEFAULT planner costing (a one-row fixture with seqscan
/// disabled proves capability, not the plan the corpus gets).
#[allow(clippy::too_many_lines)]
#[tokio::test]
async fn arm_split_claim_selects_default_order_via_the_claim_indexes()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner_fixture();
        let engine = engine_for(
            pg.clone(),
            Some(Arc::new(ConstantEmbedding::prefixed(
                "stub-fact-embed",
                &[0.25, 0.5, 0.75],
            ))),
        );
        // Three claimable jobs with a forced enqueue order, oldest first.
        let mut ids = Vec::new();
        for (idx, text) in ["arm split a", "arm split b", "arm split c"].iter().enumerate() {
            let outcome = engine
                .fact_ingest(
                    &AuthzContext::single_owner(&owner, AuthPath::HostBearer),
                    fact_draft(&owner, text),
                )
                .await?;
            sqlx::query(
                "UPDATE proxima_core.embedding_jobs
                    SET enqueued_at = now() - interval '1 hour'
                                      + make_interval(mins => $2)
                  WHERE entity_id = $1",
            )
            .bind(outcome.memory_id.into_inner())
            .bind(i32::try_from(idx)?)
            .execute(pg.pool_for_tests())
            .await?;
            ids.push(outcome.memory_id);
        }
        // The middle job becomes a stale `processing` orphan: claimable
        // only through the reclaim arm, older than the newest pending job.
        sqlx::query(
            "UPDATE proxima_core.embedding_jobs
                SET status = 'processing', updated_at = now() - interval '20 minutes'
              WHERE entity_id = $1",
        )
        .bind(ids[1].into_inner())
        .execute(pg.pool_for_tests())
        .await?;
        // Crowd rows under a different model give default costing a reason
        // to reject a seq scan of the queue.
        sqlx::query(
            "INSERT INTO proxima_core.embedding_jobs
                (owner_kind, owner_id, entity_kind, entity_id, model_id)
             SELECT 'personal', $1, 'Fact', gen_random_uuid(), 'crowd-model'
               FROM generate_series(1, 20000)",
        )
        .bind(uuid::Uuid::now_v7())
        .execute(pg.pool_for_tests())
        .await?;
        sqlx::query("ANALYZE proxima_core.embedding_jobs")
            .execute(pg.pool_for_tests())
            .await?;

        let claim_sql =
            proxima_storage_pg::verbs::fact_embeddings::claim_embedding_jobs_sql_for_tests();
        let explain_sql = format!("EXPLAIN (FORMAT JSON, COSTS OFF) {claim_sql}");
        // SQL-POLICY: fixed-fragment — EXPLAIN prefix over the audited
        // claim constant; only bound values vary.
        let plan: serde_json::Value = sqlx::query_scalar(sqlx::AssertSqlSafe(explain_sql.as_str()))
            .bind("stub-fact-embed")
            .bind(Vec::<uuid::Uuid>::new())
            .bind(2_i64)
            .fetch_one(pg.pool_for_tests())
            .await?;
        let rendered = plan.to_string();
        for index in [
            "idx_embedding_jobs_pending_claim",
            "idx_embedding_jobs_processing_reclaim",
        ] {
            assert!(
                rendered.contains(index),
                "arm-split claim must ride {index} under default costing; plan:\n{rendered}"
            );
        }

        let claims = proxima_storage_pg::verbs::fact_embeddings::claim_pending_embedding_jobs(
            pg.pool_for_tests(),
            "stub-fact-embed",
            2,
        )
        .await?;
        let claimed: Vec<_> = claims.iter().map(|claim| claim.entity_id).collect();
        assert_eq!(claims.len(), 2);
        assert!(
            claimed.contains(&ids[0]) && claimed.contains(&ids[1]),
            "the merged top-2 must be the two oldest claimable jobs across both arms, got {claimed:?}"
        );
        let rest = proxima_storage_pg::verbs::fact_embeddings::claim_pending_embedding_jobs(
            pg.pool_for_tests(),
            "stub-fact-embed",
            2,
        )
        .await?;
        assert_eq!(rest.len(), 1, "only the youngest pending job remains");
        assert_eq!(rest[0].entity_id, ids[2]);
        Ok(())
    }
    .await;
    drop(pg);
    drop_db(&db_name).await?;
    result
}

#[tokio::test]
async fn embedding_maintenance_lock_excludes_concurrent_passes()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let held = pg
            .try_embedding_maintenance_lock()
            .await?
            .expect("first pass acquires the lock");

        // A concurrent pass (same process or another one — the lock is a
        // server-side advisory lock) must skip, not queue behind the holder.
        assert!(
            pg.try_embedding_maintenance_lock().await?.is_none(),
            "second pass must observe the held lock and skip"
        );

        // Dropping the guard closes its detached connection; the server
        // releases the session lock and the next pass may run. The release
        // is asynchronous from this process's perspective, so poll briefly.
        drop(held);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(reacquired) = pg.try_embedding_maintenance_lock().await? {
                drop(reacquired);
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "lock was not released after guard drop"
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}

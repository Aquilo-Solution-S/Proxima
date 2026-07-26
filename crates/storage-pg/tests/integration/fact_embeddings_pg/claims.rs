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

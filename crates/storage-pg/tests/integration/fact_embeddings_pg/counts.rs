//! Pending and failed job count surfaces.

use super::{
    PoisonBatchTransientItemEmbedding, clear_embedding_backoff, engine_for, fact_draft,
    load_embedding_job,
};

use proxima_core::storage_ports::*;
use std::sync::Arc;

use proxima_core::llm::EMBEDDING_JOB_MAX_ATTEMPTS;
use proxima_core::test_fixtures::ConstantEmbedding;
use proxima_core::{AuthPath, AuthzContext, OwnerRef, UserId};
use uuid::Uuid;

use crate::common::{drop_db, fresh_pg, owner_fixture};

#[tokio::test]
async fn count_pending_embedding_jobs_counts_outstanding() -> Result<(), Box<dyn std::error::Error>>
{
    let (pg, db_name) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner_fixture();
        let other_owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let engine = engine_for(
            pg.clone(),
            Some(Arc::new(ConstantEmbedding::prefixed(
                "stub-fact-embed",
                &[0.25, 0.5, 0.75],
            ))),
        );
        for label in ["pending count one", "pending count two"] {
            engine
                .fact_ingest(
                    &AuthzContext::single_owner(&owner, AuthPath::HostBearer),
                    fact_draft(&owner, label),
                )
                .await?;
        }
        engine
            .fact_ingest(
                &AuthzContext::single_owner(&other_owner, AuthPath::HostBearer),
                fact_draft(&other_owner, "other owner pending count"),
            )
            .await?;

        assert_eq!(pg.count_pending_embedding_jobs(&owner).await?, 2);
        assert_eq!(pg.count_pending_embedding_jobs(&other_owner).await?, 1);

        let drain = engine.drain_embedding_jobs(10).await?;
        assert_eq!(drain.processed, 3);
        assert_eq!(drain.failed, 0);
        assert_eq!(pg.count_pending_embedding_jobs(&owner).await?, 0);
        assert_eq!(pg.count_pending_embedding_jobs(&other_owner).await?, 0);
        Ok(())
    }
    .await;
    drop(pg);
    drop_db(&db_name).await?;
    result
}

/// Merged `count_embedding_job_status` agrees with independent pending
/// and failed counts and stays owner-scoped.
#[tokio::test]
async fn count_embedding_job_status_merges_pending_and_failed_counts()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner_fixture();
        let other_owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        // Per-item failures (batch rejected as permanent, items transient)
        // are what accrue attempts and reach `failed` under the batched
        // drain; purely transient failures release the claim instead.
        let engine = engine_for(
            pg.clone(),
            Some(Arc::new(PoisonBatchTransientItemEmbedding)),
        );

        // Drive one fact to the terminal `failed` state for `owner`.
        let failing = engine
            .fact_ingest(
                &AuthzContext::single_owner(&owner, AuthPath::HostBearer),
                fact_draft(&owner, "status merge failing"),
            )
            .await?;
        for _ in 1..=EMBEDDING_JOB_MAX_ATTEMPTS {
            engine.drain_embedding_jobs(10).await?;
            clear_embedding_backoff(pg.pool_for_tests(), failing.memory_id).await?;
        }
        let (status, ..) = load_embedding_job(pg.pool_for_tests(), failing.memory_id)
            .await?
            .expect("failed job must remain in embedding_jobs");
        assert_eq!(status, "failed");

        // A second fact for the same owner stays pending (not yet drained).
        engine
            .fact_ingest(
                &AuthzContext::single_owner(&owner, AuthPath::HostBearer),
                fact_draft(&owner, "status merge pending"),
            )
            .await?;

        // A third fact for a different owner must not leak into either count.
        engine
            .fact_ingest(
                &AuthzContext::single_owner(&other_owner, AuthPath::HostBearer),
                fact_draft(&other_owner, "status merge other owner"),
            )
            .await?;

        let merged = pg.count_embedding_job_status(&owner).await?;
        assert_eq!(
            merged.pending,
            pg.count_pending_embedding_jobs(&owner).await?
        );
        assert_eq!(merged.failed, pg.count_failed_embedding_jobs(&owner).await?);
        assert_eq!(merged.pending, 1);
        assert_eq!(merged.failed, 1);

        let other_merged = pg.count_embedding_job_status(&other_owner).await?;
        assert_eq!(other_merged.pending, 1);
        assert_eq!(other_merged.failed, 0);
        Ok(())
    }
    .await;
    drop(pg);
    drop_db(&db_name).await?;
    result
}

//! Draining pending embedding jobs into stored vectors.

use super::{
    CountingBatchEmbedding, count_embedding_jobs, count_fact_embeddings, engine_for, fact_draft,
    load_embedding_head_version,
};

use std::sync::Arc;

use proxima_core::test_fixtures::ConstantEmbedding;
use proxima_core::{AuthPath, AuthzContext, EntityKind};

use crate::common::{drop_db, fresh_pg, owner_fixture};

#[tokio::test]
async fn drain_embedding_jobs_writes_embedding_and_deletes_job()
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
        let outcome = engine
            .fact_ingest(
                &AuthzContext::single_owner(&owner, AuthPath::HostBearer),
                fact_draft(&owner, "drained fact"),
            )
            .await?;

        assert_eq!(
            count_fact_embeddings(pg.pool_for_tests(), outcome.memory_id).await?,
            0
        );
        assert_eq!(
            count_embedding_jobs(pg.pool_for_tests(), outcome.memory_id).await?,
            1
        );

        let drain = engine.drain_embedding_jobs(10).await?;
        assert_eq!(drain.processed, 1);
        assert_eq!(drain.failed, 0);
        assert_eq!(
            count_fact_embeddings(pg.pool_for_tests(), outcome.memory_id).await?,
            1
        );
        assert_eq!(
            load_embedding_head_version(
                pg.pool_for_tests(),
                EntityKind::Fact,
                outcome.memory_id.into_inner(),
                "stub-fact-embed",
            )
            .await?,
            Some(1)
        );
        assert_eq!(
            count_embedding_jobs(pg.pool_for_tests(), outcome.memory_id).await?,
            0
        );
        Ok(())
    }
    .await;
    drop(pg);
    drop_db(&db_name).await?;
    result
}

#[tokio::test]
async fn drain_embeds_full_batch_in_one_provider_call() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner_fixture();
        let counting = Arc::new(CountingBatchEmbedding::default());
        let engine = engine_for(pg.clone(), Some(counting.clone()));
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        let mut memory_ids = Vec::new();
        for label in ["batched fact one", "batched fact two", "batched fact three"] {
            let outcome = engine
                .fact_ingest(&authz, fact_draft(&owner, label))
                .await?;
            memory_ids.push(outcome.memory_id);
        }

        let drain = engine.drain_embedding_jobs(10).await?;
        assert_eq!(drain.processed, 3);
        assert_eq!(drain.failed, 0);
        for memory_id in memory_ids {
            assert_eq!(
                count_fact_embeddings(pg.pool_for_tests(), memory_id).await?,
                1
            );
        }
        // The point of batching: three memories, ONE provider request.
        assert_eq!(
            *counting.batch_calls.lock().expect("counter mutex"),
            vec![3],
            "all queued texts must travel in a single embed_many call"
        );
        assert_eq!(*counting.single_calls.lock().expect("counter mutex"), 0);
        Ok(())
    }
    .await;
    drop(pg);
    drop_db(&db_name).await?;
    result
}

//! Reconciliation passes: requeueing, backfill, and idempotent enqueueing.

use super::{
    PoisonBatchTransientItemEmbedding, clear_embedding_backoff, count_embedding_jobs,
    count_embedding_jobs_for_model, count_fact_embeddings, engine_for, fact_draft,
    load_embedding_job, load_memory_text, reconcile_stub_fact_embeddings,
    seed_embedding_row_with_head,
};

use proxima_core::storage_ports::*;
use std::sync::Arc;

use proxima_core::llm::{EMBEDDING_DIM, EMBEDDING_JOB_MAX_ATTEMPTS};
use proxima_core::test_fixtures::ConstantEmbedding;
use proxima_core::{AuthPath, AuthzContext, EntityKind};
use proxima_storage_pg::{EmbeddingReconcileOptions, EmbeddingReconcileScope};

use crate::common::{drop_db, fresh_pg, owner_fixture};

#[tokio::test]
async fn reconcile_requeues_failed_embedding_jobs() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner_fixture();
        let engine = engine_for(
            pg.clone(),
            Some(Arc::new(PoisonBatchTransientItemEmbedding)),
        );
        let outcome = engine
            .fact_ingest(
                &AuthzContext::single_owner(&owner, AuthPath::HostBearer),
                fact_draft(&owner, "reconcile fact"),
            )
            .await?;

        // Drive the job to the terminal `failed` state (backoff cleared between
        // attempts so we do not wait real time).
        for _ in 1..=EMBEDDING_JOB_MAX_ATTEMPTS {
            engine.drain_embedding_jobs(10).await?;
            clear_embedding_backoff(pg.pool_for_tests(), outcome.memory_id).await?;
        }
        let (status, attempts, _) = load_embedding_job(pg.pool_for_tests(), outcome.memory_id)
            .await?
            .expect("failed job must remain in embedding_jobs");
        assert_eq!(status, "failed");
        assert_eq!(attempts, EMBEDDING_JOB_MAX_ATTEMPTS);

        // The terminal failure is visible on the readiness count.
        assert_eq!(pg.count_failed_embedding_jobs(&owner).await?, 1);
        assert_eq!(pg.count_pending_embedding_jobs(&owner).await?, 0);

        // Reconcile lifts the Fact out of the dead-end: status back to pending,
        // attempts reset, last_error cleared — so a fresh provider/model or a
        // process restart can retry it.
        let reconciled = pg
            .reconcile_embeddings(EmbeddingReconcileOptions {
                model_id: "stub-fact-embed",
                scope: EmbeddingReconcileScope::MissingOnly,
                limit: None,
            })
            .await?;
        assert_eq!(reconciled.enqueued, 1, "reconcile requeues the failed job");
        let (status, attempts, last_error) =
            load_embedding_job(pg.pool_for_tests(), outcome.memory_id)
                .await?
                .expect("job still present after requeue");
        assert_eq!(status, "pending");
        assert_eq!(attempts, 0);
        assert!(last_error.is_none(), "requeue clears last_error");
        Ok(())
    }
    .await;
    drop(pg);
    drop_db(&db_name).await?;
    result
}

#[tokio::test]
async fn fact_embedding_backfill_heals_no_client_ingest() -> Result<(), Box<dyn std::error::Error>>
{
    let (pg, db_name) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner_fixture();
        let engine = engine_for(pg.clone(), None);
        let first = engine
            .fact_ingest(
                &AuthzContext::single_owner(&owner, AuthPath::HostBearer),
                fact_draft(&owner, "backfill fact one"),
            )
            .await?;
        let second = engine
            .fact_ingest(
                &AuthzContext::single_owner(&owner, AuthPath::HostBearer),
                fact_draft(&owner, "backfill fact two"),
            )
            .await?;

        assert_eq!(
            load_memory_text(pg.pool_for_tests(), first.memory_id).await?,
            Some("backfill fact one".to_string()),
        );
        assert_eq!(
            count_embedding_jobs(pg.pool_for_tests(), first.memory_id).await?,
            0
        );
        assert_eq!(
            count_embedding_jobs(pg.pool_for_tests(), second.memory_id).await?,
            0
        );

        engine
            .set_embed_client(Some(Arc::new(ConstantEmbedding::prefixed(
                "stub-fact-embed",
                &[0.25, 0.5, 0.75],
            ))))
            .await;
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        assert_eq!(engine.backfill_fact_embeddings(&authz, &owner, 1).await?, 1);
        assert_eq!(
            count_embedding_jobs(pg.pool_for_tests(), first.memory_id).await?
                + count_embedding_jobs(pg.pool_for_tests(), second.memory_id).await?,
            1
        );
        assert_eq!(
            count_fact_embeddings(pg.pool_for_tests(), first.memory_id).await?
                + count_fact_embeddings(pg.pool_for_tests(), second.memory_id).await?,
            0
        );

        let drain = engine.drain_embedding_jobs(10).await?;
        assert_eq!(drain.processed, 1);
        assert_eq!(drain.failed, 0);
        assert_eq!(
            count_fact_embeddings(pg.pool_for_tests(), first.memory_id).await?
                + count_fact_embeddings(pg.pool_for_tests(), second.memory_id).await?,
            1
        );

        assert_eq!(
            engine.backfill_fact_embeddings(&authz, &owner, 10).await?,
            1
        );
        let drain = engine.drain_embedding_jobs(10).await?;
        assert_eq!(drain.processed, 1);
        assert_eq!(drain.failed, 0);
        assert_eq!(
            count_fact_embeddings(pg.pool_for_tests(), first.memory_id).await?
                + count_fact_embeddings(pg.pool_for_tests(), second.memory_id).await?,
            2
        );
        assert_eq!(
            count_embedding_jobs(pg.pool_for_tests(), first.memory_id).await?,
            0
        );
        assert_eq!(
            count_embedding_jobs(pg.pool_for_tests(), second.memory_id).await?,
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
async fn reconcile_embeddings_enqueues_missing_facts_idempotently()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner_fixture();
        let engine = engine_for(pg.clone(), None);
        for label in [
            "reconcile fact one",
            "reconcile fact two",
            "reconcile fact three",
        ] {
            engine
                .fact_ingest(
                    &AuthzContext::single_owner(&owner, AuthPath::HostBearer),
                    fact_draft(&owner, label),
                )
                .await?;
        }
        let stale = engine
            .fact_ingest(
                &AuthzContext::single_owner(&owner, AuthPath::HostBearer),
                fact_draft(&owner, "reconcile stale fact"),
            )
            .await?;
        let other_model_embedding = vec![0.125; EMBEDDING_DIM];
        seed_embedding_row_with_head(
            pg.pool_for_tests(),
            &owner,
            EntityKind::Fact,
            stale.memory_id,
            "other-model",
            &other_model_embedding,
        )
        .await?;

        assert_eq!(
            count_embedding_jobs_for_model(pg.pool_for_tests(), "stub-fact-embed").await?,
            0
        );
        let first =
            reconcile_stub_fact_embeddings(&pg, EmbeddingReconcileScope::MissingOnly).await?;
        assert_eq!(first.scanned, 4);
        assert_eq!(first.enqueued, 4);
        assert_eq!(first.skipped, 0);
        assert_eq!(
            count_embedding_jobs_for_model(pg.pool_for_tests(), "stub-fact-embed").await?,
            4
        );
        assert_eq!(
            count_embedding_jobs(pg.pool_for_tests(), stale.memory_id).await?,
            1
        );

        let second =
            reconcile_stub_fact_embeddings(&pg, EmbeddingReconcileScope::MissingOnly).await?;
        assert_eq!(second.scanned, 0);
        assert_eq!(second.enqueued, 0);
        assert_eq!(second.skipped, 0);
        assert_eq!(
            count_embedding_jobs_for_model(pg.pool_for_tests(), "stub-fact-embed").await?,
            4
        );
        assert_eq!(
            count_embedding_jobs(pg.pool_for_tests(), stale.memory_id).await?,
            1
        );

        let include_stale =
            reconcile_stub_fact_embeddings(&pg, EmbeddingReconcileScope::IncludeStale).await?;
        assert_eq!(include_stale.scanned, 0);
        assert_eq!(include_stale.enqueued, 0);
        assert_eq!(include_stale.skipped, 0);
        assert_eq!(
            count_embedding_jobs_for_model(pg.pool_for_tests(), "stub-fact-embed").await?,
            4
        );
        assert_eq!(
            count_embedding_jobs(pg.pool_for_tests(), stale.memory_id).await?,
            1
        );

        let include_stale_again =
            reconcile_stub_fact_embeddings(&pg, EmbeddingReconcileScope::IncludeStale).await?;
        assert_eq!(include_stale_again.scanned, 0);
        assert_eq!(include_stale_again.enqueued, 0);
        assert_eq!(include_stale_again.skipped, 0);
        assert_eq!(
            count_embedding_jobs_for_model(pg.pool_for_tests(), "stub-fact-embed").await?,
            4
        );
        Ok(())
    }
    .await;
    drop(pg);
    drop_db(&db_name).await?;
    result
}

#[tokio::test]
async fn reconcile_embedding_drain_writes_fact_embeddings() -> Result<(), Box<dyn std::error::Error>>
{
    let (pg, db_name) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner_fixture();
        let engine = engine_for(pg.clone(), None);
        let outcome = engine
            .fact_ingest(
                &AuthzContext::single_owner(&owner, AuthPath::HostBearer),
                fact_draft(&owner, "reconcile drain fact"),
            )
            .await?;
        reconcile_stub_fact_embeddings(&pg, EmbeddingReconcileScope::MissingOnly).await?;

        let client = ConstantEmbedding::prefixed("stub-fact-embed", &[0.25, 0.5, 0.75]);
        let drain = pg.drain_embedding_jobs_inline(&client, 10).await?;
        assert_eq!(drain.embedded, 1);
        assert_eq!(drain.failed, 0);
        assert_eq!(
            count_fact_embeddings(pg.pool_for_tests(), outcome.memory_id).await?,
            1
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
async fn reconcile_limit_skips_existing_heads_before_bounding()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner_fixture();
        let engine = engine_for(pg.clone(), None);
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        let covered = engine
            .fact_ingest(&authz, fact_draft(&owner, "already covered"))
            .await?;
        let missing = engine
            .fact_ingest(&authz, fact_draft(&owner, "missing after covered"))
            .await?;
        seed_embedding_row_with_head(
            pg.pool_for_tests(),
            &owner,
            EntityKind::Fact,
            covered.memory_id,
            "stub-fact-embed",
            &vec![0.25; EMBEDDING_DIM],
        )
        .await?;

        let outcome = pg
            .reconcile_embeddings(EmbeddingReconcileOptions {
                model_id: "stub-fact-embed",
                scope: EmbeddingReconcileScope::MissingOnly,
                limit: Some(1),
            })
            .await?;
        assert_eq!(outcome.scanned, 1);
        assert_eq!(outcome.enqueued, 1);
        assert_eq!(
            count_embedding_jobs(pg.pool_for_tests(), missing.memory_id).await?,
            1
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}

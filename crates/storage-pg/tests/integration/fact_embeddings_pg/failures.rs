//! Provider failure handling: retry caps, transient releases, terminal rejects, and over-limit rescue.

use super::{
    CrashOnInputEmbedding, FailingEmbedding, PoisonBatchTransientItemEmbedding,
    PoisonTextEmbedding, TokenCapEmbedding, clear_embedding_backoff, count_fact_embeddings,
    engine_for, fact_draft, load_embedding_job,
};

use std::sync::Arc;

use proxima_core::llm::EMBEDDING_JOB_MAX_ATTEMPTS;
use proxima_core::{AuthPath, AuthzContext};
use proxima_storage_pg::{EmbeddingReconcileOptions, EmbeddingReconcileScope};

use crate::common::{drop_db, fresh_pg, owner_fixture};

#[tokio::test]
async fn failed_embedding_jobs_retry_until_attempt_cap() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner_fixture();
        // Attempts accrue on the per-item isolation path (batch rejected as
        // permanent, items failing transiently). Purely transient batch
        // failures release claims instead — covered by
        // `transient_failure_releases_claim_without_burning_attempts`.
        let engine = engine_for(
            pg.clone(),
            Some(Arc::new(PoisonBatchTransientItemEmbedding)),
        );
        let outcome = engine
            .fact_ingest(
                &AuthzContext::single_owner(&owner, AuthPath::HostBearer),
                fact_draft(&owner, "failing fact"),
            )
            .await?;

        for attempt in 1..=EMBEDDING_JOB_MAX_ATTEMPTS {
            let drain = engine.drain_embedding_jobs(10).await?;
            assert_eq!(drain.processed, 1);
            assert_eq!(drain.failed, 1);
            let Some((status, attempts, last_error)) =
                load_embedding_job(pg.pool_for_tests(), outcome.memory_id).await?
            else {
                panic!("failed job must remain in embedding_jobs");
            };
            assert_eq!(attempts, attempt);
            assert_eq!(
                status,
                if attempt < EMBEDDING_JOB_MAX_ATTEMPTS {
                    "pending"
                } else {
                    "failed"
                }
            );
            assert!(
                last_error
                    .as_deref()
                    .is_some_and(|err| err.contains("forced embedding failure")),
                "last_error must preserve the embedding failure"
            );

            // Backoff: a pending retry is not immediately re-claimable.
            if attempt < EMBEDDING_JOB_MAX_ATTEMPTS {
                let immediate = engine.drain_embedding_jobs(10).await?;
                assert_eq!(
                    immediate.processed, 0,
                    "backoff must gate immediate re-claim of a pending retry"
                );
                clear_embedding_backoff(pg.pool_for_tests(), outcome.memory_id).await?;
            }
        }

        // `failed` is terminal: no drain reclaims it (requeue is reconcile-only).
        let drain = engine.drain_embedding_jobs(10).await?;
        assert_eq!(drain.processed, 0);
        assert_eq!(drain.failed, 0);
        assert_eq!(
            count_fact_embeddings(pg.pool_for_tests(), outcome.memory_id).await?,
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
async fn transient_failure_releases_claim_without_burning_attempts()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner_fixture();
        let engine = engine_for(pg.clone(), Some(Arc::new(FailingEmbedding)));
        let outcome = engine
            .fact_ingest(
                &AuthzContext::single_owner(&owner, AuthPath::HostBearer),
                fact_draft(&owner, "outage fact"),
            )
            .await?;

        // Transient provider failure says nothing about this job; release
        // the claim without burning an attempt.
        let drain = engine.drain_embedding_jobs(10).await?;
        assert_eq!(drain.processed, 0);
        assert_eq!(drain.failed, 0);
        let (status, attempts, last_error) =
            load_embedding_job(pg.pool_for_tests(), outcome.memory_id)
                .await?
                .expect("released job must remain queued");
        assert_eq!(status, "pending");
        assert_eq!(attempts, 0, "release must not burn a retry attempt");
        assert!(
            last_error
                .as_deref()
                .is_some_and(|err| err.contains("forced embedding failure")),
            "release still records the outage on the job row"
        );

        // The release backoff gates immediate re-claim (no hot loop across
        // drain passes during an outage)...
        let immediate = engine.drain_embedding_jobs(10).await?;
        assert_eq!(immediate.processed, 0);
        // ...but after the window the job is claimable again, still with
        // zero attempts burned no matter how long the outage lasted.
        clear_embedding_backoff(pg.pool_for_tests(), outcome.memory_id).await?;
        let retry = engine.drain_embedding_jobs(10).await?;
        assert_eq!(retry.processed, 0);
        let (status, attempts, _) = load_embedding_job(pg.pool_for_tests(), outcome.memory_id)
            .await?
            .expect("job still queued");
        assert_eq!(status, "pending");
        assert_eq!(attempts, 0);
        Ok(())
    }
    .await;
    drop(pg);
    drop_db(&db_name).await?;
    result
}

#[tokio::test]
async fn permanently_rejected_input_goes_terminal_and_batch_mates_still_embed()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner_fixture();
        let engine = engine_for(pg.clone(), Some(Arc::new(PoisonTextEmbedding)));
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        let good = engine
            .fact_ingest(&authz, fact_draft(&owner, "healthy fact"))
            .await?;
        let poison = engine
            .fact_ingest(&authz, fact_draft(&owner, "poison fact"))
            .await?;

        // The batch call is rejected without naming the culprit; the drain
        // isolates per item: the healthy memory embeds, the poison job goes
        // terminal on its FIRST attempt instead of retrying a hopeless
        // input four more times.
        let drain = engine.drain_embedding_jobs(10).await?;
        assert_eq!(drain.processed, 2);
        assert_eq!(drain.failed, 1);
        assert_eq!(
            count_fact_embeddings(pg.pool_for_tests(), good.memory_id).await?,
            1
        );
        assert_eq!(
            count_fact_embeddings(pg.pool_for_tests(), poison.memory_id).await?,
            0
        );
        let (status, attempts, last_error) =
            load_embedding_job(pg.pool_for_tests(), poison.memory_id)
                .await?
                .expect("poison job stays visible in embedding_jobs");
        assert_eq!(status, "failed");
        assert_eq!(attempts, 1);
        assert!(
            last_error
                .as_deref()
                .is_some_and(|err| err.starts_with("permanent: ")),
            "terminal cause must carry the permanent marker, got {last_error:?}"
        );

        // Reconcile requeues retry-exhausted jobs but must not resurrect a
        // permanently rejected input — the provider would reject it forever.
        let reconciled = pg
            .reconcile_embeddings(EmbeddingReconcileOptions {
                non_embeddable_schemas: &[],
                model_id: "stub-fact-embed",
                scope: EmbeddingReconcileScope::MissingOnly,
                limit: None,
            })
            .await?;
        assert_eq!(
            reconciled.enqueued, 0,
            "permanent rejection must survive reconcile"
        );
        let (status, _, _) = load_embedding_job(pg.pool_for_tests(), poison.memory_id)
            .await?
            .expect("poison job still terminal");
        assert_eq!(status, "failed");
        Ok(())
    }
    .await;
    drop(pg);
    drop_db(&db_name).await?;
    result
}

#[tokio::test]
async fn over_limit_input_is_rescued_as_chunked_embeddings()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner_fixture();
        let engine = engine_for(pg.clone(), Some(Arc::new(TokenCapEmbedding)));
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        let oversized = engine
            .fact_ingest(&authz, fact_draft(&owner, &"x".repeat(10_000)))
            .await?;

        // The full text (>cap) is rejected; bisection embeds every piece
        // (~10k → two ~5k halves rejected → four ~2.5k quarters accepted),
        // so the job completes with one version of multiple chunk rows —
        // the whole text stays semantically covered, not just a prefix.
        let drain = engine.drain_embedding_jobs(10).await?;
        assert_eq!(drain.processed, 1);
        assert_eq!(drain.failed, 0, "rescued job must not count as failed");
        let chunk_count = count_fact_embeddings(pg.pool_for_tests(), oversized.memory_id).await?;
        assert!(
            chunk_count >= 2,
            "over-limit memory must carry multiple chunk embeddings, got {chunk_count}"
        );
        let distinct_versions: i64 = sqlx::query_scalar(
            "SELECT count(DISTINCT embedding_version)
               FROM proxima_core.embeddings
              WHERE entity_id = $1",
        )
        .bind(oversized.memory_id.into_inner())
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(distinct_versions, 1, "chunks must share one version");
        assert!(
            load_embedding_job(pg.pool_for_tests(), oversized.memory_id)
                .await?
                .is_none(),
            "rescued job must complete, not stay queued or terminal"
        );
        Ok(())
    }
    .await;
    drop(pg);
    drop_db(&db_name).await?;
    result
}

#[tokio::test]
async fn long_input_rejected_at_every_length_still_goes_terminal()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner_fixture();
        let engine = engine_for(pg.clone(), Some(Arc::new(PoisonTextEmbedding)));
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        // "poison" leads the text, so every truncated prefix still contains
        // it: the provider rejects at every length and the rescue must not
        // mask a genuinely invalid input.
        let poison = engine
            .fact_ingest(
                &authz,
                fact_draft(&owner, &format!("poison {}", "x".repeat(10_000))),
            )
            .await?;

        let drain = engine.drain_embedding_jobs(10).await?;
        assert_eq!(drain.processed, 1);
        assert_eq!(drain.failed, 1);
        assert_eq!(
            count_fact_embeddings(pg.pool_for_tests(), poison.memory_id).await?,
            0
        );
        let (status, _, last_error) = load_embedding_job(pg.pool_for_tests(), poison.memory_id)
            .await?
            .expect("poison job stays visible in embedding_jobs");
        assert_eq!(status, "failed");
        assert!(
            last_error
                .as_deref()
                .is_some_and(|err| err.starts_with("permanent: ")),
            "terminal cause must carry the permanent marker, got {last_error:?}"
        );
        Ok(())
    }
    .await;
    drop(pg);
    drop_db(&db_name).await?;
    result
}

/// One input that kills the provider must not hold its batch-mates.
///
/// A provider that dies *because of* an input reports the same transient
/// as an outage. Releasing the whole claim then requeues the poison with
/// its batch forever: the release path burns no attempts, so the job never
/// reaches the cap. After a transient batch failure, probe the provider;
/// if it answers, isolate the jobs individually.
#[tokio::test]
async fn one_crashing_input_does_not_block_its_batch() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner_fixture();
        let engine = engine_for(pg.clone(), Some(Arc::new(CrashOnInputEmbedding)));
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);

        let mut healthy = Vec::new();
        for label in ["first fact", "second fact", "third fact"] {
            healthy.push(
                engine
                    .fact_ingest(&authz, fact_draft(&owner, label))
                    .await?,
            );
        }
        let poisoned = engine
            .fact_ingest(&authz, fact_draft(&owner, "poison fact"))
            .await?;

        // Inline embedding at write time already failed for all four (the
        // batch call carries the poison), leaving four pending jobs.
        let drain = engine.drain_embedding_jobs(10).await?;
        assert_eq!(
            drain.processed, 4,
            "every claimed job must be accounted for"
        );
        assert_eq!(drain.failed, 1, "only the poisonous input should fail");

        for outcome in &healthy {
            assert_eq!(
                count_fact_embeddings(pg.pool_for_tests(), outcome.memory_id).await?,
                1,
                "a batch-mate of a crashing input must still be embedded"
            );
            assert!(
                load_embedding_job(pg.pool_for_tests(), outcome.memory_id)
                    .await?
                    .is_none(),
                "an embedded job must be completed, not left pending"
            );
        }

        // The poisonous job is now attributable, so it burns attempts and
        // will reach the cap instead of cycling forever at attempts = 0.
        let Some((status, attempts, last_error)) =
            load_embedding_job(pg.pool_for_tests(), poisoned.memory_id).await?
        else {
            panic!("the failing job must remain in embedding_jobs");
        };
        assert_eq!(status, "pending");
        assert_eq!(attempts, 1, "an isolated failure must burn an attempt");
        assert!(
            last_error
                .as_deref()
                .is_some_and(|err| err.contains("runner process no longer running")),
            "last_error must name the provider failure: {last_error:?}"
        );
        assert_eq!(
            count_fact_embeddings(pg.pool_for_tests(), poisoned.memory_id).await?,
            0
        );

        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db_name).await;
    result
}

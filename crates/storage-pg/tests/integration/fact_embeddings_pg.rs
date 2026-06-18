//! Fact render-text and embedding coverage. Compiled by default; live
//! PG execution is left to the orchestrator.

use std::sync::Arc;

use proxima_core::llm::{EMBEDDING_DIM, EMBEDDING_JOB_MAX_ATTEMPTS, EmbeddingClient, LlmError};
use proxima_core::test_fixtures::ConstantEmbedding;
use proxima_core::verbs::event_ingest::EventDraft;
use proxima_core::{
    AuthPath, AuthzContext, EntityKind, FlavorRegistry, Owner, SourceBatchId, Storage,
};
use proxima_storage_pg::{
    EmbeddingReconcileOptions, EmbeddingReconcileOutcome, EmbeddingReconcileScope,
};
use uuid::Uuid;

use crate::common::personality::TestFactV1;
use crate::common::{drop_db, fresh_pg, owner_fixture};

#[derive(Debug)]
struct FailingEmbedding;

#[async_trait::async_trait]
impl EmbeddingClient for FailingEmbedding {
    async fn embed(&self, _text: &str) -> Result<Vec<f32>, LlmError> {
        Err(LlmError::Embed("forced embedding failure".into()))
    }

    fn model_id(&self) -> &'static str {
        "stub-fact-embed"
    }

    fn dim(&self) -> usize {
        EMBEDDING_DIM
    }
}

fn engine_for(
    pg: proxima_storage_pg::PgStorage,
    embed: Option<Arc<dyn EmbeddingClient>>,
) -> proxima_core::Engine {
    let mut registry = FlavorRegistry::new();
    registry.add_fact_schema::<TestFactV1>();
    let engine = proxima_core::Engine::new(registry.freeze()).with_storage(pg.into_handle());
    if let Some(embed) = embed {
        engine.with_embed(embed)
    } else {
        engine
    }
}

fn fact_draft(owner: &Owner, label: &str) -> EventDraft {
    let now = time::OffsetDateTime::now_utc();
    let payload = TestFactV1 {
        label: label.to_string(),
    };
    let mut draft = EventDraft::from_payload(
        owner,
        "proxima-test/fact-embedding",
        SourceBatchId::new(Uuid::now_v7()),
        &payload,
        now,
    );
    draft.org_id = None;
    draft
}

async fn count_fact_embeddings(
    pool: &sqlx::PgPool,
    memory_id: proxima_core::MemoryId,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT count(*)::bigint
           FROM proxima_core.embeddings
          WHERE entity_kind = 'Fact'
            AND entity_id = $1
            AND model_id = 'stub-fact-embed'",
    )
    .bind(memory_id.into_inner())
    .fetch_one(pool)
    .await
}

async fn count_embedding_jobs(
    pool: &sqlx::PgPool,
    memory_id: proxima_core::MemoryId,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT count(*)::bigint
           FROM proxima_core.embedding_jobs
          WHERE entity_kind = 'Fact'
            AND entity_id = $1
            AND model_id = 'stub-fact-embed'",
    )
    .bind(memory_id.into_inner())
    .fetch_one(pool)
    .await
}

async fn count_embedding_jobs_for_model(
    pool: &sqlx::PgPool,
    model_id: &str,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT count(*)::bigint
           FROM proxima_core.embedding_jobs
          WHERE model_id = $1",
    )
    .bind(model_id)
    .fetch_one(pool)
    .await
}

async fn load_embedding_job(
    pool: &sqlx::PgPool,
    memory_id: proxima_core::MemoryId,
) -> Result<Option<(String, i32, Option<String>)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT status::text, attempts, last_error
           FROM proxima_core.embedding_jobs
          WHERE entity_kind = 'Fact'
            AND entity_id = $1
            AND model_id = 'stub-fact-embed'",
    )
    .bind(memory_id.into_inner())
    .fetch_optional(pool)
    .await
}

async fn load_memory_text(
    pool: &sqlx::PgPool,
    memory_id: proxima_core::MemoryId,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT text FROM proxima_core.memories WHERE memory_id = $1")
        .bind(memory_id.into_inner())
        .fetch_one(pool)
        .await
}

async fn reconcile_stub_fact_embeddings(
    pg: &proxima_storage_pg::PgStorage,
    scope: EmbeddingReconcileScope,
) -> Result<EmbeddingReconcileOutcome, proxima_core::StorageError> {
    pg.reconcile_embeddings(EmbeddingReconcileOptions {
        model_id: "stub-fact-embed",
        scope,
        limit: None,
    })
    .await
}

#[tokio::test]
async fn fact_ingest_with_embed_client_enqueues_pending_embedding_job_once()
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
        let draft = fact_draft(&owner, "rendered fact");
        let outcome = engine
            .event_ingest(
                &AuthzContext::single_owner(&owner, AuthPath::System),
                draft.clone(),
            )
            .await?;
        let replay = engine
            .event_ingest(&AuthzContext::single_owner(&owner, AuthPath::System), draft)
            .await?;

        assert!(!outcome.idempotent_replay);
        assert!(replay.idempotent_replay);
        assert_eq!(replay.memory_id, outcome.memory_id);
        assert_eq!(
            load_memory_text(pg.pool(), outcome.memory_id).await?,
            Some("rendered fact".to_string()),
        );
        assert_eq!(
            count_fact_embeddings(pg.pool(), outcome.memory_id).await?,
            0
        );
        assert_eq!(count_embedding_jobs(pg.pool(), outcome.memory_id).await?, 1);
        Ok(())
    }
    .await;
    drop(pg);
    drop_db(&db_name).await?;
    result
}

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
            .event_ingest(
                &AuthzContext::single_owner(&owner, AuthPath::System),
                fact_draft(&owner, "drained fact"),
            )
            .await?;

        assert_eq!(
            count_fact_embeddings(pg.pool(), outcome.memory_id).await?,
            0
        );
        assert_eq!(count_embedding_jobs(pg.pool(), outcome.memory_id).await?, 1);

        let drain = engine.drain_embedding_jobs(10).await?;
        assert_eq!(drain.processed, 1);
        assert_eq!(drain.failed, 0);
        assert_eq!(
            count_fact_embeddings(pg.pool(), outcome.memory_id).await?,
            1
        );
        assert_eq!(count_embedding_jobs(pg.pool(), outcome.memory_id).await?, 0);
        Ok(())
    }
    .await;
    drop(pg);
    drop_db(&db_name).await?;
    result
}

#[tokio::test]
async fn failed_embedding_jobs_retry_until_attempt_cap() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner_fixture();
        let engine = engine_for(pg.clone(), Some(Arc::new(FailingEmbedding)));
        let outcome = engine
            .event_ingest(
                &AuthzContext::single_owner(&owner, AuthPath::System),
                fact_draft(&owner, "failing fact"),
            )
            .await?;

        for attempt in 1..=EMBEDDING_JOB_MAX_ATTEMPTS {
            let drain = engine.drain_embedding_jobs(10).await?;
            assert_eq!(drain.processed, 1);
            assert_eq!(drain.failed, 1);
            let Some((status, attempts, last_error)) =
                load_embedding_job(pg.pool(), outcome.memory_id).await?
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
        }

        let drain = engine.drain_embedding_jobs(10).await?;
        assert_eq!(drain.processed, 0);
        assert_eq!(drain.failed, 0);
        assert_eq!(
            count_fact_embeddings(pg.pool(), outcome.memory_id).await?,
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
            .event_ingest(
                &AuthzContext::single_owner(&owner, AuthPath::System),
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
            load_embedding_job(pg.pool(), outcome.memory_id)
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
async fn fact_embedding_backfill_heals_no_client_ingest() -> Result<(), Box<dyn std::error::Error>>
{
    let (pg, db_name) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner_fixture();
        let engine = engine_for(pg.clone(), None);
        let first = engine
            .event_ingest(
                &AuthzContext::single_owner(&owner, AuthPath::System),
                fact_draft(&owner, "backfill fact one"),
            )
            .await?;
        let second = engine
            .event_ingest(
                &AuthzContext::single_owner(&owner, AuthPath::System),
                fact_draft(&owner, "backfill fact two"),
            )
            .await?;

        assert_eq!(
            load_memory_text(pg.pool(), first.memory_id).await?,
            Some("backfill fact one".to_string()),
        );
        assert_eq!(count_embedding_jobs(pg.pool(), first.memory_id).await?, 0);
        assert_eq!(count_embedding_jobs(pg.pool(), second.memory_id).await?, 0);

        engine
            .set_embed_client(Some(Arc::new(ConstantEmbedding::prefixed(
                "stub-fact-embed",
                &[0.25, 0.5, 0.75],
            ))))
            .await;
        assert_eq!(engine.backfill_fact_embeddings(&owner, 1).await?, 1);
        assert_eq!(
            count_embedding_jobs(pg.pool(), first.memory_id).await?
                + count_embedding_jobs(pg.pool(), second.memory_id).await?,
            1
        );
        assert_eq!(
            count_fact_embeddings(pg.pool(), first.memory_id).await?
                + count_fact_embeddings(pg.pool(), second.memory_id).await?,
            0
        );

        let drain = engine.drain_embedding_jobs(10).await?;
        assert_eq!(drain.processed, 1);
        assert_eq!(drain.failed, 0);
        assert_eq!(
            count_fact_embeddings(pg.pool(), first.memory_id).await?
                + count_fact_embeddings(pg.pool(), second.memory_id).await?,
            1
        );

        assert_eq!(engine.backfill_fact_embeddings(&owner, 10).await?, 1);
        let drain = engine.drain_embedding_jobs(10).await?;
        assert_eq!(drain.processed, 1);
        assert_eq!(drain.failed, 0);
        assert_eq!(
            count_fact_embeddings(pg.pool(), first.memory_id).await?
                + count_fact_embeddings(pg.pool(), second.memory_id).await?,
            2
        );
        assert_eq!(count_embedding_jobs(pg.pool(), first.memory_id).await?, 0);
        assert_eq!(count_embedding_jobs(pg.pool(), second.memory_id).await?, 0);
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
                .event_ingest(
                    &AuthzContext::single_owner(&owner, AuthPath::System),
                    fact_draft(&owner, label),
                )
                .await?;
        }
        let stale = engine
            .event_ingest(
                &AuthzContext::single_owner(&owner, AuthPath::System),
                fact_draft(&owner, "reconcile stale fact"),
            )
            .await?;
        let other_model_embedding = vec![0.125; EMBEDDING_DIM];
        pg.upsert_memory_embedding(
            &owner,
            EntityKind::Fact,
            stale.memory_id,
            "other-model",
            EMBEDDING_DIM,
            &other_model_embedding,
        )
        .await?;

        assert_eq!(
            count_embedding_jobs_for_model(pg.pool(), "stub-fact-embed").await?,
            0
        );
        let first =
            reconcile_stub_fact_embeddings(&pg, EmbeddingReconcileScope::MissingOnly).await?;
        assert_eq!(first.scanned, 4);
        assert_eq!(first.enqueued, 3);
        assert_eq!(first.skipped, 1);
        assert_eq!(
            count_embedding_jobs_for_model(pg.pool(), "stub-fact-embed").await?,
            3
        );
        assert_eq!(count_embedding_jobs(pg.pool(), stale.memory_id).await?, 0);

        let second =
            reconcile_stub_fact_embeddings(&pg, EmbeddingReconcileScope::MissingOnly).await?;
        assert_eq!(second.scanned, 4);
        assert_eq!(second.enqueued, 0);
        assert_eq!(second.skipped, 4);
        assert_eq!(
            count_embedding_jobs_for_model(pg.pool(), "stub-fact-embed").await?,
            3
        );
        assert_eq!(count_embedding_jobs(pg.pool(), stale.memory_id).await?, 0);

        let include_stale =
            reconcile_stub_fact_embeddings(&pg, EmbeddingReconcileScope::IncludeStale).await?;
        assert_eq!(include_stale.scanned, 4);
        assert_eq!(include_stale.enqueued, 1);
        assert_eq!(include_stale.skipped, 3);
        assert_eq!(
            count_embedding_jobs_for_model(pg.pool(), "stub-fact-embed").await?,
            4
        );
        assert_eq!(count_embedding_jobs(pg.pool(), stale.memory_id).await?, 1);

        let include_stale_again =
            reconcile_stub_fact_embeddings(&pg, EmbeddingReconcileScope::IncludeStale).await?;
        assert_eq!(include_stale_again.scanned, 4);
        assert_eq!(include_stale_again.enqueued, 0);
        assert_eq!(include_stale_again.skipped, 4);
        assert_eq!(
            count_embedding_jobs_for_model(pg.pool(), "stub-fact-embed").await?,
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
            .event_ingest(
                &AuthzContext::single_owner(&owner, AuthPath::System),
                fact_draft(&owner, "reconcile drain fact"),
            )
            .await?;
        reconcile_stub_fact_embeddings(&pg, EmbeddingReconcileScope::MissingOnly).await?;

        let client = ConstantEmbedding::prefixed("stub-fact-embed", &[0.25, 0.5, 0.75]);
        let drain = pg.drain_embedding_jobs_inline(&client, 10).await?;
        assert_eq!(drain.embedded, 1);
        assert_eq!(drain.failed, 0);
        assert_eq!(
            count_fact_embeddings(pg.pool(), outcome.memory_id).await?,
            1
        );
        assert_eq!(count_embedding_jobs(pg.pool(), outcome.memory_id).await?, 0);
        Ok(())
    }
    .await;
    drop(pg);
    drop_db(&db_name).await?;
    result
}

#[tokio::test]
async fn fact_ingest_without_embed_client_still_succeeds() -> Result<(), Box<dyn std::error::Error>>
{
    let (pg, db_name) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner_fixture();
        let engine = engine_for(pg.clone(), None);
        let outcome = engine
            .event_ingest(
                &AuthzContext::single_owner(&owner, AuthPath::System),
                fact_draft(&owner, "no client fact"),
            )
            .await?;

        assert_eq!(
            load_memory_text(pg.pool(), outcome.memory_id).await?,
            Some("no client fact".to_string()),
        );
        assert_eq!(
            count_fact_embeddings(pg.pool(), outcome.memory_id).await?,
            0
        );
        assert_eq!(count_embedding_jobs(pg.pool(), outcome.memory_id).await?, 0);
        Ok(())
    }
    .await;
    drop(pg);
    drop_db(&db_name).await?;
    result
}

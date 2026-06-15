//! Fact render-text and embedding coverage. Compiled by default; live
//! PG execution is left to the orchestrator.

use std::sync::Arc;

use proxima_core::llm::{EMBEDDING_DIM, EMBEDDING_JOB_MAX_ATTEMPTS, EmbeddingClient, LlmError};
use proxima_core::verbs::event_ingest::EventDraft;
use proxima_core::verbs::query::MemoryStore;
use proxima_core::{
    AuthPath, AuthzContext, FactPayload, FlavorRegistry, Owner, SchemaVersion, SourceBatchId,
    SourceId, Storage,
};
use uuid::Uuid;

use crate::common::personality::TestFactV1;
use crate::common::{drop_db, fresh_pg, owner_fixture};

#[derive(Debug)]
struct StubEmbedding;

#[async_trait::async_trait]
impl EmbeddingClient for StubEmbedding {
    async fn embed(&self, _text: &str) -> Result<Vec<f32>, LlmError> {
        Ok(padded_embedding([0.25, 0.5, 0.75]))
    }

    fn model_id(&self) -> &'static str {
        "stub-fact-embed"
    }

    fn dim(&self) -> usize {
        EMBEDDING_DIM
    }
}

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

fn padded_embedding(prefix: [f32; 3]) -> Vec<f32> {
    let mut embedding = vec![0.0; EMBEDDING_DIM];
    embedding[..prefix.len()].copy_from_slice(&prefix);
    embedding
}

fn engine_for(
    pg: proxima_storage_pg::PgStorage,
    embed: Option<Arc<dyn EmbeddingClient>>,
) -> proxima_core::Engine {
    let mut registry = FlavorRegistry::new();
    registry.add_fact_schema::<TestFactV1>();
    let engine = proxima_core::Engine::new(registry.freeze(), MemoryStore::new())
        .with_storage(pg.into_handle());
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
    let payload = serde_json::to_value(payload).expect("test payload serializes");
    EventDraft {
        source_id: SourceId::new("proxima-test/fact-embedding"),
        source_batch_id: SourceBatchId::new(Uuid::now_v7()),
        principal: owner.principal.clone(),
        org_id: None,
        author_personality_instance_id: None,
        schema_id: TestFactV1::schema_id(),
        schema_version: SchemaVersion::new(TestFactV1::SCHEMA_VERSION),
        payload: proxima_core::canonical_json_bytes(&payload),
        rendered_text: None,
        observed_at: now,
        occurred_at: now,
        citation: None,
    }
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

#[tokio::test]
async fn fact_ingest_with_embed_client_enqueues_pending_embedding_job_once()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = fresh_pg().await else {
        return Ok(());
    };
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner_fixture();
        let engine = engine_for(pg.clone(), Some(Arc::new(StubEmbedding)));
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
    let Some((pg, db_name)) = fresh_pg().await else {
        return Ok(());
    };
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner_fixture();
        let engine = engine_for(pg.clone(), Some(Arc::new(StubEmbedding)));
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
    let Some((pg, db_name)) = fresh_pg().await else {
        return Ok(());
    };
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
    let Some((pg, db_name)) = fresh_pg().await else {
        return Ok(());
    };
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner_fixture();
        let engine = engine_for(pg.clone(), Some(Arc::new(StubEmbedding)));
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
    let Some((pg, db_name)) = fresh_pg().await else {
        return Ok(());
    };
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

        engine.set_embed_client(Some(Arc::new(StubEmbedding))).await;
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
async fn fact_ingest_without_embed_client_still_succeeds() -> Result<(), Box<dyn std::error::Error>>
{
    let Some((pg, db_name)) = fresh_pg().await else {
        return Ok(());
    };
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

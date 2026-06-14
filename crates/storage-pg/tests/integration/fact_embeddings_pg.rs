//! Fact render-text and embedding coverage. Compiled by default; live
//! PG execution is left to the orchestrator.

use std::sync::Arc;

use proxima_core::llm::{EmbeddingClient, LlmError};
use proxima_core::verbs::event_ingest::EventDraft;
use proxima_core::verbs::query::MemoryStore;
use proxima_core::{
    AuthPath, AuthzContext, FactPayload, FlavorRegistry, Owner, SchemaVersion, SourceBatchId,
    SourceId,
};
use uuid::Uuid;

use crate::common::personality::TestFactV1;
use crate::common::{drop_db, fresh_pg, owner_fixture};

#[derive(Debug)]
struct StubEmbedding;

#[async_trait::async_trait]
impl EmbeddingClient for StubEmbedding {
    async fn embed(&self, _text: &str) -> Result<Vec<f32>, LlmError> {
        Ok(vec![0.25, 0.5, 0.75])
    }

    fn model_id(&self) -> &'static str {
        "stub-fact-embed"
    }

    fn dim(&self) -> usize {
        3
    }
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
async fn fact_ingest_with_embed_client_writes_text_and_embedding()
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
                fact_draft(&owner, "rendered fact"),
            )
            .await?;

        assert_eq!(
            load_memory_text(pg.pool(), outcome.memory_id).await?,
            Some("rendered fact".to_string()),
        );
        assert_eq!(
            count_fact_embeddings(pg.pool(), outcome.memory_id).await?,
            1
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
        let outcome = engine
            .event_ingest(
                &AuthzContext::single_owner(&owner, AuthPath::System),
                fact_draft(&owner, "backfill fact"),
            )
            .await?;

        assert_eq!(
            load_memory_text(pg.pool(), outcome.memory_id).await?,
            Some("backfill fact".to_string()),
        );
        assert_eq!(
            count_fact_embeddings(pg.pool(), outcome.memory_id).await?,
            0
        );

        engine.set_embed_client(Some(Arc::new(StubEmbedding))).await;
        assert_eq!(engine.backfill_fact_embeddings(&owner, 10).await?, 1);
        assert_eq!(
            count_fact_embeddings(pg.pool(), outcome.memory_id).await?,
            1
        );
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
        Ok(())
    }
    .await;
    drop(pg);
    drop_db(&db_name).await?;
    result
}

use std::sync::Arc;

mod common;

use async_trait::async_trait;
use common::{drop_db, fresh_pg};
use proxima_core::engine::Engine;
use proxima_core::llm::{EMBEDDING_DIM, EmbeddingClient, LlmError};
use proxima_core::mcp::{HandleTable, McpAuthorContext, McpToolCtx, OutputMode};
use proxima_core::{
    AuthPath, AuthzContext, FlavorRegistry, OrgId, Owner, PersonalityInstanceId, Principal, UserId,
};
use serde_json::json;
use sqlx::Row;

#[derive(Debug)]
struct FixedEmbedding;

#[async_trait]
impl EmbeddingClient for FixedEmbedding {
    async fn embed(&self, _text: &str) -> Result<Vec<f32>, LlmError> {
        Ok(vec![0.0; EMBEDDING_DIM])
    }

    fn model_id(&self) -> &'static str {
        "test-utterance-embed"
    }

    fn dim(&self) -> usize {
        EMBEDDING_DIM
    }
}

#[tokio::test]
async fn record_utterance_stamps_personality_and_sidecar() -> Result<(), Box<dyn std::error::Error>>
{
    let Some((pg, db_name)) = fresh_pg().await else {
        return Ok(());
    };

    let registry = FlavorRegistry::new();
    let frozen_inner = registry.freeze();
    let frozen = Arc::new(frozen_inner.clone());
    let owner = Owner {
        principal: Principal::User(UserId::new(uuid::Uuid::nil())),
        org_id: OrgId::new(uuid::Uuid::nil()),
    };
    let personality = PersonalityInstanceId::new(uuid::Uuid::now_v7());
    let engine = Arc::new(
        Engine::new(frozen_inner)
            .with_storage(pg.clone().into_handle())
            .with_embed(Arc::new(FixedEmbedding)),
    );

    let descriptor = frozen
        .list_mcp_tools()
        .iter()
        .find(|tool| tool.name == "core/record_utterance")
        .expect("registered tool");
    let output = (descriptor.call)(
        McpToolCtx {
            pool: pg.pool().clone(),
            owner: owner.clone(),
            authz: AuthzContext::single_owner(&owner, AuthPath::System),
            handles: Some(Arc::new(HandleTable::new())),
            mode: OutputMode::Handles,
            registry: frozen,
            author: McpAuthorContext {
                model_id: "codex-test".into(),
                client_name: "codex".into(),
                client_version: "1".into(),
                personality_instance_id: Some(personality),
                caller_self_perspective: None,
            },
            caller_self_perspective: None,
            master_token_id: None,
            engine: Some(engine),
        },
        json!({
            "speaker": "user",
            "conversation_id": "thread-1",
            "text": "The citation phase needs utterance Facts.",
            "idempotency_key": "utterance-pg-personality"
        }),
    )
    .await?;
    assert_eq!(output["idempotent_replay"], json!(false));
    let handle = output["handle"].as_str().expect("handle");
    assert!(handle.starts_with('F'));

    let row = sqlx::query(
        r"SELECT m.personality_instance_id, u.speaker, u.conversation_id, u.text
           FROM proxima_core.memories m
           JOIN proxima_core.utterance_v1 u USING (memory_id)
           WHERE m.schema_id = 'core/utterance-v1'",
    )
    .fetch_one(pg.pool())
    .await?;
    assert_eq!(
        row.get::<uuid::Uuid, _>("personality_instance_id"),
        personality.into_inner()
    );
    assert_eq!(row.get::<String, _>("speaker"), "user");
    assert_eq!(row.get::<String, _>("conversation_id"), "thread-1");
    assert_eq!(
        row.get::<String, _>("text"),
        "The citation phase needs utterance Facts."
    );
    let memory_id: uuid::Uuid = sqlx::query_scalar(
        "SELECT memory_id FROM proxima_core.memories WHERE schema_id = 'core/utterance-v1'",
    )
    .fetch_one(pg.pool())
    .await?;
    let jobs: i64 = sqlx::query_scalar(
        "SELECT count(*)::bigint
           FROM proxima_core.embedding_jobs
          WHERE entity_kind = 'Fact'
            AND entity_id = $1
            AND model_id = 'test-utterance-embed'
            AND status = 'pending'",
    )
    .bind(memory_id)
    .fetch_one(pg.pool())
    .await?;
    assert_eq!(jobs, 1);

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

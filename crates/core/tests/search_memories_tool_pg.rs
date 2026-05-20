mod common;

use std::sync::Arc;

use async_trait::async_trait;
use common::{drop_db, fresh_pg, owner_fixture};
use proxima_core::auth::NoAuth;
use proxima_core::llm::{EmbeddingClient, LlmError};
use proxima_core::personality::{
    PersonalityInstanceId, PersonalityTool, PersonalityToolContext, substrate_pack,
};
use proxima_core::verbs::query::MemoryStore;
use proxima_core::{
    Engine, FlavorRegistry, HandleTable, MemoryId, OwnerPrincipalKind, Principal, WakeChainDepth,
};
use uuid::Uuid;

#[derive(Debug)]
struct FixedEmbedding;

#[async_trait]
impl EmbeddingClient for FixedEmbedding {
    async fn embed(&self, _text: &str) -> Result<Vec<f32>, LlmError> {
        Ok(vec![1.0, 0.0, 0.0])
    }

    fn model_id(&self) -> &'static str {
        "test-embed"
    }

    fn dim(&self) -> usize {
        3
    }
}

#[tokio::test]
async fn search_memories_returns_handles_and_records_read_log()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = fresh_pg().await else {
        return Ok(());
    };
    pg.run_migrations().await?;

    let owner = owner_fixture();
    let memory_id =
        insert_embedded_memory(&pg, &owner, "semantic-only memory", [1.0, 0.0, 0.0]).await?;
    let engine = Engine::new(
        FlavorRegistry::new().freeze(),
        MemoryStore::new(),
        Box::new(NoAuth::new(owner.principal.clone(), owner.clone())),
    )
    .with_storage(pg.clone().into_handle())
    .with_embed(Arc::new(FixedEmbedding));
    let tool = substrate_pack()
        .iter()
        .find(|tool| tool.tool_id() == "core/search_memories")
        .expect("search_memories substrate tool");
    let palette: Vec<Arc<dyn PersonalityTool>> = Vec::new();
    let read_log = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let ctx = PersonalityToolContext::new(
        &engine,
        &owner,
        "test/personality",
        PersonalityInstanceId::new(Uuid::nil()),
        MemoryId::new(Uuid::now_v7()),
        MemoryId::new(Uuid::now_v7()),
        WakeChainDepth::new(0),
        Vec::new(),
        Vec::new(),
        &palette,
        Arc::new(HandleTable::new()),
    )
    .with_read_log(read_log.clone());

    let result = tool
        .invoke(
            &ctx,
            serde_json::json!({
                "query": "not lexical",
                "mode": "semantic",
                "limit": 3,
            }),
        )
        .await?;

    assert_eq!(result.content["memories"][0]["memory"], "A1");
    assert_eq!(
        read_log.lock().await.as_slice(),
        &[(MemoryId::new(memory_id), WakeChainDepth::new(4))]
    );

    drop(engine);
    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

async fn insert_embedded_memory(
    pg: &proxima_storage_pg::PgStorage,
    owner: &proxima_core::Owner,
    text: &str,
    embedding: [f32; 3],
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let memory_id = Uuid::now_v7();
    let owner_kind = OwnerPrincipalKind::of(&owner.principal);
    let owner_principal_id = match &owner.principal {
        Principal::User(user) => user.into_inner(),
        Principal::Group(group) => group.into_inner(),
    };
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_principal_kind, owner_principal_id, owner_org_id,
             schema_id, schema_version, kind, text, operator_kind, model_id,
             prompt_version, personality_instance_id, wake_chain_depth)
         VALUES ($1, $2, $3, $4, 'test/search-abstraction-v1', 1,
                 'Abstraction', $5, 'Wake', 'test-model', 'test-v1',
                 '00000000-0000-0000-0000-000000000000'::uuid, 4)",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner.org_id.into_inner())
    .bind(text)
    .execute(pg.pool())
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.embeddings
            (entity_kind, entity_id, embedding_version, model_id, vec, dim,
             owner_principal_kind, owner_principal_id, owner_org_id)
         VALUES ('Abstraction', $1, 1, 'test-embed', $2, 3, $3, $4, $5)",
    )
    .bind(memory_id)
    .bind(Vec::from(embedding))
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner.org_id.into_inner())
    .execute(pg.pool())
    .await?;
    Ok(memory_id)
}

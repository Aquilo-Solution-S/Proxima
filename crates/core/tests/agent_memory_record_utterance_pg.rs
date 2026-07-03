use std::sync::Arc;

mod common;

use common::{ConstantEmbedding, drop_db, fresh_pg};
use proxima_core::engine::Engine;
use proxima_core::mcp::{HandleTable, McpAuthorContext, McpToolCtx, McpToolExtensions, OutputMode};
use proxima_core::{AuthPath, AuthzContext, FlavorRegistry, Owner, OwnerRef, UserId};
use serde_json::json;
use sqlx::Row;

#[tokio::test]
async fn record_utterance_persists_sidecar_and_embedding_job()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;

    let registry = FlavorRegistry::new();
    let frozen_inner = registry.freeze_or_panic_for_tests();
    let frozen = Arc::new(frozen_inner.clone());
    let owner: Owner = OwnerRef::Personal(UserId::new(uuid::Uuid::nil()));
    let engine = Arc::new(
        Engine::new(frozen_inner)
            .with_storage_ports(Arc::new(pg.clone()).storage_ports())
            .with_embed(Arc::new(ConstantEmbedding::zero("test-utterance-embed"))),
    );

    let descriptor = frozen
        .list_mcp_tools()
        .iter()
        .find(|tool| tool.name == "core_record_utterance")
        .expect("registered tool");
    let output = (descriptor.call)(
        McpToolCtx {
            owner,
            authz: AuthzContext::single_owner(&owner, AuthPath::HostBearer),
            handles: Some(Arc::new(HandleTable::new())),
            mode: OutputMode::Handles,
            registry: frozen,
            author: McpAuthorContext {
                model_id: "codex-test".into(),
                client_name: "codex".into(),
                client_version: "1".into(),
                caller_self_perspective: None,
            },
            caller_self_perspective: None,
            extensions: McpToolExtensions::with(pg.pool_for_tests().clone()),
            engine: Some(engine),
        },
        json!({
            "speaker": "user",
            "conversation_id": "thread-1",
            "text": "The citation phase needs utterance Facts.",
            "idempotency_key": "utterance-pg-sidecar"
        }),
    )
    .await?;
    assert_eq!(output["idempotent_replay"], json!(false));
    let handle = output["handle"].as_str().expect("handle");
    assert!(handle.starts_with('F'));

    let row = sqlx::query(
        r"SELECT u.speaker, u.conversation_id, u.text
           FROM proxima_core.memories m
           JOIN proxima_core.utterance_v1 u USING (memory_id)
           WHERE m.schema_id = 'core/utterance-v1'",
    )
    .fetch_one(pg.pool_for_tests())
    .await?;
    assert_eq!(row.get::<String, _>("speaker"), "user");
    assert_eq!(row.get::<String, _>("conversation_id"), "thread-1");
    assert_eq!(
        row.get::<String, _>("text"),
        "The citation phase needs utterance Facts."
    );
    let memory_id: uuid::Uuid = sqlx::query_scalar(
        "SELECT memory_id FROM proxima_core.memories WHERE schema_id = 'core/utterance-v1'",
    )
    .fetch_one(pg.pool_for_tests())
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
    .fetch_one(pg.pool_for_tests())
    .await?;
    assert_eq!(jobs, 1);

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

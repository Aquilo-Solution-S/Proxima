mod common;

use common::{drop_db, fresh_pg, owner_fixture};
use proxima_core::{SchemaId, SchemaVersion};
use proxima_storage_pg::verbs::derive_append::{DerivedDraft, append_derived_in_tx};

#[tokio::test]
async fn external_agent_abstraction_persists_with_replay() -> Result<(), Box<dyn std::error::Error>>
{
    let Some((pg, db_name)) = fresh_pg().await else {
        return Ok(());
    };

    let result = async {
        pg.run_migrations().await?;
        proxima_mcp_substrate::migrator().run(pg.pool()).await?;
        let owner = owner_fixture();
        let memory_id = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, b"derive-test-1");
        let draft = DerivedDraft {
            memory_id,
            owner,
            kind: "Abstraction",
            schema_id: SchemaId::new("proxima-mcp/agent-derivation-v1".into()),
            schema_version: SchemaVersion::new(1),
            text: "the agent view".into(),
            operator_kind: "ExternalAgent",
            model_id: "claude-opus-4.7",
            prompt_version: "mcp-agent-v1",
            personality_id: "external/mcp-agent",
            sidecar_table: Some("proxima_mcp.agent_derivation_v1"),
            sidecar_payload: Some(serde_json::json!({
                "title": "x",
                "body": "the agent view",
                "tags": [],
                "idempotency_key": null,
                "source_memory_ids": [],
                "model_id": "claude-opus-4.7",
                "client_name": "codex",
                "client_version": "1",
            })),
        };

        let mut tx = pg.pool().begin().await?;
        let outcome = append_derived_in_tx(&mut tx, &draft).await?;
        tx.commit().await?;
        assert_eq!(outcome.memory_id.into_inner(), memory_id);
        assert!(!outcome.idempotent_replay);

        let mut tx = pg.pool().begin().await?;
        let replay = append_derived_in_tx(&mut tx, &draft).await?;
        tx.commit().await?;
        assert!(replay.idempotent_replay);

        let row_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM proxima_mcp.agent_derivation_v1 WHERE memory_id = $1",
        )
        .bind(memory_id)
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(row_count, 1);
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn external_agent_perspective_persists() -> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = fresh_pg().await else {
        return Ok(());
    };

    let result = async {
        pg.run_migrations().await?;
        proxima_mcp_substrate::migrator().run(pg.pool()).await?;
        let memory_id = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, b"derive-test-2");
        let draft = DerivedDraft {
            memory_id,
            owner: owner_fixture(),
            kind: "Perspective",
            schema_id: SchemaId::new("proxima-mcp/agent-derivation-v1".into()),
            schema_version: SchemaVersion::new(1),
            text: "perspective body".into(),
            operator_kind: "ExternalAgent",
            model_id: "claude-opus-4.7",
            prompt_version: "mcp-agent-v1",
            personality_id: "external/mcp-agent",
            sidecar_table: Some("proxima_mcp.agent_derivation_v1"),
            sidecar_payload: Some(serde_json::json!({
                "title": "p",
                "body": "perspective body",
                "tags": [],
                "idempotency_key": null,
                "source_memory_ids": [],
                "model_id": "claude-opus-4.7",
                "client_name": "codex",
                "client_version": "1",
            })),
        };
        let mut tx = pg.pool().begin().await?;
        append_derived_in_tx(&mut tx, &draft).await?;
        tx.commit().await?;
        let kind: String =
            sqlx::query_scalar("SELECT kind FROM proxima_core.memories WHERE memory_id = $1")
                .bind(memory_id)
                .fetch_one(pg.pool())
                .await?;
        assert_eq!(kind, "Perspective");
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

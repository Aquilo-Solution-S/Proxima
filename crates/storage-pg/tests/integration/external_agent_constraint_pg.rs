use crate::common::{drop_db, fresh_pg, owner_fixture};

#[tokio::test]
async fn external_agent_memory_operator_kind_is_rejected() -> Result<(), Box<dyn std::error::Error>>
{
    let (pg, db_name) = fresh_pg().await;

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        let owner = owner_fixture();
        let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(&owner);
        let memory_id = uuid::Uuid::now_v7();
        let err = sqlx::query(
            "INSERT INTO proxima_core.memories
                (memory_id, owner_kind, owner_id, schema_id, schema_version, kind, text,
                 operator_kind, operator_id, input_contract_id, source_batch_id, model_id, prompt_version)
             VALUES ($1, $2, $3, 'core/agent-derivation-v1', 1,
                     'Abstraction', 'body', 'ExternalAgent',
                     '00000000-0000-0000-0000-000000000411'::uuid,
                     '00000000-0000-0000-0000-000000000412'::uuid, NULL,
                     'claude-opus-4.7', 'mcp-agent-v1')",
        )
        .bind(memory_id)
        .bind(owner_kind)
        .bind(owner_id)
        .execute(pg.pool())
        .await
        .expect_err("ExternalAgent is not a derived memory operator phase");
        assert!(err.to_string().contains("ExternalAgent"));
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

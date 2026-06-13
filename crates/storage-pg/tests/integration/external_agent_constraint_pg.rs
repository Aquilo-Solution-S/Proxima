use crate::common::{drop_db, fresh_pg, owner_fixture};

#[tokio::test]
async fn external_agent_operator_kind_is_admitted() -> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = fresh_pg().await else {
        return Ok(());
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        let owner = owner_fixture();
        let memory_id = uuid::Uuid::now_v7();
        sqlx::query(
            "INSERT INTO proxima_core.memories
                (memory_id, owner_principal_kind, owner_principal_id, owner_org_id,
                 schema_id, schema_version, kind, text, operator_kind, model_id,
                 prompt_version, personality_instance_id)
             VALUES ($1, 'User', $2, $3, 'proxima-agent-memory/agent-derivation-v1', 1,
                     'Abstraction', 'body', 'ExternalAgent', 'claude-opus-4.7',
                     'mcp-agent-v1',
                     '00000000-0000-0000-0000-000000000000'::uuid)",
        )
        .bind(memory_id)
        .bind(match owner.principal {
            proxima_core::Principal::User(u) => u.into_inner(),
            proxima_core::Principal::Group(g) => g.into_inner(),
        })
        .bind(owner.org_id.into_inner())
        .execute(pg.pool())
        .await?;
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

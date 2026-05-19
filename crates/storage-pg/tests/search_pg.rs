mod common;

use common::{drop_db, fresh_pg, owner_fixture};

use proxima_core::verbs::query::{EntityKind, MemorySearchRequest, SearchMode};
use proxima_core::{OrgId, Owner, OwnerPrincipalKind, Principal, SchemaId, Storage, UserId};
use uuid::Uuid;

#[tokio::test]
async fn semantic_search_ranks_nearest_vector_and_isolates_owner()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = fresh_pg().await else {
        return Ok(());
    };
    pg.run_migrations().await?;

    let owner = owner_fixture();
    let other_owner = Owner {
        principal: Principal::User(UserId::new(Uuid::from_u128(7))),
        org_id: OrgId::new(Uuid::nil()),
    };

    let near = insert_embedded_memory(&pg, &owner, "nearest", [0.99, 0.01, 0.0]).await?;
    let far = insert_embedded_memory(&pg, &owner, "orthogonal", [0.0, 1.0, 0.0]).await?;
    let other = insert_embedded_memory(&pg, &other_owner, "other owner", [1.0, 0.0, 0.0]).await?;

    let rows = pg
        .search_memories(
            &MemorySearchRequest {
                owner: owner.clone(),
                query: "semantic query".into(),
                mode: SearchMode::Semantic,
                limit: 10,
                kind: Some(EntityKind::Abstraction),
                schema_id: Some(SchemaId::new("test/search-abstraction-v1".into())),
                query_embedding: Some(vec![1.0, 0.0, 0.0]),
                embedding_model_id: Some("test-embed".into()),
                embedding_dim: Some(3),
                reader_personality_instance_id: None,
            },
            &[],
        )
        .await?;

    assert_eq!(
        rows.first().map(|row| row.memory_id.into_inner()),
        Some(near)
    );
    assert!(rows.iter().any(|row| row.memory_id.into_inner() == far));
    assert!(!rows.iter().any(|row| row.memory_id.into_inner() == other));

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

async fn insert_embedded_memory(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
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
                 '00000000-0000-0000-0000-000000000000'::uuid, 2)",
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

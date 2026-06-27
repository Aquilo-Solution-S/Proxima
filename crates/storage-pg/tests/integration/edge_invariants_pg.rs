use crate::common::personality::{
    TEST_ABSTRACTION_SCHEMA, TEST_PERSPECTIVE_SCHEMA, apply_test_schemas, ingest_other_fact,
    ingest_test_fact,
};
use proxima_core::{
    EntityKind, MemoryId, MemoryOperatorKind, Owner, OwnerPrincipalKind, Principal, RelationClass,
    UserId,
};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

fn owner_parts(owner: &Owner) -> (OwnerPrincipalKind, Uuid) {
    owner.columns()
}

fn other_owner() -> Owner {
    Principal::User(UserId::new(Uuid::now_v7()))
}

async fn insert_derived_memory(
    pg: &PgStorage,
    owner: &Owner,
    kind: EntityKind,
) -> Result<MemoryId, sqlx::Error> {
    let (owner_kind, owner_principal_id) = owner_parts(owner);
    let memory_id = Uuid::now_v7();
    let schema_id = match kind {
        EntityKind::Perspective => TEST_PERSPECTIVE_SCHEMA,
        _ => TEST_ABSTRACTION_SCHEMA,
    };
    let operator_kind = match kind {
        EntityKind::Perspective => MemoryOperatorKind::AtoP,
        _ => MemoryOperatorKind::FtoA,
    };
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_principal_kind, owner_principal_id,
             schema_id, schema_version, kind, text, operator_kind, model_id,
             prompt_version, personality_instance_id)
         VALUES ($1, $2, $3, $4, 1, $5, 'derived', $6, 'test-model',
                 'v1', $7)",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(schema_id)
    .bind(kind)
    .bind(operator_kind)
    .bind(Uuid::now_v7())
    .execute(pg.pool())
    .await?;
    Ok(MemoryId::new(memory_id))
}

#[allow(clippy::too_many_arguments)]
async fn insert_memory_edge(
    pg: &PgStorage,
    owner: &Owner,
    relation_class: RelationClass,
    source_kind: EntityKind,
    source_memory_id: MemoryId,
    target_kind: EntityKind,
    target_memory_id: MemoryId,
) -> Result<(), sqlx::Error> {
    let (owner_kind, owner_principal_id) = owner_parts(owner);
    sqlx::query(
        "INSERT INTO proxima_core.edges
            (edge_id, relation, relation_class,
             source_kind, source_memory_id, source_goal_id,
             target_kind, target_memory_id, target_goal_id,
             authorship_kind, authorship_owner_memory_id,
             owner_principal_kind, owner_principal_id)
         VALUES ($1, 'test/relation', $2,
                 $3, $4, NULL,
                 $5, $6, NULL,
                 'Engine', NULL,
                 $7, $8)",
    )
    .bind(Uuid::now_v7())
    .bind(relation_class)
    .bind(source_kind)
    .bind(source_memory_id.into_inner())
    .bind(target_kind)
    .bind(target_memory_id.into_inner())
    .bind(owner_kind)
    .bind(owner_principal_id)
    .execute(pg.pool())
    .await?;
    Ok(())
}

#[tokio::test]
async fn trigger_rejects_upward_edges_and_semantic_fact_to_fact() {
    let (pg, db_name) = crate::common::fresh_pg().await;

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        apply_test_schemas(pg.pool()).await?;

        let owner = crate::common::owner_fixture();
        let fact_a = ingest_test_fact(&pg, &owner, "a").await;
        let fact_b = ingest_test_fact(&pg, &owner, "b").await;
        let perspective = insert_derived_memory(&pg, &owner, EntityKind::Perspective).await?;

        let err = insert_memory_edge(
            &pg,
            &owner,
            RelationClass::Provenance,
            EntityKind::Fact,
            fact_a,
            EntityKind::Perspective,
            perspective,
        )
        .await
        .expect_err("Fact -> Perspective must be rejected");
        assert!(err.to_string().contains("layer violation"));

        let err = insert_memory_edge(
            &pg,
            &owner,
            RelationClass::Causal,
            EntityKind::Fact,
            fact_a,
            EntityKind::Fact,
            fact_b,
        )
        .await
        .expect_err("Causal Fact -> Fact must be rejected");
        assert!(err.to_string().contains("semantic Fact-to-Fact"));

        insert_memory_edge(
            &pg,
            &owner,
            RelationClass::Structural,
            EntityKind::Fact,
            fact_a,
            EntityKind::Fact,
            fact_b,
        )
        .await?;
        Ok(())
    }
    .await;

    let _ = crate::common::drop_db(&db_name).await;
    result.expect("edge invariant trigger rejects invalid edge shapes");
}

#[tokio::test]
async fn trigger_rejects_endpoint_kind_and_owner_mismatch() {
    let (pg, db_name) = crate::common::fresh_pg().await;

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        apply_test_schemas(pg.pool()).await?;

        let owner = crate::common::owner_fixture();
        let other = other_owner();
        let fact = ingest_test_fact(&pg, &owner, "a").await;
        let other_fact = ingest_test_fact(&pg, &other, "b").await;

        let err = insert_memory_edge(
            &pg,
            &owner,
            RelationClass::Structural,
            EntityKind::Abstraction,
            fact,
            EntityKind::Fact,
            fact,
        )
        .await
        .expect_err("stored Fact endpoint cannot be declared Abstraction");
        assert!(err.to_string().contains("source kind"));

        let err = insert_memory_edge(
            &pg,
            &other,
            RelationClass::Structural,
            EntityKind::Fact,
            fact,
            EntityKind::Fact,
            other_fact,
        )
        .await
        .expect_err("cross-owner source stamp must be rejected");
        assert!(err.to_string().contains("Owner boundary"));
        Ok(())
    }
    .await;

    let _ = crate::common::drop_db(&db_name).await;
    result.expect("edge invariant trigger rejects endpoint lies and cross-owner edges");
}

#[tokio::test]
async fn trigger_allows_cross_domain_fact_set_abstraction() {
    let (pg, db_name) = crate::common::fresh_pg().await;

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        apply_test_schemas(pg.pool()).await?;

        let owner = crate::common::owner_fixture();
        let fact_a = ingest_test_fact(&pg, &owner, "a").await;
        let fact_b = ingest_other_fact(&pg, &owner, "b").await;
        let abstraction = insert_derived_memory(&pg, &owner, EntityKind::Abstraction).await?;

        insert_memory_edge(
            &pg,
            &owner,
            RelationClass::Provenance,
            EntityKind::Abstraction,
            abstraction,
            EntityKind::Fact,
            fact_a,
        )
        .await?;
        insert_memory_edge(
            &pg,
            &owner,
            RelationClass::Provenance,
            EntityKind::Abstraction,
            abstraction,
            EntityKind::Fact,
            fact_b,
        )
        .await?;
        Ok(())
    }
    .await;

    let _ = crate::common::drop_db(&db_name).await;
    result.expect("cross-domain facts may be connected through an Abstraction");
}

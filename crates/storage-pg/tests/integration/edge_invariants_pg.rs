use proxima_core::{
    EntityKind, MemoryId, MemoryOperatorKind, Owner, OwnerRef, RelationClass, UserId,
};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

const TEST_ABSTRACTION_SCHEMA: &str = "test/edge-invariant-abstraction-v1";
const TEST_PERSPECTIVE_SCHEMA: &str = "test/edge-invariant-perspective-v1";

fn other_owner() -> Owner {
    OwnerRef::Personal(UserId::new(Uuid::now_v7()))
}

async fn ingest_test_fact(pg: &PgStorage, owner: &Owner, text: &str) -> MemoryId {
    crate::common::seed_memory(pg, owner, EntityKind::Fact, text)
        .await
        .expect("seed fact")
}

async fn ingest_other_fact(pg: &PgStorage, owner: &Owner, text: &str) -> MemoryId {
    crate::common::seed_memory(pg, owner, EntityKind::Fact, text)
        .await
        .expect("seed other fact")
}

async fn insert_derived_memory(
    pg: &PgStorage,
    owner: &Owner,
    kind: EntityKind,
) -> Result<MemoryId, sqlx::Error> {
    let memory_id = Uuid::now_v7();
    let schema_id = match kind {
        EntityKind::Perspective => TEST_PERSPECTIVE_SCHEMA,
        _ => TEST_ABSTRACTION_SCHEMA,
    };
    let operator_kind = match kind {
        EntityKind::Perspective => MemoryOperatorKind::AtoP,
        _ => MemoryOperatorKind::FtoA,
    };
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(owner);
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_kind, owner_id, schema_id, schema_version, kind, text,
             operator_kind, model_id, prompt_version)
         VALUES ($1, $2, $3, $4, 1, $5, 'derived', $6, 'test-model',
                 'v1')",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(schema_id)
    .bind(kind)
    .bind(operator_kind)
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
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(owner);
    sqlx::query(
        "INSERT INTO proxima_core.edges
            (edge_id, owner_kind, owner_id, relation, relation_class,
             source_kind, source_memory_id, source_goal_id,
             target_kind, target_memory_id, target_goal_id,
             authorship_kind, authorship_owner_memory_id)
         VALUES ($1, $2, $3, 'test/relation', $4,
                 $5, $6, NULL,
                 $7, $8, NULL,
                 'Engine', NULL)",
    )
    .bind(Uuid::now_v7())
    .bind(owner_kind)
    .bind(owner_id)
    .bind(relation_class)
    .bind(source_kind)
    .bind(source_memory_id.into_inner())
    .bind(target_kind)
    .bind(target_memory_id.into_inner())
    .execute(pg.pool())
    .await?;
    Ok(())
}

#[tokio::test]
async fn trigger_rejects_upward_edges_and_semantic_fact_to_fact() {
    let (pg, db_name) = crate::common::fresh_pg().await;

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
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
async fn trigger_rejects_endpoint_kind_and_allows_cross_owner_edges() {
    let (pg, db_name) = crate::common::fresh_pg().await;

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
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

        insert_memory_edge(
            &pg,
            &owner,
            RelationClass::Structural,
            EntityKind::Fact,
            fact,
            EntityKind::Fact,
            other_fact,
        )
        .await?;
        Ok(())
    }
    .await;

    let _ = crate::common::drop_db(&db_name).await;
    result.expect("edge invariant trigger rejects endpoint lies and allows cross-owner edges");
}

#[tokio::test]
async fn trigger_allows_cross_domain_fact_set_abstraction() {
    let (pg, db_name) = crate::common::fresh_pg().await;

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
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

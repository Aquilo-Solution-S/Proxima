//! Relation required-tag gating across fact, pinned-memory, untagged, and cross-flavor schemas.

use super::{
    CROSS_FLAVOR_ASSIGNED_TO_RELATION, TEST_FOLLOW_ACTOR_RELATION, TEST_FOLLOW_RELATION,
    TEST_PIN_ACTOR_RELATION, append_follow_head_edge, append_follow_head_edge_for_relation,
    append_pinned_edge_for_relation, create_sidecar, cross_actor_fact,
    cross_flavor_registry_for_test, engine_for, fact, fresh_pg_with_cross_flavor_sidecars,
    fresh_pg_with_sidecars, ingest_fact, ingest_fact_payload, memory_fact_entity_id, plain_fact,
    registry_for_test,
};

use crate::common::{drop_db, owner_fixture, owner_write_permit};
use proxima_core::{EdgeAuthorshipKind, EdgeId, StorageError};
use proxima_storage_pg::verbs::edge_write::{CheckedEdgeEndpoint, append_owner_checked_edge};
use uuid::Uuid;

#[tokio::test]
async fn follow_head_required_tag_accepts_matching_fact_schema()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg_with_sidecars().await;

    let result = async {
        pg.run_migrations().await?;
        create_sidecar(&pg).await?;
        let owner = owner_fixture();
        let registry = registry_for_test();
        let engine = engine_for(&pg, registry.clone());

        let source = ingest_fact(&pg, &engine, &owner, &fact("source", "v1", "Present")).await?;
        let target = ingest_fact(&pg, &engine, &owner, &fact("target", "v1", "Present")).await?;
        let source_entity = memory_fact_entity_id(&pg, source.memory_id).await?;
        let target_entity = memory_fact_entity_id(&pg, target.memory_id).await?;
        let edge_id = append_follow_head_edge_for_relation(
            &pg,
            &registry,
            &owner,
            TEST_FOLLOW_ACTOR_RELATION,
            source_entity,
            target_entity,
        )
        .await?;

        let relation: (String,) =
            sqlx::query_as("SELECT relation FROM proxima_core.edges WHERE edge_id = $1")
                .bind(edge_id)
                .fetch_one(pg.pool_for_tests())
                .await?;
        assert_eq!(relation.0, TEST_FOLLOW_ACTOR_RELATION);
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn required_tag_rejects_endpoint_schema_missing_tag() -> Result<(), Box<dyn std::error::Error>>
{
    let (pg, db_name) = fresh_pg_with_sidecars().await;

    let result = async {
        pg.run_migrations().await?;
        create_sidecar(&pg).await?;
        let owner = owner_fixture();
        let registry = registry_for_test();
        let engine = engine_for(&pg, registry.clone());

        let source = ingest_fact(&pg, &engine, &owner, &fact("source", "v1", "Present")).await?;
        let target =
            ingest_fact_payload(&pg, &engine, &owner, &plain_fact("target", "v1", "Present"))
                .await?;
        let source_entity = memory_fact_entity_id(&pg, source.memory_id).await?;
        let target_entity = memory_fact_entity_id(&pg, target.memory_id).await?;
        let relation = registry
            .resolve_relation(TEST_FOLLOW_ACTOR_RELATION)
            .expect("tagged follow relation");

        let permit = owner_write_permit(&owner, proxima_core::AccessKind::Fact).await?;
        let mut tx = pg.pool_for_tests().begin().await?;
        let err = append_owner_checked_edge(
            &mut tx,
            &permit,
            EdgeId::new(Uuid::now_v7()),
            relation,
            CheckedEdgeEndpoint::fact_entity(proxima_core::FactEntityId::new(source_entity)),
            CheckedEdgeEndpoint::fact_entity(proxima_core::FactEntityId::new(target_entity)),
            EdgeAuthorshipKind::ExternalAgent,
            None,
        )
        .await
        .expect_err("target schema lacks required actor tag");
        let StorageError::ConstraintViolation(err_msg) = err else {
            panic!("unexpected error: {err}");
        };
        assert!(
            err_msg.contains("endpoint missing required capability tag")
                && err_msg.contains("target")
                && err_msg.contains("actor"),
            "unexpected error: {err_msg}",
        );
        tx.rollback().await?;
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn pinned_memory_required_tag_accepts_matching_schema()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg_with_sidecars().await;

    let result = async {
        pg.run_migrations().await?;
        create_sidecar(&pg).await?;
        let owner = owner_fixture();
        let registry = registry_for_test();
        let engine = engine_for(&pg, registry.clone());

        let source = ingest_fact(&pg, &engine, &owner, &fact("source", "v1", "Present")).await?;
        let target = ingest_fact(&pg, &engine, &owner, &fact("target", "v1", "Present")).await?;
        let edge_id = append_pinned_edge_for_relation(
            &pg,
            &registry,
            &owner,
            TEST_PIN_ACTOR_RELATION,
            source.memory_id.into_inner(),
            target.memory_id.into_inner(),
        )
        .await?;

        let raw: (Option<Uuid>, Option<Uuid>) = sqlx::query_as(
            "SELECT source_memory_id, target_memory_id
               FROM proxima_core.edges
              WHERE edge_id = $1",
        )
        .bind(edge_id)
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(
            raw,
            (
                Some(source.memory_id.into_inner()),
                Some(target.memory_id.into_inner())
            )
        );
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn untagged_relation_edge_append_unchanged() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg_with_sidecars().await;

    let result = async {
        pg.run_migrations().await?;
        create_sidecar(&pg).await?;
        let owner = owner_fixture();
        let registry = registry_for_test();
        let engine = engine_for(&pg, registry.clone());
        let source =
            ingest_fact_payload(&pg, &engine, &owner, &plain_fact("source", "v1", "Present"))
                .await?;
        let target =
            ingest_fact_payload(&pg, &engine, &owner, &plain_fact("target", "v1", "Present"))
                .await?;
        let source_entity = memory_fact_entity_id(&pg, source.memory_id).await?;
        let target_entity = memory_fact_entity_id(&pg, target.memory_id).await?;
        let relation = registry
            .resolve_relation(TEST_FOLLOW_RELATION)
            .expect("untagged follow relation");
        assert!(relation.descriptor.source_required_tags.is_empty());
        assert!(relation.descriptor.target_required_tags.is_empty());

        let edge_id =
            append_follow_head_edge(&pg, &registry, &owner, source_entity, target_entity).await?;
        let relation: (String,) =
            sqlx::query_as("SELECT relation FROM proxima_core.edges WHERE edge_id = $1")
                .bind(edge_id)
                .fetch_one(pg.pool_for_tests())
                .await?;
        assert_eq!(relation.0, TEST_FOLLOW_RELATION);
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn cross_flavor_relation_accepts_schema_tagged_by_other_flavor()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg_with_cross_flavor_sidecars().await;

    let result = async {
        pg.run_migrations().await?;
        create_sidecar(&pg).await?;
        let owner = owner_fixture();
        let registry = cross_flavor_registry_for_test();
        let engine = engine_for(&pg, registry.clone());

        let source = ingest_fact_payload(
            &pg,
            &engine,
            &owner,
            &cross_actor_fact("source", "v1", "Present"),
        )
        .await?;
        let target = ingest_fact_payload(
            &pg,
            &engine,
            &owner,
            &cross_actor_fact("target", "v1", "Present"),
        )
        .await?;
        let source_entity = memory_fact_entity_id(&pg, source.memory_id).await?;
        let target_entity = memory_fact_entity_id(&pg, target.memory_id).await?;
        let edge_id = append_follow_head_edge_for_relation(
            &pg,
            &registry,
            &owner,
            CROSS_FLAVOR_ASSIGNED_TO_RELATION,
            source_entity,
            target_entity,
        )
        .await?;

        let row: (String,) =
            sqlx::query_as("SELECT relation FROM proxima_core.edges WHERE edge_id = $1")
                .bind(edge_id)
                .fetch_one(pg.pool_for_tests())
                .await?;
        assert_eq!(row.0, CROSS_FLAVOR_ASSIGNED_TO_RELATION);
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

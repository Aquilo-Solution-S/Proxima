//! Endpoint binding guards, change-history preservation, and memory-endpoint round trips.

use super::{
    TEST_FOLLOW_RELATION, TEST_PIN_RELATION, append_follow_head_edge, append_pinned_edge,
    create_sidecar, engine_for, fact, fresh_pg_with_sidecars, ingest_fact, memory_fact_entity_id,
    raw_insert_follow_edge, registry_for_test,
};

use std::sync::Arc;

use crate::common::{drop_db, owner_fixture, owner_write_permit};
use proxima_core::mcp::core_tools::list_change_events::{ListChangeEventsArgs, list_change_events};
use proxima_core::mcp::{McpAuthorContext, McpToolCtx, McpToolExtensions};
use proxima_core::storage_ports::*;
use proxima_core::verbs::change_history::ChangeHistoryRequest;
use proxima_core::verbs::query::QueryRequest;
use proxima_core::{
    AuthPath, AuthzContext, ChangeEventKind, EdgeAuthorshipKind, EdgeId, EdgeTargetProjection,
    EntityRef, OwnerRef, StorageError, UserId,
};
use proxima_storage_pg::verbs::edge_write::{CheckedEdgeEndpoint, append_owner_checked_edge};
use uuid::Uuid;

#[tokio::test]
async fn endpoint_guards_reject_binding_mismatch_and_invalid_fact_entities()
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

        let follow_relation = registry
            .resolve_relation(TEST_FOLLOW_RELATION)
            .expect("follow relation");
        let permit = owner_write_permit(&owner, proxima_core::AccessKind::Fact).await?;
        let mut tx = pg.pool_for_tests().begin().await?;
        let err = append_owner_checked_edge(
            &mut tx,
            &permit,
            EdgeId::new(Uuid::now_v7()),
            follow_relation,
            CheckedEdgeEndpoint::fact(source.memory_id),
            CheckedEdgeEndpoint::fact(target.memory_id),
            EdgeAuthorshipKind::ExternalAgent,
            None,
        )
        .await
        .expect_err("FollowHead relation must reject pinned endpoints");
        assert!(matches!(err, StorageError::ConstraintViolation(_)));

        let pin_relation = registry
            .resolve_relation(TEST_PIN_RELATION)
            .expect("pin relation");
        let err = append_owner_checked_edge(
            &mut tx,
            &permit,
            EdgeId::new(Uuid::now_v7()),
            pin_relation,
            CheckedEdgeEndpoint::fact_entity(proxima_core::FactEntityId::new(source_entity)),
            CheckedEdgeEndpoint::fact_entity(proxima_core::FactEntityId::new(target_entity)),
            EdgeAuthorshipKind::ExternalAgent,
            None,
        )
        .await
        .expect_err("Pin relation must reject fact-entity endpoints");
        assert!(matches!(err, StorageError::ConstraintViolation(_)));

        let (owner_kind, owner_id) = owner.columns();
        let err = sqlx::query(
            "INSERT INTO proxima_core.edges
                (edge_id, relation, relation_class, owner_kind, owner_id,
                 source_kind, source_memory_id, source_fact_entity_id,
                 target_kind, target_fact_entity_id, authorship_kind)
             VALUES ($1, $2, 'Structural', $3, $4,
                     'Fact', $5, $6, 'Fact', $7, 'ExternalAgent')",
        )
        .bind(Uuid::now_v7())
        .bind(TEST_FOLLOW_RELATION)
        .bind(owner_kind)
        .bind(owner_id)
        .bind(source.memory_id.into_inner())
        .bind(source_entity)
        .bind(target_entity)
        .execute(pg.pool_for_tests())
        .await
        .expect_err("SQL exactly-one guard rejects malformed endpoint");
        assert!(err.to_string().contains("edges_source_endpoint_chk"));
        tx.rollback().await?;

        let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(&owner);
        let err = sqlx::query(
            "INSERT INTO proxima_core.edges
                (edge_id, owner_kind, owner_id, relation, relation_class,
                 source_kind, source_memory_id, source_fact_entity_id,
                 target_kind, target_fact_entity_id,
                 authorship_kind, authorship_owner_memory_id)
             VALUES ($1, $2, $3, 'test/bad-three-way', 'Structural',
                     'Fact', $4, $5,
                     'Fact', $6,
                     'ExternalAgent', NULL)",
        )
        .bind(Uuid::now_v7())
        .bind(owner_kind)
        .bind(owner_id)
        .bind(source.memory_id.into_inner())
        .bind(source_entity)
        .bind(target_entity)
        .execute(pg.pool_for_tests())
        .await
        .expect_err("SQL exactly-one CHECK rejects memory + fact entity");
        assert!(err.to_string().contains("edges_source_endpoint_chk"));

        let err = sqlx::query(
            "INSERT INTO proxima_core.edges
                (edge_id, owner_kind, owner_id, relation, relation_class,
                 source_kind, source_fact_entity_id,
                 target_kind, target_fact_entity_id,
                 authorship_kind, authorship_owner_memory_id)
             VALUES ($1, $2, $3, 'test/bad-kind', 'Structural',
                     'Abstraction', $4,
                     'Fact', $5,
                     'ExternalAgent', NULL)",
        )
        .bind(Uuid::now_v7())
        .bind(owner_kind)
        .bind(owner_id)
        .bind(source_entity)
        .bind(target_entity)
        .execute(pg.pool_for_tests())
        .await
        .expect_err("fact_entity endpoint must declare Fact kind");
        assert!(
            err.to_string().contains("edges_source_endpoint_chk")
                || err.to_string().contains("source kind Abstraction")
        );

        let err = raw_insert_follow_edge(&pg, &owner, Uuid::now_v7(), target_entity)
            .await
            .expect_err("missing fact entity endpoint must be rejected");
        assert!(
            err.to_string().contains("endpoint not found")
                || err
                    .as_database_error()
                    .is_some_and(sqlx::error::DatabaseError::is_foreign_key_violation)
        );

        let other = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let other_source =
            ingest_fact(&pg, &engine, &other, &fact("other", "v1", "Present")).await?;
        let other_entity = memory_fact_entity_id(&pg, other_source.memory_id).await?;
        raw_insert_follow_edge(&pg, &owner, source_entity, other_entity).await?;

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn change_history_and_list_change_events_preserve_fact_entity_endpoints()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg_with_sidecars().await;

    let result = async {
        pg.run_migrations().await?;
        create_sidecar(&pg).await?;
        let owner = owner_fixture();
        let registry = registry_for_test();
        let engine = Arc::new(engine_for(&pg, registry.clone()));
        let source = ingest_fact(&pg, &engine, &owner, &fact("source", "v1", "Present")).await?;
        let target = ingest_fact(&pg, &engine, &owner, &fact("target", "v1", "Present")).await?;
        let source_entity = memory_fact_entity_id(&pg, source.memory_id).await?;
        let target_entity = memory_fact_entity_id(&pg, target.memory_id).await?;
        let edge_id =
            append_follow_head_edge(&pg, &registry, &owner, source_entity, target_entity).await?;

        let history = pg
            .change_history(
                std::slice::from_ref(&owner),
                &ChangeHistoryRequest {
                    owner,
                    limit: 100,
                    before: None,
                },
            )
            .await?;
        let history_edge = history
            .events
            .iter()
            .find(|event| {
                matches!(
                    event.kind,
                    ChangeEventKind::EdgeAppend { edge_id: seen, .. } if seen == edge_id
                )
            })
            .expect("change_history fact-entity edge");
        assert!(matches!(
            history_edge.kind,
            ChangeEventKind::EdgeAppend {
                source: EntityRef::FactEntity(_),
                target: EdgeTargetProjection::Visible {
                    target: EntityRef::FactEntity(_),
                },
                ..
            }
        ));

        let ctx = McpToolCtx {
            owner,
            authz: AuthzContext::single_owner(&owner, AuthPath::HostBearer),
            registry: Arc::new(registry),
            author: McpAuthorContext {
                model_id: "test".into(),
                client_name: "test".into(),
                client_version: "1".into(),
                caller_self_perspective: None,
            },
            caller_self_perspective: None,
            extensions: McpToolExtensions::with(pg.pool_for_tests().clone()),
            engine: Some(engine),
        };
        let listed = list_change_events(
            ctx,
            ListChangeEventsArgs {
                since: None,
                limit: Some(100),
            },
        )
        .await?;
        let edge_id_text = format!("E:{edge_id}");
        let item = listed
            .events
            .iter()
            .find(|event| event.edge.as_deref() == Some(edge_id_text.as_str()))
            .expect("list_change_events fact-entity edge");
        let source_text = format!("fact_entity:{source_entity}");
        let target_text = format!("fact_entity:{target_entity}");
        assert_eq!(item.source.as_deref(), Some(source_text.as_str()));
        assert_eq!(item.target.as_deref(), Some(target_text.as_str()));
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn pin_relations_still_round_trip_memory_endpoints() -> Result<(), Box<dyn std::error::Error>>
{
    let (pg, db_name) = fresh_pg_with_sidecars().await;

    let result = async {
        pg.run_migrations().await?;
        create_sidecar(&pg).await?;
        let owner = owner_fixture();
        let registry = registry_for_test();
        let engine = engine_for(&pg, registry.clone());
        let source = ingest_fact(&pg, &engine, &owner, &fact("source", "v1", "Present")).await?;
        let target = ingest_fact(&pg, &engine, &owner, &fact("target", "v1", "Present")).await?;
        let edge_id = append_pinned_edge(
            &pg,
            &registry,
            &owner,
            source.memory_id.into_inner(),
            target.memory_id.into_inner(),
        )
        .await?;

        let raw: (Option<Uuid>, Option<Uuid>, Option<Uuid>, Option<Uuid>) = sqlx::query_as(
            "SELECT source_memory_id, source_fact_entity_id,
                    target_memory_id, target_fact_entity_id
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
                None,
                Some(target.memory_id.into_inner()),
                None
            )
        );

        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        let mut req = QueryRequest::for_owner(owner);
        req.limit = 100;
        let response = engine.query(&authz, &req).await?;
        let edge = response
            .edges
            .iter()
            .find(|edge| edge.id == edge_id)
            .expect("pin edge remains queryable");
        assert_eq!(edge.source, EntityRef::Memory(source.memory_id));
        assert_eq!(
            edge.target,
            EdgeTargetProjection::Visible {
                target: EntityRef::Memory(target.memory_id),
            }
        );
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

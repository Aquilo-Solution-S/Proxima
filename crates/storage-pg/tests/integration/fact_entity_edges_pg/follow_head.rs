//! Follow-head edges: log writes, graph resolution to the latest head, and tombstoned-head visibility.

use super::{
    append_follow_head_edge, create_sidecar, engine_for, fact, fresh_pg_with_sidecars, ingest_fact,
    memory_fact_entity_id, registry_for_test,
};

use crate::common::{drop_db, owner_fixture};
use proxima_core::storage_ports::*;
use proxima_core::verbs::query::{QueryRequest, TombstoneFilter};
use proxima_core::{AuthPath, AuthzContext, ChangeEventKind, EdgeTargetProjection, EntityRef};
use uuid::Uuid;

#[tokio::test]
async fn follow_head_edge_writes_log_and_graph_resolves_to_latest_head()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg_with_sidecars().await;

    let result = async {
        pg.run_migrations().await?;
        create_sidecar(&pg).await?;
        let owner = owner_fixture();
        let registry = registry_for_test();
        let engine = engine_for(&pg, registry.clone());

        let source_v1 = ingest_fact(&pg, &engine, &owner, &fact("source", "v1", "Present")).await?;
        let target_v1 = ingest_fact(&pg, &engine, &owner, &fact("target", "v1", "Present")).await?;
        let source_entity = memory_fact_entity_id(&pg, source_v1.memory_id).await?;
        let target_entity = memory_fact_entity_id(&pg, target_v1.memory_id).await?;
        let edge_id =
            append_follow_head_edge(&pg, &registry, &owner, source_entity, target_entity).await?;

        let raw: (Option<Uuid>, Option<Uuid>, Option<Uuid>, Option<Uuid>) = sqlx::query_as(
            "SELECT source_memory_id, source_fact_entity_id,
                    target_memory_id, target_fact_entity_id
               FROM proxima_core.edges
              WHERE edge_id = $1",
        )
        .bind(edge_id)
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(raw, (None, Some(source_entity), None, Some(target_entity)));

        let events = pg
            .list_change_events_after(std::slice::from_ref(&owner), Uuid::nil(), 100)
            .await?;
        let edge_event = events
            .iter()
            .find_map(|row| match &row.event.kind {
                ChangeEventKind::EdgeAppend {
                    edge_id: seen,
                    source,
                    target,
                    ..
                } if *seen == edge_id => Some((*source, *target)),
                _ => None,
            })
            .expect("fact-entity EdgeAppend is decoded");
        assert_eq!(
            edge_event.0,
            EntityRef::FactEntity(proxima_core::FactEntityId::new(source_entity))
        );
        assert_eq!(
            edge_event.1,
            EdgeTargetProjection::Visible {
                target: EntityRef::FactEntity(proxima_core::FactEntityId::new(target_entity)),
            }
        );

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let source_v2 = ingest_fact(&pg, &engine, &owner, &fact("source", "v2", "Present")).await?;

        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        let mut req = QueryRequest::for_owner(owner);
        req.limit = 100;
        let response = engine.query(&authz, &req).await?;
        let edge = response
            .edges
            .iter()
            .find(|edge| edge.id == edge_id)
            .expect("follow-head graph edge");
        assert_eq!(
            edge.source,
            EntityRef::Memory(source_v2.memory_id),
            "graph rows resolve follow-head endpoints to the current memory"
        );
        assert_eq!(
            edge.target,
            EdgeTargetProjection::Visible {
                target: EntityRef::Memory(target_v1.memory_id),
            }
        );
        assert!(!matches!(edge.source, EntityRef::FactEntity(_)));
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn follow_head_tombstoned_head_uses_existing_visibility()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg_with_sidecars().await;

    let result = async {
        pg.run_migrations().await?;
        create_sidecar(&pg).await?;
        let owner = owner_fixture();
        let registry = registry_for_test();
        let engine = engine_for(&pg, registry.clone());
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);

        let source = ingest_fact(&pg, &engine, &owner, &fact("source", "v1", "Present")).await?;
        let target_v1 = ingest_fact(&pg, &engine, &owner, &fact("target", "v1", "Present")).await?;
        let source_entity = memory_fact_entity_id(&pg, source.memory_id).await?;
        let target_entity = memory_fact_entity_id(&pg, target_v1.memory_id).await?;
        let edge_id =
            append_follow_head_edge(&pg, &registry, &owner, source_entity, target_entity).await?;

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let target_tombstone =
            ingest_fact(&pg, &engine, &owner, &fact("target", "deleted", "Deleted")).await?;

        let mut req = QueryRequest::for_owner(owner);
        req.limit = 100;
        let response = engine.query(&authz, &req).await?;
        assert!(
            response.edges.iter().all(|edge| edge.id != edge_id),
            "PresentOnly hides edges whose resolved head is a sidecar tombstone"
        );

        req.tombstones = TombstoneFilter::IncludeTombstoned;
        let response = engine.query(&authz, &req).await?;
        let edge = response
            .edges
            .iter()
            .find(|edge| edge.id == edge_id)
            .expect("IncludeTombstoned shows follow-head edge");
        assert_eq!(edge.source, EntityRef::Memory(source.memory_id));
        assert_eq!(
            edge.target,
            EdgeTargetProjection::Visible {
                target: EntityRef::Memory(target_tombstone.memory_id),
            }
        );
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

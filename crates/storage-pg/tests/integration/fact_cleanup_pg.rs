use std::sync::Arc;

use crate::common::{drop_db, fresh_pg, owner_fixture};

use proxima_core::engine::Engine;
use proxima_core::verbs::event_ingest::{
    Citation, CitationMappingHint, CitedObjectHint, EventDraft,
};
use proxima_core::verbs::query::{
    MemoryLineageDirection, MemoryLineageRequest, MemoryStore, QueryRequest,
};
use proxima_core::verbs::schema::{FlavorRegistryFrozen, PayloadKind, SchemaInfo};
use proxima_core::{
    AuthPath, AuthzContext, MemoryId, Owner, OwnerPrincipalKind, Principal, SchemaId,
    SchemaVersion, SourceBatchId, SourceId, Storage,
};
use uuid::Uuid;

fn schemas_for_test() -> Vec<SchemaInfo> {
    vec![
        SchemaInfo::opaque(
            SchemaId::new("test/cleanup-fact-v1".into()),
            SchemaVersion::new(1),
            PayloadKind::Fact,
        ),
        SchemaInfo::opaque(
            SchemaId::new("test/cleanup-cited-v1".into()),
            SchemaVersion::new(1),
            PayloadKind::CitedObject,
        ),
        SchemaInfo::opaque(
            SchemaId::new("test/cleanup-citation-v1".into()),
            SchemaVersion::new(1),
            PayloadKind::CitationMapping,
        ),
        SchemaInfo::opaque(
            SchemaId::new("test/cleanup-abstraction-v1".into()),
            SchemaVersion::new(1),
            PayloadKind::Abstraction,
        ),
    ]
}

fn fresh_draft(owner: Owner) -> EventDraft {
    let now = time::OffsetDateTime::now_utc();
    EventDraft {
        source_id: SourceId::new("test/cleanup-source"),
        source_batch_id: SourceBatchId::new(Uuid::now_v7()),
        principal: owner.principal,
        org_id: Some(owner.org_id),
        schema_id: SchemaId::new("test/cleanup-fact-v1".into()),
        schema_version: SchemaVersion::new(1),
        payload: format!("cleanup {}", Uuid::now_v7()).into_bytes(),
        observed_at: now,
        occurred_at: now,
        citation: Some(Citation {
            object: CitedObjectHint {
                schema_id: SchemaId::new("test/cleanup-cited-v1".into()),
                schema_version: SchemaVersion::new(1),
                content_hash: [9; 32],
            },
            mapping: CitationMappingHint {
                schema_id: SchemaId::new("test/cleanup-citation-v1".into()),
                schema_version: SchemaVersion::new(1),
            },
        }),
    }
}

#[tokio::test]
async fn cleanup_due_facts_erases_fact_and_tombstones_direct_derivative()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = fresh_pg().await else {
        return Ok(());
    };
    pg.run_migrations().await?;

    let owner = owner_fixture();
    let authz = AuthzContext::single_owner(&owner, AuthPath::System);
    let registry = FlavorRegistryFrozen::with_schemas(schemas_for_test());
    let storage: Arc<dyn Storage> = Arc::new(pg.clone());
    let engine = Engine::new(registry, MemoryStore::new()).with_storage(storage);

    let ingest = engine
        .event_ingest(&authz, fresh_draft(owner.clone()))
        .await?;
    let fact_id = ingest.memory_id.into_inner();
    let event_id = ingest.event_id.into_inner().to_vec();
    let citation_mapping_id: Uuid = sqlx::query_scalar(
        "SELECT citation_mapping_id
           FROM proxima_core.memories
          WHERE memory_id = $1",
    )
    .bind(fact_id)
    .fetch_one(pg.pool())
    .await?;

    sqlx::query(
        "UPDATE proxima_core.memories
            SET created_at = now() - INTERVAL '2 days'
          WHERE memory_id = $1",
    )
    .bind(fact_id)
    .execute(pg.pool())
    .await?;

    let derivative_id = insert_direct_derivative(&pg, &owner, fact_id).await?;

    engine.set_fact_retention(&authz, &owner, 60).await?;
    let cleanup = engine.cleanup_due_facts(&authz, &owner).await?;
    assert_eq!(cleanup.facts_erased, 1);
    assert_eq!(cleanup.derivatives_tombstoned, 1);

    assert_fact_erased(&pg, fact_id, &event_id, citation_mapping_id).await?;
    assert_derivative_tombstoned(&pg, derivative_id).await?;
    assert_entity_delete_emitted(&pg, fact_id).await?;
    assert_entity_delete_emitted(&pg, derivative_id).await?;
    assert_tombstoned_derivative_filtered(&pg, &engine, &authz, &owner, derivative_id).await?;

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

async fn assert_fact_erased(
    pg: &proxima_storage_pg::PgStorage,
    fact_id: Uuid,
    event_id: &[u8],
    citation_mapping_id: Uuid,
) -> Result<(), Box<dyn std::error::Error>> {
    let fact_count: i64 = sqlx::query_scalar(
        "SELECT count(*)::bigint FROM proxima_core.memories WHERE memory_id = $1",
    )
    .bind(fact_id)
    .fetch_one(pg.pool())
    .await?;
    assert_eq!(fact_count, 0);

    let event_count: i64 =
        sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.events WHERE event_id = $1")
            .bind(event_id)
            .fetch_one(pg.pool())
            .await?;
    assert_eq!(event_count, 0);

    let citation_count: i64 = sqlx::query_scalar(
        "SELECT count(*)::bigint
           FROM proxima_core.citation_mappings
          WHERE citation_mapping_id = $1",
    )
    .bind(citation_mapping_id)
    .fetch_one(pg.pool())
    .await?;
    assert_eq!(citation_count, 0);
    Ok(())
}

async fn assert_derivative_tombstoned(
    pg: &proxima_storage_pg::PgStorage,
    derivative_id: Uuid,
) -> Result<(), Box<dyn std::error::Error>> {
    let tombstoned_at: Option<time::OffsetDateTime> = sqlx::query_scalar(
        "SELECT tombstoned_at
           FROM proxima_core.memories
          WHERE memory_id = $1",
    )
    .bind(derivative_id)
    .fetch_one(pg.pool())
    .await?;
    assert!(tombstoned_at.is_some());
    Ok(())
}

async fn assert_entity_delete_emitted(
    pg: &proxima_storage_pg::PgStorage,
    memory_id: Uuid,
) -> Result<(), Box<dyn std::error::Error>> {
    let delete_events: i64 = sqlx::query_scalar(
        "SELECT count(*)::bigint
           FROM proxima_core.change_event
          WHERE kind = 'EntityDelete'
            AND entity_memory_id = $1",
    )
    .bind(memory_id)
    .fetch_one(pg.pool())
    .await?;
    assert_eq!(delete_events, 1);
    Ok(())
}

async fn assert_tombstoned_derivative_filtered(
    pg: &proxima_storage_pg::PgStorage,
    engine: &Engine,
    authz: &AuthzContext,
    owner: &Owner,
    derivative_id: Uuid,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut req = QueryRequest::for_principal(owner.principal.clone());
    req.memory_ids = vec![MemoryId::new(derivative_id)];
    let query = engine.query(authz, &req).await?;
    assert!(query.memories.is_empty());

    let lineage = pg
        .walk_memory_lineage(&MemoryLineageRequest {
            principal: owner.principal.clone(),
            start_memory_id: MemoryId::new(derivative_id),
            direction: MemoryLineageDirection::Ancestors,
            depth: 2,
            limit: 10,
            reader_personality_instance_id: None,
        })
        .await?;
    assert!(lineage.nodes.is_empty());
    Ok(())
}

async fn insert_direct_derivative(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    fact_id: Uuid,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let derivative_id = Uuid::now_v7();
    let edge_id = Uuid::now_v7();
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
         VALUES ($1, $2, $3, $4, 'test/cleanup-abstraction-v1', 1,
                 'Abstraction', 'direct derivative', 'FtoA', 'test-model',
                 'test-prompt', '00000000-0000-0000-0000-000000000000'::uuid, 0)",
    )
    .bind(derivative_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner.org_id.into_inner())
    .execute(pg.pool())
    .await?;

    sqlx::query(
        "INSERT INTO proxima_core.edges
            (edge_id, relation, relation_class,
             source_kind, source_memory_id, source_goal_id,
             target_kind, target_memory_id, target_goal_id,
             authorship_kind, authorship_owner_memory_id,
             owner_principal_kind, owner_principal_id, owner_org_id)
         VALUES ($1, 'core/derived-from', 'Provenance',
                 'Abstraction', $2, NULL,
                 'Fact', $3, NULL,
                 'OperatorFtoA', $2,
                 $4, $5, $6)",
    )
    .bind(edge_id)
    .bind(derivative_id)
    .bind(fact_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner.org_id.into_inner())
    .execute(pg.pool())
    .await?;

    Ok(derivative_id)
}

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
    AuthPath, AuthzContext, EntityKind, MemoryId, Owner, OwnerPrincipalKind, Principal, SchemaId,
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

fn schemas_for_uploaded_blob_gc_test() -> Vec<SchemaInfo> {
    let mut schemas = schemas_for_test();
    let cited_object = schemas
        .iter_mut()
        .find(|schema| schema.kind == PayloadKind::CitedObject)
        .expect("test registry has a CitedObject schema");
    cited_object.sidecar_table = Some("proxima_core.cited_uploaded_blob_v1".into());
    schemas
}

fn fresh_draft(owner: Owner) -> EventDraft {
    fresh_draft_with_content_hash(owner, [9; 32])
}

fn fresh_draft_with_content_hash(owner: Owner, content_hash: [u8; 32]) -> EventDraft {
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
                content_hash,
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

#[tokio::test]
async fn cleanup_due_facts_tombstones_transitive_derivatives()
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
    let citation_mapping_id = citation_mapping_id_for_memory(&pg, fact_id).await?;
    age_fact(&pg, fact_id).await?;

    let first_derivative_id = insert_direct_derivative(&pg, &owner, fact_id).await?;
    let second_derivative_id = insert_derivative(
        &pg,
        &owner,
        first_derivative_id,
        EntityKind::Abstraction,
        "transitive derivative",
    )
    .await?;

    engine.set_fact_retention(&authz, &owner, 60).await?;
    let cleanup = engine.cleanup_due_facts(&authz, &owner).await?;
    assert_eq!(cleanup.facts_erased, 1);
    assert_eq!(cleanup.derivatives_tombstoned, 2);

    assert_fact_erased(&pg, fact_id, &event_id, citation_mapping_id).await?;
    assert_derivative_tombstoned(&pg, first_derivative_id).await?;
    assert_derivative_tombstoned(&pg, second_derivative_id).await?;
    assert_entity_delete_emitted(&pg, first_derivative_id).await?;
    assert_entity_delete_emitted(&pg, second_derivative_id).await?;

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn cleanup_due_facts_aggressively_tombstones_multi_support_derivative()
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

    let due_fact = engine
        .event_ingest(
            &authz,
            fresh_draft_with_content_hash(owner.clone(), [1; 32]),
        )
        .await?;
    let surviving_fact = engine
        .event_ingest(
            &authz,
            fresh_draft_with_content_hash(owner.clone(), [2; 32]),
        )
        .await?;
    let due_fact_id = due_fact.memory_id.into_inner();
    let surviving_fact_id = surviving_fact.memory_id.into_inner();
    age_fact(&pg, due_fact_id).await?;

    let derivative_id = insert_direct_derivative(&pg, &owner, due_fact_id).await?;
    insert_provenance_edge(
        &pg,
        &owner,
        derivative_id,
        surviving_fact_id,
        EntityKind::Fact,
    )
    .await?;

    engine.set_fact_retention(&authz, &owner, 60).await?;
    let cleanup = engine.cleanup_due_facts(&authz, &owner).await?;
    assert_eq!(cleanup.facts_erased, 1);
    assert_eq!(cleanup.derivatives_tombstoned, 1);

    assert_derivative_tombstoned(&pg, derivative_id).await?;
    assert_memory_exists(&pg, surviving_fact_id).await?;
    assert_entity_delete_emitted(&pg, derivative_id).await?;

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn cleanup_due_facts_garbage_collects_cited_objects_by_reference_count()
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

    let first = engine
        .event_ingest(&authz, fresh_draft(owner.clone()))
        .await?;
    let second = engine
        .event_ingest(&authz, fresh_draft(owner.clone()))
        .await?;
    let first_fact_id = first.memory_id.into_inner();
    let second_fact_id = second.memory_id.into_inner();
    let cited_object_id = cited_object_id_for_memory(&pg, first_fact_id).await?;
    assert_eq!(
        cited_object_id_for_memory(&pg, second_fact_id).await?,
        cited_object_id
    );

    age_fact(&pg, first_fact_id).await?;
    engine.set_fact_retention(&authz, &owner, 60).await?;
    let first_cleanup = engine.cleanup_due_facts(&authz, &owner).await?;
    assert_eq!(first_cleanup.facts_erased, 1);
    assert_eq!(first_cleanup.cited_objects_erased, 0);
    assert_cited_object_exists(&pg, cited_object_id).await?;

    age_fact(&pg, second_fact_id).await?;
    let second_cleanup = engine.cleanup_due_facts(&authz, &owner).await?;
    assert_eq!(second_cleanup.facts_erased, 1);
    assert_eq!(second_cleanup.cited_objects_erased, 1);
    assert_cited_object_erased(&pg, cited_object_id).await?;

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn cleanup_due_facts_deletes_cited_object_sidecars_and_surfaces_s3_refs()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = fresh_pg().await else {
        return Ok(());
    };
    pg.run_migrations().await?;

    let owner = owner_fixture();
    let authz = AuthzContext::single_owner(&owner, AuthPath::System);
    let registry = FlavorRegistryFrozen::with_schemas(schemas_for_uploaded_blob_gc_test());
    let storage: Arc<dyn Storage> = Arc::new(pg.clone());
    let engine = Engine::new(registry, MemoryStore::new()).with_storage(storage);

    let ingest = engine
        .event_ingest(&authz, fresh_draft(owner.clone()))
        .await?;
    let fact_id = ingest.memory_id.into_inner();
    let cited_object_id = cited_object_id_for_memory(&pg, fact_id).await?;
    insert_uploaded_blob_sidecar(&pg, cited_object_id).await?;
    age_fact(&pg, fact_id).await?;

    engine.set_fact_retention(&authz, &owner, 60).await?;
    let cleanup = engine.cleanup_due_facts(&authz, &owner).await?;
    assert_eq!(cleanup.facts_erased, 1);
    assert_eq!(cleanup.cited_objects_erased, 1);
    assert_eq!(
        cleanup.orphaned_s3_blobs,
        vec![proxima_core::verbs::fact_cleanup::OrphanedS3Blob {
            bucket: "b".into(),
            object_key: "k".into(),
        }]
    );

    assert_uploaded_blob_sidecar_erased(&pg, cited_object_id).await?;
    assert_cited_object_erased(&pg, cited_object_id).await?;

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

async fn assert_memory_exists(
    pg: &proxima_storage_pg::PgStorage,
    memory_id: Uuid,
) -> Result<(), Box<dyn std::error::Error>> {
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*)::bigint
           FROM proxima_core.memories
          WHERE memory_id = $1",
    )
    .bind(memory_id)
    .fetch_one(pg.pool())
    .await?;
    assert_eq!(count, 1);
    Ok(())
}

async fn assert_cited_object_exists(
    pg: &proxima_storage_pg::PgStorage,
    cited_object_id: Uuid,
) -> Result<(), Box<dyn std::error::Error>> {
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*)::bigint
           FROM proxima_core.cited_objects
          WHERE cited_object_id = $1",
    )
    .bind(cited_object_id)
    .fetch_one(pg.pool())
    .await?;
    assert_eq!(count, 1);
    Ok(())
}

async fn assert_cited_object_erased(
    pg: &proxima_storage_pg::PgStorage,
    cited_object_id: Uuid,
) -> Result<(), Box<dyn std::error::Error>> {
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*)::bigint
           FROM proxima_core.cited_objects
          WHERE cited_object_id = $1",
    )
    .bind(cited_object_id)
    .fetch_one(pg.pool())
    .await?;
    assert_eq!(count, 0);
    Ok(())
}

async fn assert_uploaded_blob_sidecar_erased(
    pg: &proxima_storage_pg::PgStorage,
    cited_object_id: Uuid,
) -> Result<(), Box<dyn std::error::Error>> {
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*)::bigint
           FROM proxima_core.cited_uploaded_blob_v1
          WHERE cited_object_id = $1",
    )
    .bind(cited_object_id)
    .fetch_one(pg.pool())
    .await?;
    assert_eq!(count, 0);
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

async fn age_fact(
    pg: &proxima_storage_pg::PgStorage,
    fact_id: Uuid,
) -> Result<(), Box<dyn std::error::Error>> {
    sqlx::query(
        "UPDATE proxima_core.memories
            SET created_at = now() - INTERVAL '2 days'
          WHERE memory_id = $1",
    )
    .bind(fact_id)
    .execute(pg.pool())
    .await?;
    Ok(())
}

async fn citation_mapping_id_for_memory(
    pg: &proxima_storage_pg::PgStorage,
    memory_id: Uuid,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let citation_mapping_id = sqlx::query_scalar(
        "SELECT citation_mapping_id
           FROM proxima_core.memories
          WHERE memory_id = $1",
    )
    .bind(memory_id)
    .fetch_one(pg.pool())
    .await?;
    Ok(citation_mapping_id)
}

async fn cited_object_id_for_memory(
    pg: &proxima_storage_pg::PgStorage,
    memory_id: Uuid,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let cited_object_id = sqlx::query_scalar(
        "SELECT cited_object_id
           FROM proxima_core.citation_mappings
          WHERE memory_id = $1",
    )
    .bind(memory_id)
    .fetch_one(pg.pool())
    .await?;
    Ok(cited_object_id)
}

async fn insert_uploaded_blob_sidecar(
    pg: &proxima_storage_pg::PgStorage,
    cited_object_id: Uuid,
) -> Result<(), Box<dyn std::error::Error>> {
    sqlx::query(
        "INSERT INTO proxima_core.cited_uploaded_blob_v1
            (cited_object_id, bucket, object_key, sha256, byte_len, mime, filename, etag)
         VALUES ($1, 'b', 'k', $2, 1, 'application/octet-stream', 'blob.bin', NULL)",
    )
    .bind(cited_object_id)
    .bind([7_u8; 32].as_slice())
    .execute(pg.pool())
    .await?;
    Ok(())
}

async fn insert_direct_derivative(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    fact_id: Uuid,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    insert_derivative(pg, owner, fact_id, EntityKind::Fact, "direct derivative").await
}

async fn insert_derivative(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    origin_id: Uuid,
    origin_kind: EntityKind,
    text: &str,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let derivative_id = Uuid::now_v7();
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
                 'Abstraction', $5, 'FtoA', 'test-model',
                 'test-prompt', '00000000-0000-0000-0000-000000000000'::uuid, 0)",
    )
    .bind(derivative_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner.org_id.into_inner())
    .bind(text)
    .execute(pg.pool())
    .await?;

    insert_provenance_edge(pg, owner, derivative_id, origin_id, origin_kind).await?;

    Ok(derivative_id)
}

async fn insert_provenance_edge(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    derivative_id: Uuid,
    origin_id: Uuid,
    origin_kind: EntityKind,
) -> Result<(), Box<dyn std::error::Error>> {
    let edge_id = Uuid::now_v7();
    let owner_kind = OwnerPrincipalKind::of(&owner.principal);
    let owner_principal_id = match &owner.principal {
        Principal::User(user) => user.into_inner(),
        Principal::Group(group) => group.into_inner(),
    };

    sqlx::query(
        "INSERT INTO proxima_core.edges
            (edge_id, relation, relation_class,
             source_kind, source_memory_id, source_goal_id,
             target_kind, target_memory_id, target_goal_id,
             authorship_kind, authorship_owner_memory_id,
             owner_principal_kind, owner_principal_id, owner_org_id)
         VALUES ($1, 'core/derived-from', 'Provenance',
                 'Abstraction', $2, NULL,
                 $3, $4, NULL,
                 'OperatorFtoA', $2,
                 $5, $6, $7)",
    )
    .bind(edge_id)
    .bind(derivative_id)
    .bind(origin_kind)
    .bind(origin_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner.org_id.into_inner())
    .execute(pg.pool())
    .await?;

    Ok(())
}

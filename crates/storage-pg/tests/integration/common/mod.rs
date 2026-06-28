// Each integration-test binary independently includes this module via
// `mod common;`. Items unused by a particular binary would otherwise trip
// `dead_code` even though another binary uses them.
#![allow(dead_code)]

pub mod personality;

#[path = "../../../src/test_fixtures.rs"]
mod storage_pg_test_fixtures;

pub use proxima_core::test_fixtures::owner_fixture;
use proxima_core::verbs::event_ingest::EventDraft;
use proxima_core::{
    EdgeId, EntityKind, MemoryId, Owner, RelationClass, SchemaId, SchemaVersion, SourceBatchId,
    SourceId, Storage,
};
#[allow(unused_imports)]
pub use proxima_pg_testkit::{
    create_db, create_db_from_template, db_url, drop_db, ensure_template, unique_db_name,
};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

#[allow(unused_imports)]
pub use storage_pg_test_fixtures::core_template_name;

pub async fn fresh_pg() -> (PgStorage, String) {
    storage_pg_test_fixtures::fresh_pg("proxima_test").await
}

pub async fn insert_home(
    pg: &PgStorage,
    entity_id: Uuid,
    owner: &Owner,
) -> Result<(), sqlx::Error> {
    let (owner_kind, owner_principal_id) = owner.columns();
    sqlx::query(proxima_storage_pg::access::owner_ref_compat::sql(
        "INSERT INTO __PROXIMA_ENTITY_OWNER__
            (entity_id, owner_principal_kind, owner_principal_id, is_home, granted_by)
         VALUES ($1, $2, $3, true, $4)
         ON CONFLICT DO NOTHING",
    ))
    .bind(entity_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(Uuid::nil())
    .execute(pg.pool())
    .await
    .map(|_| ())
}

pub async fn share_entity(
    pg: &PgStorage,
    entity_id: Uuid,
    owner: &Owner,
) -> Result<(), sqlx::Error> {
    let (owner_kind, owner_principal_id) = owner.columns();
    sqlx::query(proxima_storage_pg::access::owner_ref_compat::sql(
        "INSERT INTO __PROXIMA_ENTITY_OWNER__
            (entity_id, owner_principal_kind, owner_principal_id, is_home, granted_by)
         VALUES ($1, $2, $3, false, $4)
         ON CONFLICT DO NOTHING",
    ))
    .bind(entity_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(Uuid::nil())
    .execute(pg.pool())
    .await
    .map(|_| ())
}

pub async fn seed_memory(
    pg: &PgStorage,
    owner: &Owner,
    kind: EntityKind,
    text: &str,
) -> Result<MemoryId, Box<dyn std::error::Error>> {
    if matches!(kind, EntityKind::Fact) {
        let now = time::OffsetDateTime::now_utc();
        let draft = EventDraft {
            source_id: SourceId::new("test/edge-access"),
            source_batch_id: SourceBatchId::new(Uuid::now_v7()),
            principal: *owner,
            author_personality_instance_id: None,
            schema_id: SchemaId::new("test/edge-access-fact-v1".into()),
            schema_version: SchemaVersion::new(1),
            payload: text.as_bytes().to_vec(),
            rendered_text: Some(text.to_string()),
            observed_at: now,
            occurred_at: now,
            citation: None,
        };
        let outcome = pg.ingest_event_atomic(&draft, None).await?;
        insert_home(pg, outcome.memory_id.into_inner(), owner).await?;
        return Ok(outcome.memory_id);
    }

    let memory_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, schema_id, schema_version, kind, text, operator_kind, model_id,
             prompt_version, personality_instance_id, wake_chain_depth)
         VALUES ($1, 'test/edge-access-v1', 1, $2, $3, 'Wake',
                 'test-model', 'edge-access-v1',
                 '00000000-0000-0000-0000-000000000000'::uuid, 0)",
    )
    .bind(memory_id)
    .bind(kind)
    .bind(text)
    .execute(pg.pool())
    .await?;
    insert_home(pg, memory_id, owner).await?;
    Ok(MemoryId::new(memory_id))
}

pub async fn seed_memory_edge(
    pg: &PgStorage,
    _owner: &Owner,
    source: (EntityKind, MemoryId),
    target: (EntityKind, MemoryId),
    relation: &str,
    relation_class: RelationClass,
) -> Result<EdgeId, sqlx::Error> {
    let edge_id = Uuid::now_v7();
    let (source_kind, source_memory_id) = source;
    let (target_kind, target_memory_id) = target;
    sqlx::query(
        "INSERT INTO proxima_core.edges
            (edge_id, relation, relation_class,
             source_kind, source_memory_id, source_goal_id,
             target_kind, target_memory_id, target_goal_id,
             authorship_kind, authorship_owner_memory_id)
         VALUES ($1, $2, $3, $4, $5, NULL, $6, $7, NULL,
                 'Engine', NULL)",
    )
    .bind(edge_id)
    .bind(relation)
    .bind(relation_class)
    .bind(source_kind)
    .bind(source_memory_id.into_inner())
    .bind(target_kind)
    .bind(target_memory_id.into_inner())
    .execute(pg.pool())
    .await?;
    Ok(EdgeId::new(edge_id))
}

#![allow(dead_code)]

use proxima_core::FactIngestPort;

// Each integration-test binary independently includes this module via
// `mod common;`. Items unused by a particular binary would otherwise trip
// `dead_code` even though another binary uses them.

#[path = "../../../src/test_fixtures.rs"]
mod storage_pg_test_fixtures;

pub use proxima_core::test_fixtures::owner_fixture;
use proxima_core::verbs::fact_ingest::{FactReceiptDraft, FactWriteCommand};
use proxima_core::{
    EdgeId, EntityKind, MemoryId, Owner, RelationClass, SchemaId, SchemaVersion, SourceBatchId,
    SourceId,
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
    set_owned_row_owner(pg, entity_id, owner).await
}

pub async fn share_entity(
    pg: &PgStorage,
    entity_id: Uuid,
    owner: &Owner,
) -> Result<(), sqlx::Error> {
    set_owned_row_owner(pg, entity_id, owner).await
}

async fn set_owned_row_owner(
    pg: &PgStorage,
    entity_id: Uuid,
    owner: &Owner,
) -> Result<(), sqlx::Error> {
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(owner);
    sqlx::query(
        "UPDATE proxima_core.memories
            SET owner_kind = $2, owner_id = $3
          WHERE memory_id = $1",
    )
    .bind(entity_id)
    .bind(owner_kind)
    .bind(owner_id)
    .execute(pg.pool_for_tests())
    .await?;
    sqlx::query(
        "UPDATE proxima_core.goals
            SET owner_kind = $2, owner_id = $3
          WHERE goal_id = $1",
    )
    .bind(entity_id)
    .bind(owner_kind)
    .bind(owner_id)
    .execute(pg.pool_for_tests())
    .await?;
    Ok(())
}

pub async fn seed_memory(
    pg: &PgStorage,
    owner: &Owner,
    kind: EntityKind,
    text: &str,
) -> Result<MemoryId, Box<dyn std::error::Error>> {
    if matches!(kind, EntityKind::Fact) {
        let now = time::OffsetDateTime::now_utc();
        let draft = FactWriteCommand {
            schema_id: SchemaId::new("test/edge-access-fact-v1".into()),
            schema_version: SchemaVersion::new(1),
            payload: text.as_bytes().to_vec(),
            rendered_text: Some(text.to_string()),
            receipt: Some(FactReceiptDraft {
                source_id: SourceId::new("test/edge-access"),
                source_batch_id: SourceBatchId::new(Uuid::now_v7()),
                observed_at: now,
                occurred_at: now,
            }),
            citation: None,
        };
        let outcome = pg.ingest_fact_atomic(owner, &draft, None).await?;
        return Ok(outcome.memory_id);
    }

    let memory_id = Uuid::now_v7();
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(owner);
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_kind, owner_id, schema_id, schema_version, kind, text,
             operator_kind, operator_id, input_contract_id, source_batch_id, model_id, prompt_version)
         VALUES ($1, $2, $3, 'test/edge-access-v1', 1, $4, $5,
                 CASE WHEN $4 = 'Perspective'::proxima_core.entity_kind
                      THEN 'AtoP'::proxima_core.memory_operator_kind
                      ELSE 'AtoA'::proxima_core.memory_operator_kind END,
                 '00000000-0000-0000-0000-000000000101'::uuid,
                 '00000000-0000-0000-0000-000000000102'::uuid,
                 NULL,
                 'test-model', 'edge-access-v1')",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(kind)
    .bind(text)
    .execute(pg.pool_for_tests())
    .await?;
    Ok(MemoryId::new(memory_id))
}

pub async fn seed_memory_edge(
    pg: &PgStorage,
    owner: &Owner,
    source: (EntityKind, MemoryId),
    target: (EntityKind, MemoryId),
    relation: &str,
    relation_class: RelationClass,
) -> Result<EdgeId, sqlx::Error> {
    let edge_id = Uuid::now_v7();
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(owner);
    let (source_kind, source_memory_id) = source;
    let (target_kind, target_memory_id) = target;
    sqlx::query(
        "INSERT INTO proxima_core.edges
            (edge_id, owner_kind, owner_id, relation, relation_class,
             source_kind, source_memory_id, source_goal_id,
             target_kind, target_memory_id, target_goal_id,
             authorship_kind, authorship_owner_memory_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7, NULL, $8, $9, NULL,
                 'Engine', NULL)",
    )
    .bind(edge_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(relation)
    .bind(relation_class)
    .bind(source_kind)
    .bind(source_memory_id.into_inner())
    .bind(target_kind)
    .bind(target_memory_id.into_inner())
    .execute(pg.pool_for_tests())
    .await?;
    Ok(EdgeId::new(edge_id))
}

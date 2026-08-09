#![allow(dead_code)]

use std::sync::Arc;

use proxima_core::FactIngestPort;

// Each integration-test binary independently includes this module via
// `mod common;`. Items unused by a particular binary would otherwise trip
// `dead_code` even though another binary uses them.

mod registry;
#[path = "../../../src/test_fixtures.rs"]
mod storage_pg_test_fixtures;

use proxima_core::storage_ports::{
    ComplianceAdminPort, OwnerDropProofPort, OwnerWritePermit, StoragePorts,
};
pub use proxima_core::test_fixtures::owner_fixture;
use proxima_core::verbs::fact_ingest::{FactReceiptDraft, FactWriteCommand};
use proxima_core::verbs::schema::FlavorRegistryFrozen;
use proxima_core::{
    AccessKind, AuthPath, AuthzContext, EdgeKind, Engine, EntityKind, FlavorRegistry, MemoryId,
    Owner, OwnerRef, OwnerRefKind, Role, SchemaId, SchemaVersion, SourceBatchId, SourceId, UserId,
};
#[allow(unused_imports)]
pub use proxima_pg_testkit::{
    create_db, create_db_from_template, db_url, drop_db, ensure_template, unique_db_name,
};
use proxima_storage_pg::PgStorage;
pub use registry::{
    compliance_fact_registry, embedding_lifecycle_registry, fact_blob_only_registry,
    fact_blob_registry, query_registry, receiptless_fact_registry, sidecar_fact_registry,
};
use uuid::Uuid;

#[allow(unused_imports)]
pub use storage_pg_test_fixtures::core_template_name;

pub async fn fresh_pg() -> (PgStorage, String) {
    storage_pg_test_fixtures::fresh_pg("proxima_test").await
}

pub fn owner_parts(owner: &Owner) -> (OwnerRefKind, Option<Uuid>) {
    proxima_storage_pg::access::owner_columns::owner_binds(owner)
}

pub fn test_registry() -> FlavorRegistryFrozen {
    FlavorRegistry::new().freeze_or_panic_for_tests()
}

pub fn engine_with_registry(pg: &PgStorage, registry: FlavorRegistryFrozen) -> Engine {
    Engine::new(registry).with_storage_ports(Arc::new(pg.clone()).storage_ports())
}

pub fn storage_ports_with_compliance(
    pg: &PgStorage,
    compliance_admin: Arc<dyn ComplianceAdminPort>,
) -> StoragePorts {
    storage_ports_with_compliance_and_drop_proof(pg, compliance_admin, None)
}

pub fn storage_ports_with_compliance_and_drop_proof(
    pg: &PgStorage,
    compliance_admin: Arc<dyn ComplianceAdminPort>,
    owner_drop_proof: Option<Arc<dyn OwnerDropProofPort>>,
) -> StoragePorts {
    let pg = Arc::new(pg.clone());
    let mut builder = StoragePorts::builder()
        .fact_ingest(pg.clone())
        .mcp_call_write(pg.clone())
        .mcp_call_read(pg.clone())
        .memory_authoring(pg.clone())
        .memory_read(pg.clone())
        .memory_inspect(pg.clone())
        .embedding_text(pg.clone())
        .embedding_write(pg.clone())
        .embedding_job(pg.clone())
        .embedding_maintenance(pg.clone())
        .goal_write(pg.clone())
        .goal_read(pg.clone())
        .goal_wake_candidate(pg.clone())
        .change_event(pg.clone())
        .edge_read(pg.clone())
        .citation(pg.clone())
        .owner_access_read(pg.clone())
        .owner_membership_admin(pg.clone())
        .owner_transfer(pg.clone())
        .source_batch(pg.clone())
        .source_cursor(pg.clone())
        .fact_retention(pg.clone())
        .compliance_erase(pg.clone())
        .compliance_admin(compliance_admin)
        .registry_projection(pg);

    if let Some(owner_drop_proof) = owner_drop_proof {
        builder = builder.owner_drop_proof(owner_drop_proof);
    }

    builder.build()
}

pub async fn owner_write_permit(
    owner: &Owner,
    kind: AccessKind,
) -> Result<OwnerWritePermit, Box<dyn std::error::Error>> {
    let authz = match owner {
        OwnerRef::Personal(user_id) => AuthzContext::for_subject(*user_id, AuthPath::HostBearer),
        OwnerRef::Group(_) => AuthzContext::for_subject_with_role(
            UserId::new(Uuid::now_v7()),
            [(*owner, Role::admin())],
            AuthPath::HostBearer,
        ),
        OwnerRef::World => AuthzContext::denied_for_owner(owner),
    };
    let engine = Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests());
    Ok(engine.authorize_owner_write(&authz, owner, kind).await?)
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
            lexical_language: None,
            receipt: Some(FactReceiptDraft {
                source_id: SourceId::new("test/edge-access"),
                source_batch_id: SourceBatchId::new(Uuid::now_v7()),
                observed_at: now,
                occurred_at: now,
            }),
            citation: None,
            derived_from: Vec::new(),
        };
        let permit = owner_write_permit(owner, AccessKind::Fact).await?;
        let outcome = pg.ingest_fact_atomic(&permit, &draft, None).await?;
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

/// Seed one index row between two memories, straight into the table.
///
/// Production code never writes an edge as a free-standing act, so there is
/// no verb for this and there should not be one: the fixture exists to stand
/// up read-path graphs without also standing up the node writes that would
/// have produced them. `kind` is a fixture parameter for the same reason —
/// the tests need both kinds present, and no production caller may choose.
pub async fn seed_memory_edge(
    pg: &PgStorage,
    owner: &Owner,
    source: (EntityKind, MemoryId),
    target: (EntityKind, MemoryId),
    kind: EdgeKind,
) -> Result<(), sqlx::Error> {
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(owner);
    let (source_kind, source_memory_id) = source;
    let (target_kind, target_memory_id) = target;
    sqlx::query(
        "INSERT INTO proxima_core.edges
            (source_kind, source_id, target_kind, target_id, kind, owner_kind, owner_id)
         VALUES ($1::text::proxima_core.edge_endpoint_kind, $2,
                 $3::text::proxima_core.edge_endpoint_kind, $4,
                 $5::text::proxima_core.edge_kind, $6, $7)
         ON CONFLICT DO NOTHING",
    )
    .bind(source_kind.as_str())
    .bind(source_memory_id.into_inner())
    .bind(target_kind.as_str())
    .bind(target_memory_id.into_inner())
    .bind(kind.as_str())
    .bind(owner_kind)
    .bind(owner_id)
    .execute(pg.pool_for_tests())
    .await?;
    Ok(())
}

/// Count index rows between two memories, in either kind.
pub async fn count_memory_edges(
    pg: &PgStorage,
    source: MemoryId,
    target: MemoryId,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT count(*)::bigint FROM proxima_core.edges
          WHERE source_id = $1 AND target_id = $2",
    )
    .bind(source.into_inner())
    .bind(target.into_inner())
    .fetch_one(pg.pool_for_tests())
    .await
}

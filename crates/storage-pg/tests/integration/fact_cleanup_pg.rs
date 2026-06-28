use std::sync::Arc;

use crate::common::{drop_db, fresh_pg, owner_fixture};

use proxima_core::engine::Engine;
use proxima_core::llm::EMBEDDING_DIM;
use proxima_core::verbs::event_ingest::{
    Citation, CitationMappingHint, CitedObjectHint, EventDraft,
};
use proxima_core::verbs::query::{MemoryLineageDirection, MemoryLineageRequest, QueryRequest};
use proxima_core::verbs::schema::{FlavorRegistryFrozen, PayloadKind, SchemaInfo};
use proxima_core::{
    AuthPath, AuthzContext, CORE_MOTIVATED_BY_RELATION, ChangeEventKind, EntityKind, EntityRef,
    FactPayload, FlavorRegistry, GoalId, MemoryId, Owner, PayloadKeyBuilder, Relation, SchemaId,
    SchemaVersion, SidecarPayload, SourceBatchId, SourceId, Storage, StorageError,
    canonical_json_bytes,
};
use proxima_storage_pg::sidecars::{PgMemoryPayload, PgMemoryPayloadFuture};
use proxima_storage_pg::verbs::event_ingest::{EventIngestSidecarFuture, PgFactSidecar};
use proxima_storage_pg::{
    PgSidecarRegistry, PgSidecarRegistryFrozen, PgStorage, register_core_pg_sidecars,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CleanupStatefulFactV1 {
    entity_key: String,
    body: String,
    state: String,
}

impl FactPayload for CleanupStatefulFactV1 {
    const SCHEMA_ID: &'static str = "test/cleanup-stateful-fact-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn event_key(&self) -> Vec<u8> {
        let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_str("entity_key", &self.entity_key);
        key.field_str("body", &self.body);
        key.field_str("state", &self.state);
        key.finish()
    }

    fn render(&self) -> String {
        format!("{}: {}", self.entity_key, self.body)
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("proxima_test.cleanup_stateful_fact_v1")
    }

    fn natural_key_columns() -> &'static [&'static str] {
        &["entity_key"]
    }
}

impl PgFactSidecar for CleanupStatefulFactV1 {
    fn insert_sidecar<'t>(
        self,
        tx: &'t mut sqlx::Transaction<'_, sqlx::Postgres>,
        memory_id: MemoryId,
    ) -> EventIngestSidecarFuture<'t>
    where
        Self: 't,
    {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO proxima_test.cleanup_stateful_fact_v1
                    (memory_id, entity_key, body, state)
                 VALUES ($1, $2, $3, $4)",
            )
            .bind(memory_id.into_inner())
            .bind(&self.entity_key)
            .bind(&self.body)
            .bind(&self.state)
            .execute(tx.as_mut())
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
            Ok(())
        })
    }
}

impl PgMemoryPayload for CleanupStatefulFactV1 {
    fn load_memory_payload(
        _pool: &sqlx::PgPool,
        _memory_id: MemoryId,
    ) -> PgMemoryPayloadFuture<'_> {
        Box::pin(async { Ok(None) })
    }
}

fn schemas_for_test() -> Vec<SchemaInfo> {
    let mut edge_schema = SchemaInfo::opaque(
        SchemaId::new("test/cleanup-edge-v1".into()),
        SchemaVersion::new(1),
        PayloadKind::Edge,
    );
    edge_schema.sidecar_table = Some("proxima_core.agent_link_v1".into());

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
        edge_schema,
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

fn stateful_registry_for_test() -> FlavorRegistryFrozen {
    let mut registry = FlavorRegistry::new();
    registry.add_fact_schema::<CleanupStatefulFactV1>();
    registry.freeze()
}

fn stateful_pg_sidecars_for_test() -> PgSidecarRegistryFrozen {
    let registry = stateful_registry_for_test();
    let mut sidecars = PgSidecarRegistry::new();
    register_core_pg_sidecars(&mut sidecars);
    sidecars.add_fact::<CleanupStatefulFactV1>();
    sidecars
        .freeze_against(registry.schemas())
        .expect("test PG sidecars match test schemas")
}

async fn fresh_pg_with_stateful_sidecars() -> (PgStorage, String) {
    let (pg, db_name) = fresh_pg().await;
    (pg.with_sidecars(stateful_pg_sidecars_for_test()), db_name)
}

async fn create_stateful_sidecar(pg: &PgStorage) -> Result<(), sqlx::Error> {
    sqlx::query("CREATE SCHEMA proxima_test")
        .execute(pg.pool())
        .await?;
    sqlx::query(
        "CREATE TABLE proxima_test.cleanup_stateful_fact_v1 (
            memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
            entity_key text NOT NULL,
            body text NOT NULL,
            state text NOT NULL
        )",
    )
    .execute(pg.pool())
    .await?;
    Ok(())
}

fn stateful_engine_for(pg: &PgStorage) -> Engine {
    let storage: Arc<dyn Storage> = Arc::new(pg.clone());
    Engine::new(stateful_registry_for_test()).with_storage(storage)
}

fn stateful_fact(entity_key: &str, body: &str) -> CleanupStatefulFactV1 {
    CleanupStatefulFactV1 {
        entity_key: entity_key.to_string(),
        body: body.to_string(),
        state: "Present".to_string(),
    }
}

fn stateful_draft_for(owner: &Owner, payload_value: &Value) -> EventDraft {
    let now = time::OffsetDateTime::now_utc();
    EventDraft {
        source_id: SourceId::new(format!("test/cleanup-stateful/{}", Uuid::now_v7())),
        source_batch_id: SourceBatchId::new(Uuid::now_v7()),
        principal: *owner,
        author_personality_instance_id: None,
        schema_id: CleanupStatefulFactV1::schema_id(),
        schema_version: SchemaVersion::new(CleanupStatefulFactV1::SCHEMA_VERSION),
        payload: canonical_json_bytes(payload_value),
        rendered_text: None,
        observed_at: now,
        occurred_at: now,
        citation: None,
    }
}

async fn ingest_stateful_fact(
    pg: &PgStorage,
    engine: &Engine,
    owner: &Owner,
    payload: &CleanupStatefulFactV1,
) -> Result<proxima_core::EventIngestOutcome, Box<dyn std::error::Error>> {
    let payload_value = serde_json::to_value(payload)?;
    let draft = stateful_draft_for(owner, &payload_value);
    let authz = AuthzContext::single_owner(owner, AuthPath::System);
    let authorized = engine
        .authorize_event_ingest(&authz, Relation::Ingest, draft)
        .await?;
    let sidecar_payload = SidecarPayload::fact(payload.clone());
    Ok(pg
        .ingest_event_with_typed_sidecar(&authorized, &sidecar_payload, None)
        .await?)
}

fn fresh_draft(owner: Owner) -> EventDraft {
    fresh_draft_with_content_hash(owner, [9; 32])
}

fn fresh_draft_with_content_hash(owner: Owner, content_hash: [u8; 32]) -> EventDraft {
    let now = time::OffsetDateTime::now_utc();
    EventDraft {
        source_id: SourceId::new("test/cleanup-source"),
        source_batch_id: SourceBatchId::new(Uuid::now_v7()),
        principal: owner,
        author_personality_instance_id: None,
        schema_id: SchemaId::new("test/cleanup-fact-v1".into()),
        schema_version: SchemaVersion::new(1),
        payload: format!("cleanup {}", Uuid::now_v7()).into_bytes(),
        rendered_text: None,
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
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;

    let owner = owner_fixture();
    let authz = AuthzContext::single_owner(&owner, AuthPath::System);
    let registry = FlavorRegistryFrozen::with_schemas(schemas_for_test());
    let storage: Arc<dyn Storage> = Arc::new(pg.clone());
    let engine = Engine::new(registry).with_storage(storage);

    let ingest = engine.event_ingest(&authz, fresh_draft(owner)).await?;
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
    let provenance_edge_id = edge_id_between(&pg, derivative_id, fact_id).await?;
    insert_agent_link_sidecar(&pg, provenance_edge_id).await?;
    insert_embedding_artifacts(&pg, &owner, EntityKind::Fact, fact_id).await?;
    insert_embedding_artifacts(&pg, &owner, EntityKind::Abstraction, derivative_id).await?;

    engine.set_fact_retention(&authz, &owner, 60).await?;
    let cleanup = engine.cleanup_due_facts(&authz, &owner).await?;
    assert_eq!(cleanup.facts_erased, 1);
    assert_eq!(cleanup.derivatives_tombstoned, 1);

    assert_fact_erased(&pg, fact_id, &event_id, citation_mapping_id).await?;
    assert_derivative_tombstoned(&pg, derivative_id).await?;
    assert_agent_link_sidecar_erased(&pg, provenance_edge_id).await?;
    assert_embedding_artifacts_erased(&pg, fact_id).await?;
    assert_embedding_artifacts_erased(&pg, derivative_id).await?;
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
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;

    let owner = owner_fixture();
    let authz = AuthzContext::single_owner(&owner, AuthPath::System);
    let registry = FlavorRegistryFrozen::with_schemas(schemas_for_test());
    let storage: Arc<dyn Storage> = Arc::new(pg.clone());
    let engine = Engine::new(registry).with_storage(storage);

    let ingest = engine.event_ingest(&authz, fresh_draft(owner)).await?;
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
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;

    let owner = owner_fixture();
    let authz = AuthzContext::single_owner(&owner, AuthPath::System);
    let registry = FlavorRegistryFrozen::with_schemas(schemas_for_test());
    let storage: Arc<dyn Storage> = Arc::new(pg.clone());
    let engine = Engine::new(registry).with_storage(storage);

    let due_fact = engine
        .event_ingest(&authz, fresh_draft_with_content_hash(owner, [1; 32]))
        .await?;
    let surviving_fact = engine
        .event_ingest(&authz, fresh_draft_with_content_hash(owner, [2; 32]))
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
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;

    let owner = owner_fixture();
    let authz = AuthzContext::single_owner(&owner, AuthPath::System);
    let registry = FlavorRegistryFrozen::with_schemas(schemas_for_test());
    let storage: Arc<dyn Storage> = Arc::new(pg.clone());
    let engine = Engine::new(registry).with_storage(storage);

    let first = engine.event_ingest(&authz, fresh_draft(owner)).await?;
    let second = engine.event_ingest(&authz, fresh_draft(owner)).await?;
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
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;

    let owner = owner_fixture();
    let authz = AuthzContext::single_owner(&owner, AuthPath::System);
    let registry = FlavorRegistryFrozen::with_schemas(schemas_for_uploaded_blob_gc_test());
    let storage: Arc<dyn Storage> = Arc::new(pg.clone());
    let engine = Engine::new(registry).with_storage(storage);

    let ingest = engine.event_ingest(&authz, fresh_draft(owner)).await?;
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

#[tokio::test]
async fn tombstone_fact_forgets_uncited_fact() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;

    let owner = owner_fixture();
    let authz = AuthzContext::single_owner(&owner, AuthPath::System);
    let registry = FlavorRegistryFrozen::with_schemas(schemas_for_test());
    let storage: Arc<dyn Storage> = Arc::new(pg.clone());
    let engine = Engine::new(registry).with_storage(storage);

    let mut draft = fresh_draft(owner);
    draft.citation = None;
    let ingest = engine.event_ingest(&authz, draft).await?;
    let fact_id = ingest.memory_id.into_inner();

    let outcome = engine
        .tombstone_fact(&authz, &owner, ingest.memory_id)
        .await?;
    assert!(outcome.fact_erased);
    assert_eq!(outcome.derivatives_tombstoned, 0);
    assert_eq!(outcome.cited_objects_erased, 0);
    assert!(outcome.orphaned_s3_blobs.is_empty());
    assert_memory_erased(&pg, fact_id).await?;

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn tombstone_fact_non_head_version_leaves_head_intact()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg_with_stateful_sidecars().await;
    pg.run_migrations().await?;
    create_stateful_sidecar(&pg).await?;

    let owner = owner_fixture();
    let authz = AuthzContext::single_owner(&owner, AuthPath::System);
    let engine = stateful_engine_for(&pg);

    let first = ingest_stateful_fact(&pg, &engine, &owner, &stateful_fact("file", "v1")).await?;
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    let second = ingest_stateful_fact(&pg, &engine, &owner, &stateful_fact("file", "v2")).await?;
    let fact_entity_id = fact_entity_id_for_memory(&pg, first.memory_id.into_inner()).await?;
    assert_eq!(
        fact_entity_id_for_memory(&pg, second.memory_id.into_inner()).await?,
        fact_entity_id
    );
    assert_eq!(
        current_memory_id(&pg, fact_entity_id).await?,
        second.memory_id.into_inner()
    );

    let outcome = engine
        .tombstone_fact(&authz, &owner, first.memory_id)
        .await?;
    assert!(outcome.fact_erased);
    assert_eq!(outcome.derivatives_tombstoned, 0);
    assert_memory_erased(&pg, first.memory_id.into_inner()).await?;
    assert_stateful_sidecar_erased(&pg, first.memory_id.into_inner()).await?;
    assert_memory_exists(&pg, second.memory_id.into_inner()).await?;
    assert_fact_entity_exists(&pg, fact_entity_id).await?;
    assert_eq!(
        current_memory_id(&pg, fact_entity_id).await?,
        second.memory_id.into_inner()
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn tombstone_fact_keeps_shared_cited_object() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;

    let owner = owner_fixture();
    let authz = AuthzContext::single_owner(&owner, AuthPath::System);
    let registry = FlavorRegistryFrozen::with_schemas(schemas_for_uploaded_blob_gc_test());
    let storage: Arc<dyn Storage> = Arc::new(pg.clone());
    let engine = Engine::new(registry).with_storage(storage);

    let first = engine.event_ingest(&authz, fresh_draft(owner)).await?;
    let second = engine.event_ingest(&authz, fresh_draft(owner)).await?;
    let first_fact_id = first.memory_id.into_inner();
    let second_fact_id = second.memory_id.into_inner();
    let cited_object_id = cited_object_id_for_memory(&pg, first_fact_id).await?;
    assert_eq!(
        cited_object_id_for_memory(&pg, second_fact_id).await?,
        cited_object_id
    );
    insert_uploaded_blob_sidecar(&pg, cited_object_id).await?;

    let first_outcome = engine
        .tombstone_fact(&authz, &owner, first.memory_id)
        .await?;
    assert!(first_outcome.fact_erased);
    assert_eq!(first_outcome.cited_objects_erased, 0);
    assert!(first_outcome.orphaned_s3_blobs.is_empty());
    assert_cited_object_exists(&pg, cited_object_id).await?;

    let second_outcome = engine
        .tombstone_fact(&authz, &owner, second.memory_id)
        .await?;
    assert!(second_outcome.fact_erased);
    assert_eq!(second_outcome.cited_objects_erased, 1);
    assert_eq!(
        second_outcome.orphaned_s3_blobs,
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

#[tokio::test]
async fn tombstone_fact_cascades_to_lineage_children() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;

    let owner = owner_fixture();
    let authz = AuthzContext::single_owner(&owner, AuthPath::System);
    let registry = FlavorRegistryFrozen::with_schemas(schemas_for_test());
    let storage: Arc<dyn Storage> = Arc::new(pg.clone());
    let engine = Engine::new(registry).with_storage(storage);

    let ingest = engine.event_ingest(&authz, fresh_draft(owner)).await?;
    let fact_id = ingest.memory_id.into_inner();
    let derivative_id = insert_direct_derivative(&pg, &owner, fact_id).await?;

    let outcome = engine
        .tombstone_fact(&authz, &owner, ingest.memory_id)
        .await?;
    assert!(outcome.fact_erased);
    assert_eq!(outcome.derivatives_tombstoned, 1);
    assert_derivative_tombstoned(&pg, derivative_id).await?;
    assert_entity_delete_emitted(&pg, derivative_id).await?;
    assert_entity_delete_emitted(&pg, fact_id).await?;

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn tombstone_fact_is_idempotent() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;

    let owner = owner_fixture();
    let authz = AuthzContext::single_owner(&owner, AuthPath::System);
    let registry = FlavorRegistryFrozen::with_schemas(schemas_for_test());
    let storage: Arc<dyn Storage> = Arc::new(pg.clone());
    let engine = Engine::new(registry).with_storage(storage);

    let ingest = engine.event_ingest(&authz, fresh_draft(owner)).await?;

    let first = engine
        .tombstone_fact(&authz, &owner, ingest.memory_id)
        .await?;
    assert!(first.fact_erased);
    let second = engine
        .tombstone_fact(&authz, &owner, ingest.memory_id)
        .await?;
    assert!(!second.fact_erased);
    assert_eq!(second.derivatives_tombstoned, 0);
    assert_eq!(second.cited_objects_erased, 0);
    assert!(second.orphaned_s3_blobs.is_empty());

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn tombstone_fact_drops_goal_evidence_edge() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;

    let owner = owner_fixture();
    let authz = AuthzContext::single_owner(&owner, AuthPath::System);
    let registry = FlavorRegistryFrozen::with_schemas(schemas_for_test());
    let storage: Arc<dyn Storage> = Arc::new(pg.clone());
    let engine = Engine::new(registry).with_storage(storage);

    let ingest = engine.event_ingest(&authz, fresh_draft(owner)).await?;
    let fact_id = ingest.memory_id.into_inner();
    let goal_id = insert_active_goal(&pg, &owner).await?;
    let edge_id = insert_motivated_by_edge(&pg, &owner, goal_id, fact_id).await?;

    let outcome = engine
        .tombstone_fact(&authz, &owner, ingest.memory_id)
        .await?;
    assert!(outcome.fact_erased);

    assert_edge_erased(&pg, edge_id).await?;
    assert_goal_active(&pg, goal_id).await?;
    assert_motivated_by_edge_delete_emitted(&pg, &owner, edge_id, goal_id, fact_id).await?;

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

async fn assert_embedding_artifacts_erased(
    pg: &proxima_storage_pg::PgStorage,
    memory_id: Uuid,
) -> Result<(), Box<dyn std::error::Error>> {
    let embedding_count: i64 = sqlx::query_scalar(
        "SELECT count(*)::bigint
           FROM proxima_core.embeddings
          WHERE entity_id = $1",
    )
    .bind(memory_id)
    .fetch_one(pg.pool())
    .await?;
    assert_eq!(embedding_count, 0);

    let job_count: i64 = sqlx::query_scalar(
        "SELECT count(*)::bigint
           FROM proxima_core.embedding_jobs
          WHERE entity_id = $1",
    )
    .bind(memory_id)
    .fetch_one(pg.pool())
    .await?;
    assert_eq!(job_count, 0);
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

async fn assert_memory_erased(
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
    assert_eq!(count, 0);
    Ok(())
}

async fn assert_fact_entity_exists(
    pg: &proxima_storage_pg::PgStorage,
    fact_entity_id: Uuid,
) -> Result<(), Box<dyn std::error::Error>> {
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*)::bigint
           FROM proxima_core.fact_entities
          WHERE fact_entity_id = $1",
    )
    .bind(fact_entity_id)
    .fetch_one(pg.pool())
    .await?;
    assert_eq!(count, 1);
    Ok(())
}

async fn assert_stateful_sidecar_erased(
    pg: &proxima_storage_pg::PgStorage,
    memory_id: Uuid,
) -> Result<(), Box<dyn std::error::Error>> {
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*)::bigint
           FROM proxima_test.cleanup_stateful_fact_v1
          WHERE memory_id = $1",
    )
    .bind(memory_id)
    .fetch_one(pg.pool())
    .await?;
    assert_eq!(count, 0);
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

async fn fact_entity_id_for_memory(
    pg: &proxima_storage_pg::PgStorage,
    memory_id: Uuid,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let fact_entity_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT fact_entity_id
           FROM proxima_core.memories
          WHERE memory_id = $1",
    )
    .bind(memory_id)
    .fetch_one(pg.pool())
    .await?;
    Ok(fact_entity_id.expect("stateful Fact has fact_entity_id"))
}

async fn current_memory_id(
    pg: &proxima_storage_pg::PgStorage,
    fact_entity_id: Uuid,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    Ok(sqlx::query_scalar(
        "SELECT current_memory_id
           FROM proxima_core.fact_entities
          WHERE fact_entity_id = $1",
    )
    .bind(fact_entity_id)
    .fetch_one(pg.pool())
    .await?)
}

async fn insert_active_goal(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let goal_id = Uuid::now_v7();
    let (owner_kind, owner_principal_id) = owner.columns();
    let request_id = format!("cleanup-goal-{}", Uuid::now_v7());
    sqlx::query(
        "INSERT INTO proxima_core.goals
            (goal_id, schema_id, text, state, authorship_kind, request_id,
             schema_version, payload, title, idempotency_key)
         VALUES ($1, 'core/simple-text-v1',
                 'goal text', 'Active', 'User', $2, 1,
                 $3, 'goal title',
                 md5($4::text || ':' || $5::text || ':' || $2))",
    )
    .bind(goal_id)
    .bind(request_id)
    .bind(b"{}".to_vec())
    .bind(owner_kind)
    .bind(owner_principal_id)
    .execute(pg.pool())
    .await?;
    insert_home(pg, goal_id, owner).await?;
    Ok(goal_id)
}

async fn insert_motivated_by_edge(
    pg: &proxima_storage_pg::PgStorage,
    _owner: &Owner,
    goal_id: Uuid,
    fact_id: Uuid,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let edge_id = Uuid::now_v7();

    sqlx::query(
        "INSERT INTO proxima_core.edges
            (edge_id, relation, relation_class,
             source_kind, source_goal_id,
             target_kind, target_memory_id,
             authorship_kind)
         VALUES ($1, $2, 'Structural',
                 'Goal', $3,
                 'Fact', $4,
                 'User')",
    )
    .bind(edge_id)
    .bind(CORE_MOTIVATED_BY_RELATION)
    .bind(goal_id)
    .bind(fact_id)
    .execute(pg.pool())
    .await?;

    Ok(edge_id)
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

async fn assert_agent_link_sidecar_erased(
    pg: &proxima_storage_pg::PgStorage,
    edge_id: Uuid,
) -> Result<(), Box<dyn std::error::Error>> {
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*)::bigint
           FROM proxima_core.agent_link_v1
          WHERE edge_id = $1",
    )
    .bind(edge_id)
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

async fn assert_motivated_by_edge_delete_emitted(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    edge_id: Uuid,
    goal_id: Uuid,
    fact_id: Uuid,
) -> Result<(), Box<dyn std::error::Error>> {
    let delete_events = pg
        .list_change_events_after(std::slice::from_ref(owner), Uuid::nil(), 100)
        .await?;
    let found = delete_events.iter().any(|event| {
        matches!(
            &event.event.kind,
            ChangeEventKind::EdgeDelete {
                edge_id: seen_edge_id,
                relation,
                source: EntityRef::Goal(seen_goal_id),
                target: EntityRef::Memory(seen_fact_id),
            } if *seen_edge_id == edge_id
                && relation == CORE_MOTIVATED_BY_RELATION
                && *seen_goal_id == GoalId::new(goal_id)
                && *seen_fact_id == MemoryId::new(fact_id)
        )
    });
    assert!(
        found,
        "expected EdgeDelete change_event for motivated-by edge"
    );
    Ok(())
}

async fn assert_edge_erased(
    pg: &proxima_storage_pg::PgStorage,
    edge_id: Uuid,
) -> Result<(), Box<dyn std::error::Error>> {
    let edge_count: i64 = sqlx::query_scalar(
        "SELECT count(*)::bigint
           FROM proxima_core.edges
          WHERE edge_id = $1",
    )
    .bind(edge_id)
    .fetch_one(pg.pool())
    .await?;
    assert_eq!(edge_count, 0);
    Ok(())
}

async fn assert_goal_active(
    pg: &proxima_storage_pg::PgStorage,
    goal_id: Uuid,
) -> Result<(), Box<dyn std::error::Error>> {
    let state: proxima_core::verbs::goal_write::GoalState =
        sqlx::query_scalar("SELECT state FROM proxima_core.goals WHERE goal_id = $1")
            .bind(goal_id)
            .fetch_one(pg.pool())
            .await?;
    assert_eq!(state, proxima_core::verbs::goal_write::GoalState::Active);
    Ok(())
}

async fn assert_tombstoned_derivative_filtered(
    pg: &proxima_storage_pg::PgStorage,
    engine: &Engine,
    authz: &AuthzContext,
    owner: &Owner,
    derivative_id: Uuid,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut req = QueryRequest::for_principal(*owner);
    req.memory_ids = vec![MemoryId::new(derivative_id)];
    let query = engine.query(authz, &req).await?;
    assert!(query.memories.is_empty());

    let lineage = pg
        .walk_memory_lineage(
            std::slice::from_ref(owner),
            &MemoryLineageRequest {
                principal: *owner,
                start_memory_id: MemoryId::new(derivative_id),
                direction: MemoryLineageDirection::Ancestors,
                depth: 2,
                limit: 10,
                reader_personality_instance_id: None,
            },
        )
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

async fn insert_agent_link_sidecar(
    pg: &proxima_storage_pg::PgStorage,
    edge_id: Uuid,
) -> Result<(), Box<dyn std::error::Error>> {
    sqlx::query(
        "INSERT INTO proxima_core.agent_link_v1 (edge_id, reason, confidence)
         VALUES ($1, 'cleanup test edge sidecar', 100)",
    )
    .bind(edge_id)
    .execute(pg.pool())
    .await?;
    Ok(())
}

async fn edge_id_between(
    pg: &proxima_storage_pg::PgStorage,
    source_memory_id: Uuid,
    target_memory_id: Uuid,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let edge_id = sqlx::query_scalar(
        "SELECT edge_id
           FROM proxima_core.edges
          WHERE source_memory_id = $1
            AND target_memory_id = $2",
    )
    .bind(source_memory_id)
    .bind(target_memory_id)
    .fetch_one(pg.pool())
    .await?;
    Ok(edge_id)
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

    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, schema_id, schema_version, kind, text, operator_kind, model_id,
             prompt_version, personality_instance_id, wake_chain_depth)
         VALUES ($1, 'test/cleanup-abstraction-v1', 1,
                 'Abstraction', $2, 'FtoA', 'test-model',
                 'test-prompt', '00000000-0000-0000-0000-000000000000'::uuid, 0)",
    )
    .bind(derivative_id)
    .bind(text)
    .execute(pg.pool())
    .await?;
    insert_home(pg, derivative_id, owner).await?;

    insert_provenance_edge(pg, owner, derivative_id, origin_id, origin_kind).await?;

    Ok(derivative_id)
}

async fn insert_embedding_artifacts(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    kind: EntityKind,
    memory_id: Uuid,
) -> Result<(), Box<dyn std::error::Error>> {
    let (owner_kind, owner_principal_id) = owner.columns();
    sqlx::query(
        "INSERT INTO proxima_core.embeddings
            (entity_kind, entity_id, embedding_version, model_id, vec,
             owner_principal_kind, owner_principal_id)
         VALUES ($1, $2, 1, 'cleanup-embed', $3::vector, $4, $5)",
    )
    .bind(kind)
    .bind(memory_id)
    .bind(zero_vector_literal())
    .bind(owner_kind)
    .bind(owner_principal_id)
    .execute(pg.pool())
    .await?;

    sqlx::query(
        "INSERT INTO proxima_core.embedding_jobs
            (owner_principal_kind, owner_principal_id,
             entity_kind, entity_id, model_id)
         VALUES ($1, $2, $3, $4, 'cleanup-embed')",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(kind)
    .bind(memory_id)
    .execute(pg.pool())
    .await?;
    Ok(())
}

fn zero_vector_literal() -> String {
    let mut out = String::with_capacity(EMBEDDING_DIM.saturating_mul(2).saturating_add(2));
    out.push('[');
    for idx in 0..EMBEDDING_DIM {
        if idx > 0 {
            out.push(',');
        }
        out.push('0');
    }
    out.push(']');
    out
}

async fn insert_provenance_edge(
    pg: &proxima_storage_pg::PgStorage,
    _owner: &Owner,
    derivative_id: Uuid,
    origin_id: Uuid,
    origin_kind: EntityKind,
) -> Result<(), Box<dyn std::error::Error>> {
    let edge_id = Uuid::now_v7();

    sqlx::query(
        "INSERT INTO proxima_core.edges
            (edge_id, relation, relation_class,
             source_kind, source_memory_id, source_goal_id,
             target_kind, target_memory_id, target_goal_id,
             authorship_kind, authorship_owner_memory_id)
         VALUES ($1, 'core/derived-from', 'Provenance',
                 'Abstraction', $2, NULL,
                 $3, $4, NULL,
                 'OperatorFtoA', $2)",
    )
    .bind(edge_id)
    .bind(derivative_id)
    .bind(origin_kind)
    .bind(origin_id)
    .execute(pg.pool())
    .await?;

    Ok(())
}

async fn insert_home(
    pg: &proxima_storage_pg::PgStorage,
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

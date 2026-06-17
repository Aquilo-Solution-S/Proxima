//! Task 4 fact-entity citation helper coverage.

use std::sync::Arc;

use crate::common::{drop_db, fresh_pg, owner_fixture};
use proxima_core::engine::Engine;
use proxima_core::storage::Storage;
use proxima_core::verbs::event_ingest::{
    Citation, CitationMappingHint, CitedObjectHint, EventDraft,
};
use proxima_core::verbs::schema::PayloadKind;
use proxima_core::{
    AuthPath, AuthzContext, FactEntityId, FactPayload, FlavorRegistry, FlavorRegistryFrozen, Owner,
    Role, SchemaId, SchemaVersion, SourceBatchId, SourceId, StorageError, canonical_json_bytes,
};
use proxima_storage_pg::PgStorage;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

const CITED_OBJECT_SCHEMA: &str = "test/entity-head-cited-object-v1";
const CITATION_MAPPING_SCHEMA: &str = "test/entity-head-citation-mapping-v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct StatefulFactV1 {
    entity_key: String,
    body: String,
}

impl FactPayload for StatefulFactV1 {
    const SCHEMA_ID: &'static str = "test/entity-head-cited-fact-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn render(&self) -> String {
        format!("{}: {}", self.entity_key, self.body)
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("proxima_test.entity_head_cited_fact_v1")
    }

    fn natural_key_columns() -> &'static [&'static str] {
        &["entity_key"]
    }
}

fn registry_for_test() -> FlavorRegistryFrozen {
    let mut registry = FlavorRegistry::new();
    registry.add_fact_schema::<StatefulFactV1>();
    registry.add_opaque_schema(
        SchemaId::new(CITED_OBJECT_SCHEMA.into()),
        SchemaVersion::new(1),
        PayloadKind::CitedObject,
    );
    registry.add_opaque_schema(
        SchemaId::new(CITATION_MAPPING_SCHEMA.into()),
        SchemaVersion::new(1),
        PayloadKind::CitationMapping,
    );
    registry.freeze()
}

async fn create_sidecar(pg: &PgStorage) -> Result<(), sqlx::Error> {
    sqlx::query("CREATE SCHEMA proxima_test")
        .execute(pg.pool())
        .await?;
    sqlx::query(
        "CREATE TABLE proxima_test.entity_head_cited_fact_v1 (
            memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
            entity_key text NOT NULL,
            body text NOT NULL
        )",
    )
    .execute(pg.pool())
    .await?;
    Ok(())
}

fn engine_for(pg: &PgStorage) -> Engine {
    let storage: Arc<dyn Storage> = Arc::new(pg.clone());
    Engine::new(registry_for_test()).with_storage(storage)
}

fn fact(entity_key: &str, body: &str) -> StatefulFactV1 {
    StatefulFactV1 {
        entity_key: entity_key.to_string(),
        body: body.to_string(),
    }
}

fn draft_for(owner: &Owner, payload_value: &Value) -> EventDraft {
    let now = time::OffsetDateTime::now_utc();
    EventDraft {
        source_id: SourceId::new(format!("test/fact-entity-citation/{}", Uuid::now_v7())),
        source_batch_id: SourceBatchId::new(Uuid::now_v7()),
        principal: owner.principal.clone(),
        org_id: Some(owner.org_id),
        author_personality_instance_id: None,
        schema_id: StatefulFactV1::schema_id(),
        schema_version: SchemaVersion::new(StatefulFactV1::SCHEMA_VERSION),
        payload: canonical_json_bytes(payload_value),
        rendered_text: None,
        observed_at: now,
        occurred_at: now,
        citation: Some(Citation {
            object: CitedObjectHint {
                schema_id: SchemaId::new(CITED_OBJECT_SCHEMA.into()),
                schema_version: SchemaVersion::new(1),
                content_hash: *blake3::hash(
                    format!(
                        "{}:{}",
                        payload_value["entity_key"].as_str().unwrap_or_default(),
                        payload_value["body"].as_str().unwrap_or_default()
                    )
                    .as_bytes(),
                )
                .as_bytes(),
            },
            mapping: CitationMappingHint {
                schema_id: SchemaId::new(CITATION_MAPPING_SCHEMA.into()),
                schema_version: SchemaVersion::new(1),
            },
        }),
    }
}

async fn ingest_fact(
    pg: &PgStorage,
    engine: &Engine,
    owner: &Owner,
    payload: &StatefulFactV1,
) -> Result<proxima_core::EventIngestOutcome, StorageError> {
    let payload_value =
        serde_json::to_value(payload).map_err(|err| StorageError::Internal(err.to_string()))?;
    let draft = draft_for(owner, &payload_value);
    let authz = AuthzContext::single_owner(owner, AuthPath::System);
    let authorized = engine
        .authorize_event_ingest(&authz, Role::SourceIngest, draft)
        .map_err(|err| StorageError::Internal(err.to_string()))?;
    pg.ingest_event_with_sidecar(
        &authorized,
        StatefulFactV1::sidecar_table().expect("test sidecar"),
        &payload_value,
        None,
    )
    .await
}

async fn memory_fact_entity_id(pg: &PgStorage, memory_id: Uuid) -> Result<Uuid, sqlx::Error> {
    let id: Option<Uuid> = sqlx::query_scalar(
        "SELECT fact_entity_id
           FROM proxima_core.memories
          WHERE memory_id = $1",
    )
    .bind(memory_id)
    .fetch_one(pg.pool())
    .await?;
    Ok(id.expect("stateful Fact has fact_entity_id"))
}

#[tokio::test]
async fn citation_of_entity_head_follows_updates_while_fact_citation_pins()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;

    let result = async {
        pg.run_migrations().await?;
        create_sidecar(&pg).await?;
        let owner = owner_fixture();
        let engine = engine_for(&pg);

        let v1 = ingest_fact(&pg, &engine, &owner, &fact("entity", "v1")).await?;
        let fact_entity_id = memory_fact_entity_id(&pg, v1.memory_id.into_inner()).await?;
        let v1_citation = pg
            .citation_of_fact(&owner, v1.memory_id)
            .await?
            .expect("v1 citation");

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let v2 = ingest_fact(&pg, &engine, &owner, &fact("entity", "v2")).await?;
        let v2_citation = pg
            .citation_of_fact(&owner, v2.memory_id)
            .await?
            .expect("v2 citation");

        let pinned_v1 = pg
            .citation_of_fact(&owner, v1.memory_id)
            .await?
            .expect("v1 citation remains pinned");
        assert_eq!(
            pinned_v1.citation_mapping_id,
            v1_citation.citation_mapping_id
        );

        let head = pg
            .citation_of_entity_head(&owner, FactEntityId::new(fact_entity_id))
            .await?
            .expect("entity head citation");
        assert_eq!(head.citation_mapping_id, v2_citation.citation_mapping_id);
        assert_ne!(head.citation_mapping_id, v1_citation.citation_mapping_id);
        assert_eq!(head.cited_object_id, v2_citation.cited_object_id);

        let has_fact_entity_column: bool = sqlx::query_scalar(
            "SELECT EXISTS (
                SELECT 1
                  FROM information_schema.columns
                 WHERE table_schema = 'proxima_core'
                   AND table_name = 'citation_mappings'
                   AND column_name = 'fact_entity_id'
            )",
        )
        .fetch_one(pg.pool())
        .await?;
        assert!(
            !has_fact_entity_column,
            "citation_mappings storage stays memory-pinned"
        );
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

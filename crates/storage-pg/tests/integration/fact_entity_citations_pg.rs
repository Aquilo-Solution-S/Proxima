//! Fact-entity citation helper coverage.

use std::sync::Arc;

use crate::common::{drop_db, fresh_pg, owner_fixture};
use proxima_core::engine::Engine;
use proxima_core::storage_ports::*;
use proxima_core::verbs::fact_ingest::{
    Citation, CitationMappingHint, CitedObjectHint, FactReceiptDraft, FactWriteCommand,
};
use proxima_core::verbs::schema::PayloadKind;
use proxima_core::{
    AuthPath, AuthzContext, FactEntityId, FactPayload, FlavorRegistry, FlavorRegistryFrozen,
    MemoryId, Owner, PayloadKeyBuilder, Relation, SchemaId, SchemaVersion, SidecarPayload,
    SourceBatchId, SourceId, StorageError, canonical_json_bytes,
};
use proxima_storage_pg::sidecars::{PgMemoryPayload, PgMemoryPayloadFuture};
use proxima_storage_pg::verbs::fact_ingest::{FactIngestSidecarFuture, PgFactSidecar};
use proxima_storage_pg::{
    PgSidecarRegistry, PgSidecarRegistryFrozen, PgStorage, register_core_pg_sidecars,
};
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

    fn receipt_key(&self) -> Vec<u8> {
        let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_str("entity_key", &self.entity_key);
        key.field_str("body", &self.body);
        key.finish()
    }

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

impl PgFactSidecar for StatefulFactV1 {
    fn insert_sidecar<'t>(
        self,
        tx: &'t mut sqlx::Transaction<'_, sqlx::Postgres>,
        memory_id: MemoryId,
    ) -> FactIngestSidecarFuture<'t>
    where
        Self: 't,
    {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO proxima_test.entity_head_cited_fact_v1
                    (memory_id, entity_key, body)
                 VALUES ($1, $2, $3)",
            )
            .bind(memory_id.into_inner())
            .bind(&self.entity_key)
            .bind(&self.body)
            .execute(tx.as_mut())
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
            Ok(())
        })
    }
}

impl PgMemoryPayload for StatefulFactV1 {
    fn load_memory_payload(
        _ctx: proxima_storage_pg::sidecars::PgSidecarReadCtx<'_>,
        _memory_id: MemoryId,
    ) -> PgMemoryPayloadFuture<'_> {
        Box::pin(async { Ok(None) })
    }
}

fn registry_for_test() -> FlavorRegistryFrozen {
    let mut registry = FlavorRegistry::new();
    registry.add_fact_schema_or_panic_for_tests::<StatefulFactV1>();
    registry.add_opaque_schema_or_panic_for_tests(
        SchemaId::new(CITED_OBJECT_SCHEMA.into()),
        SchemaVersion::new(1),
        PayloadKind::CitedObject,
    );
    registry.add_opaque_schema_or_panic_for_tests(
        SchemaId::new(CITATION_MAPPING_SCHEMA.into()),
        SchemaVersion::new(1),
        PayloadKind::CitationMapping,
    );
    registry.freeze_or_panic_for_tests()
}

fn pg_sidecars_for_test() -> PgSidecarRegistryFrozen {
    let registry = registry_for_test();
    let mut sidecars = PgSidecarRegistry::new();
    register_core_pg_sidecars(&mut sidecars);
    sidecars.add_fact::<StatefulFactV1>();
    sidecars
        .freeze_against(registry.schemas())
        .expect("test PG sidecars match test schemas")
}

async fn fresh_pg_with_sidecars() -> (PgStorage, String) {
    let (pg, db_name) = fresh_pg().await;
    (pg.with_sidecars(pg_sidecars_for_test()), db_name)
}

async fn create_sidecar(pg: &PgStorage) -> Result<(), sqlx::Error> {
    sqlx::query("CREATE SCHEMA proxima_test")
        .execute(pg.pool_for_tests())
        .await?;
    sqlx::query(
        "CREATE TABLE proxima_test.entity_head_cited_fact_v1 (
            memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
            entity_key text NOT NULL,
            body text NOT NULL
        )",
    )
    .execute(pg.pool_for_tests())
    .await?;
    Ok(())
}

fn engine_for(pg: &PgStorage) -> Engine {
    Engine::new(registry_for_test()).with_storage_ports(Arc::new(pg.clone()).storage_ports())
}

fn fact(entity_key: &str, body: &str) -> StatefulFactV1 {
    StatefulFactV1 {
        entity_key: entity_key.to_string(),
        body: body.to_string(),
    }
}

fn draft_for(_owner: &Owner, payload_value: &Value) -> FactWriteCommand {
    let now = time::OffsetDateTime::now_utc();
    FactWriteCommand {
        schema_id: StatefulFactV1::schema_id(),
        schema_version: SchemaVersion::new(StatefulFactV1::SCHEMA_VERSION),
        payload: canonical_json_bytes(payload_value),
        rendered_text: None,
        lexical_language: None,
        receipt: Some(FactReceiptDraft {
            source_id: SourceId::new(format!("test/fact-entity-citation/{}", Uuid::now_v7())),
            source_batch_id: SourceBatchId::new(Uuid::now_v7()),
            observed_at: now,
            occurred_at: now,
        }),
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
        derived_from: None,
    }
}

async fn ingest_fact(
    pg: &PgStorage,
    engine: &Engine,
    owner: &Owner,
    payload: &StatefulFactV1,
) -> Result<proxima_core::FactIngestOutcome, StorageError> {
    let payload_value =
        serde_json::to_value(payload).map_err(|err| StorageError::Internal(err.to_string()))?;
    let draft = draft_for(owner, &payload_value);
    let authz = AuthzContext::single_owner(owner, AuthPath::HostBearer);
    let authorized = engine
        .authorize_fact_ingest(&authz, Relation::Ingest, draft)
        .await
        .map_err(|err| StorageError::Internal(err.to_string()))?;
    let sidecar_payload = SidecarPayload::fact(payload.clone());
    pg.ingest_fact_with_typed_sidecar(
        &authorized,
        std::slice::from_ref(&sidecar_payload),
        None,
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
    .fetch_one(pg.pool_for_tests())
    .await?;
    Ok(id.expect("stateful Fact has fact_entity_id"))
}

#[tokio::test]
async fn citation_of_entity_head_follows_updates_while_fact_citation_pins()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg_with_sidecars().await;

    let result = async {
        pg.run_migrations().await?;
        create_sidecar(&pg).await?;
        let owner = owner_fixture();
        let engine = engine_for(&pg);

        let v1 = ingest_fact(&pg, &engine, &owner, &fact("entity", "v1")).await?;
        let fact_entity_id = memory_fact_entity_id(&pg, v1.memory_id.into_inner()).await?;
        let v1_citation = pg
            .citation_of_fact(v1.memory_id)
            .await?
            .expect("v1 citation");

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let v2 = ingest_fact(&pg, &engine, &owner, &fact("entity", "v2")).await?;
        let v2_citation = pg
            .citation_of_fact(v2.memory_id)
            .await?
            .expect("v2 citation");

        let pinned_v1 = pg
            .citation_of_fact(v1.memory_id)
            .await?
            .expect("v1 citation remains pinned");
        assert_eq!(
            pinned_v1.citation_mapping_id,
            v1_citation.citation_mapping_id
        );

        let head = pg
            .citation_of_entity_head(
                std::slice::from_ref(&owner),
                FactEntityId::new(fact_entity_id),
            )
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
        .fetch_one(pg.pool_for_tests())
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

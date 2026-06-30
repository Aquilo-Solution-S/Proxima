//! Task 4 fact-entity cleanup fan-out coverage.

#![allow(clippy::too_many_lines)]

use std::sync::Arc;

use crate::common::{drop_db, fresh_pg, owner_fixture};
use proxima_core::engine::Engine;
use proxima_core::storage_ports::*;
use proxima_core::verbs::fact_ingest::{
    Citation, CitationMappingHint, CitedObjectHint, FactReceiptDraft, FactWriteCommand,
};
use proxima_core::verbs::schema::PayloadKind;
use proxima_core::{
    AuthPath, AuthorshipKindMask, AuthzContext, EdgeAuthorshipKind, EdgeId, EdgePayload,
    EndpointBinding, EntityKindMask, FactPayload, FlavorRegistry, FlavorRegistryFrozen, MemoryId,
    Owner, OwnerRef, PayloadKeyBuilder, Relation, RelationClass, RelationDescriptor, SchemaId,
    SchemaRef, SchemaVersion, SidecarPayload, SourceBatchId, SourceId, StorageError, UserId,
    canonical_json_bytes,
};
use proxima_storage_pg::sidecars::{
    PgEdgeSidecar, PgMemoryPayload, PgMemoryPayloadFuture, PgSidecarFuture,
};
use proxima_storage_pg::verbs::edge_write::{CheckedEdgeEndpoint, append_owner_checked_typed_edge};
use proxima_storage_pg::verbs::fact_ingest::{FactIngestSidecarFuture, PgFactSidecar};
use proxima_storage_pg::{
    PgSidecarRegistry, PgSidecarRegistryFrozen, PgStorage, register_core_pg_sidecars,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

const FOLLOW_RELATION: &str = "test/cleanup-follow-head";
const CITED_OBJECT_SCHEMA: &str = "test/cleanup-cited-object-v1";
const CITATION_MAPPING_SCHEMA: &str = "test/cleanup-citation-mapping-v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct StatefulFactV1 {
    entity_key: String,
    body: String,
    state: String,
}

impl FactPayload for StatefulFactV1 {
    const SCHEMA_ID: &'static str = "test/cleanup-stateful-fact-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn receipt_key(&self) -> Vec<u8> {
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

impl PgMemoryPayload for StatefulFactV1 {
    fn load_memory_payload(
        _ctx: proxima_storage_pg::sidecars::PgSidecarReadCtx<'_>,
        _memory_id: MemoryId,
    ) -> PgMemoryPayloadFuture<'_> {
        Box::pin(async { Ok(None) })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct FollowEdgeV1 {
    reason: String,
    confidence: i16,
}

impl EdgePayload for FollowEdgeV1 {
    const SCHEMA_ID: &'static str = "test/cleanup-follow-edge-v1";
    const SCHEMA_VERSION: u32 = 1;
    const RELATION_CLASS: RelationClass = RelationClass::Structural;

    fn sidecar_table() -> &'static str {
        "proxima_core.agent_link_v1"
    }
}

impl PgEdgeSidecar for FollowEdgeV1 {
    fn insert_edge_sidecar<'t>(
        &'t self,
        tx: &'t mut sqlx::PgConnection,
        edge_id: EdgeId,
    ) -> PgSidecarFuture<'t> {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO proxima_core.agent_link_v1
                    (edge_id, reason, confidence)
                 VALUES ($1, $2, $3)",
            )
            .bind(edge_id.into_inner())
            .bind(&self.reason)
            .bind(self.confidence)
            .execute(tx)
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
            Ok(())
        })
    }
}

fn registry_for_test() -> FlavorRegistryFrozen {
    let mut registry = FlavorRegistry::new();
    registry.add_fact_schema_or_panic_for_tests::<StatefulFactV1>();
    registry.add_edge_schema_or_panic_for_tests::<FollowEdgeV1>();
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
    registry.add_relation_or_panic_for_tests(RelationDescriptor::typed(
        FOLLOW_RELATION,
        RelationClass::Structural,
        SchemaRef::new(FollowEdgeV1::schema_id(), SchemaVersion::new(1)),
        EndpointBinding::FollowHead,
        EndpointBinding::FollowHead,
        EntityKindMask::fact(),
        EntityKindMask::fact(),
        AuthorshipKindMask::external_agent(),
    ));
    registry.freeze_or_panic_for_tests()
}

fn pg_sidecars_for_test() -> PgSidecarRegistryFrozen {
    let registry = registry_for_test();
    let mut sidecars = PgSidecarRegistry::new();
    register_core_pg_sidecars(&mut sidecars);
    sidecars.add_fact::<StatefulFactV1>();
    sidecars.add_edge::<FollowEdgeV1>();
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
        "CREATE TABLE proxima_test.cleanup_stateful_fact_v1 (
            memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
            entity_key text NOT NULL,
            body text NOT NULL,
            state text NOT NULL
        )",
    )
    .execute(pg.pool_for_tests())
    .await?;
    Ok(())
}

fn engine_for(pg: &PgStorage, registry: FlavorRegistryFrozen) -> Engine {
    Engine::new(registry).with_storage_ports(Arc::new(pg.clone()).storage_ports())
}

fn fact(entity_key: &str, body: &str) -> StatefulFactV1 {
    StatefulFactV1 {
        entity_key: entity_key.to_string(),
        body: body.to_string(),
        state: "Present".to_string(),
    }
}

fn draft_for(_owner: &Owner, payload_value: &Value, cited: bool) -> FactWriteCommand {
    let now = time::OffsetDateTime::now_utc();
    let citation = cited.then(|| Citation {
        object: CitedObjectHint {
            schema_id: SchemaId::new(CITED_OBJECT_SCHEMA.into()),
            schema_version: SchemaVersion::new(1),
            content_hash: *blake3::hash(
                format!(
                    "{}:{}",
                    payload_value["entity_key"].as_str().unwrap_or_default(),
                    Uuid::now_v7()
                )
                .as_bytes(),
            )
            .as_bytes(),
        },
        mapping: CitationMappingHint {
            schema_id: SchemaId::new(CITATION_MAPPING_SCHEMA.into()),
            schema_version: SchemaVersion::new(1),
        },
    });
    FactWriteCommand {
        schema_id: StatefulFactV1::schema_id(),
        schema_version: SchemaVersion::new(StatefulFactV1::SCHEMA_VERSION),
        payload: canonical_json_bytes(payload_value),
        rendered_text: None,
        citation,
        receipt: Some(FactReceiptDraft {
            source_id: SourceId::new(format!("test/fact-entity-cleanup/{}", Uuid::now_v7())),
            source_batch_id: SourceBatchId::new(Uuid::now_v7()),
            observed_at: now,
            occurred_at: now,
        }),
    }
}

async fn ingest_fact(
    pg: &PgStorage,
    engine: &Engine,
    owner: &Owner,
    payload: &StatefulFactV1,
    cited: bool,
) -> Result<proxima_core::FactIngestOutcome, StorageError> {
    let payload_value =
        serde_json::to_value(payload).map_err(|err| StorageError::Internal(err.to_string()))?;
    let draft = draft_for(owner, &payload_value, cited);
    let authz = AuthzContext::single_owner(owner, AuthPath::System);
    let authorized = engine
        .authorize_fact_ingest(&authz, Relation::Ingest, draft)
        .await
        .map_err(|err| StorageError::Internal(err.to_string()))?;
    let sidecar_payload = SidecarPayload::fact(payload.clone());
    pg.ingest_fact_with_typed_sidecar(&authorized, &sidecar_payload, None)
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

async fn current_memory_id(pg: &PgStorage, fact_entity_id: Uuid) -> Result<Uuid, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT current_memory_id
           FROM proxima_core.fact_entities
          WHERE fact_entity_id = $1",
    )
    .bind(fact_entity_id)
    .fetch_one(pg.pool_for_tests())
    .await
}

async fn append_follow_head_edge(
    pg: &PgStorage,
    registry: &FlavorRegistryFrozen,
    owner: &Owner,
    source_fact_entity_id: Uuid,
    target_fact_entity_id: Uuid,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let relation = registry
        .resolve_relation(FOLLOW_RELATION)
        .expect("follow-head relation");
    let edge_id = Uuid::now_v7();
    let payload = FollowEdgeV1 {
        reason: "cleanup sidecar proof".to_string(),
        confidence: 100,
    };
    let mut tx = pg.pool_for_tests().begin().await?;
    append_owner_checked_typed_edge(
        &mut tx,
        owner,
        EdgeId::new(edge_id),
        relation,
        CheckedEdgeEndpoint::fact_entity(proxima_core::FactEntityId::new(source_fact_entity_id)),
        CheckedEdgeEndpoint::fact_entity(proxima_core::FactEntityId::new(target_fact_entity_id)),
        EdgeAuthorshipKind::ExternalAgent,
        None,
        &payload,
    )
    .await?;
    tx.commit().await?;
    Ok(edge_id)
}

async fn age_memory(pg: &PgStorage, memory_id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE proxima_core.memories
            SET created_at = now() - INTERVAL '2 days'
          WHERE memory_id = $1",
    )
    .bind(memory_id)
    .execute(pg.pool_for_tests())
    .await?;
    Ok(())
}

async fn set_retention_and_cleanup(
    engine: &Engine,
    owner: &Owner,
) -> Result<proxima_core::verbs::fact_cleanup::CleanupDueFactsOutcome, proxima_core::ProtocolError>
{
    let authz = AuthzContext::single_owner(owner, AuthPath::System);
    engine.set_fact_retention(&authz, owner, 60).await?;
    engine.cleanup_due_facts(&authz, owner).await
}

async fn assert_edge_and_sidecar_exist(
    pg: &PgStorage,
    edge_id: Uuid,
) -> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(
        count_by_id(pg, "proxima_core.edges", "edge_id", edge_id).await?,
        1
    );
    assert_eq!(
        count_by_id(pg, "proxima_core.agent_link_v1", "edge_id", edge_id).await?,
        1
    );
    Ok(())
}

async fn assert_edge_and_sidecar_erased(
    pg: &PgStorage,
    edge_id: Uuid,
) -> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(
        count_by_id(pg, "proxima_core.edges", "edge_id", edge_id).await?,
        0
    );
    assert_eq!(
        count_by_id(pg, "proxima_core.agent_link_v1", "edge_id", edge_id).await?,
        0
    );
    Ok(())
}

async fn count_by_id(
    pg: &PgStorage,
    table: &str,
    column: &str,
    id: Uuid,
) -> Result<i64, sqlx::Error> {
    let sql = match (table, column) {
        ("proxima_core.edges", "edge_id") => {
            "SELECT count(*)::bigint FROM proxima_core.edges WHERE edge_id = $1"
        }
        ("proxima_core.agent_link_v1", "edge_id") => {
            "SELECT count(*)::bigint FROM proxima_core.agent_link_v1 WHERE edge_id = $1"
        }
        ("proxima_core.memories", "memory_id") => {
            "SELECT count(*)::bigint FROM proxima_core.memories WHERE memory_id = $1"
        }
        ("proxima_core.fact_entities", "fact_entity_id") => {
            "SELECT count(*)::bigint FROM proxima_core.fact_entities WHERE fact_entity_id = $1"
        }
        _ => panic!("unsupported test count query for {table}.{column}"),
    };
    sqlx::query_scalar(sql)
        .bind(id)
        .fetch_one(pg.pool_for_tests())
        .await
}

async fn assert_no_dangling_current_memory_id(pg: &PgStorage) -> Result<(), sqlx::Error> {
    let dangling: i64 = sqlx::query_scalar(
        "SELECT count(*)::bigint
           FROM proxima_core.fact_entities fe
           LEFT JOIN proxima_core.memories m
             ON m.memory_id = fe.current_memory_id
          WHERE m.memory_id IS NULL",
    )
    .fetch_one(pg.pool_for_tests())
    .await?;
    assert_eq!(dangling, 0);
    Ok(())
}

async fn assert_no_orphan_edge_sidecars(pg: &PgStorage) -> Result<(), sqlx::Error> {
    let orphaned: i64 = sqlx::query_scalar(
        "SELECT count(*)::bigint
           FROM proxima_core.agent_link_v1 sidecar
           LEFT JOIN proxima_core.edges edge_row
             ON edge_row.edge_id = sidecar.edge_id
          WHERE edge_row.edge_id IS NULL",
    )
    .fetch_one(pg.pool_for_tests())
    .await?;
    assert_eq!(orphaned, 0);
    Ok(())
}

async fn memory_exists(pg: &PgStorage, memory_id: Uuid) -> Result<bool, sqlx::Error> {
    let count = count_by_id(pg, "proxima_core.memories", "memory_id", memory_id).await?;
    Ok(count == 1)
}

#[tokio::test]
async fn erasing_non_head_version_keeps_follow_head_edge() -> Result<(), Box<dyn std::error::Error>>
{
    let (pg, db_name) = fresh_pg_with_sidecars().await;

    let result = async {
        pg.run_migrations().await?;
        create_sidecar(&pg).await?;
        let owner = owner_fixture();
        let registry = registry_for_test();
        let engine = engine_for(&pg, registry.clone());

        let source_v1 = ingest_fact(&pg, &engine, &owner, &fact("source", "v1"), true).await?;
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let source_v2 = ingest_fact(&pg, &engine, &owner, &fact("source", "v2"), true).await?;
        let target = ingest_fact(&pg, &engine, &owner, &fact("target", "v1"), true).await?;
        let source_entity = memory_fact_entity_id(&pg, source_v1.memory_id.into_inner()).await?;
        let edge_id = append_follow_head_edge(
            &pg,
            &registry,
            &owner,
            source_entity,
            memory_fact_entity_id(&pg, target.memory_id.into_inner()).await?,
        )
        .await?;
        assert_eq!(
            current_memory_id(&pg, source_entity).await?,
            source_v2.memory_id.into_inner()
        );

        age_memory(&pg, source_v1.memory_id.into_inner()).await?;
        let cleanup = set_retention_and_cleanup(&engine, &owner).await?;
        assert_eq!(cleanup.facts_erased, 1);

        assert!(!memory_exists(&pg, source_v1.memory_id.into_inner()).await?);
        assert_eq!(
            current_memory_id(&pg, source_entity).await?,
            source_v2.memory_id.into_inner()
        );
        assert_edge_and_sidecar_exist(&pg, edge_id).await?;
        assert_no_dangling_current_memory_id(&pg).await?;
        assert_no_orphan_edge_sidecars(&pg).await?;
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn erasing_current_head_repoints_to_prior_live_version()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg_with_sidecars().await;

    let result = async {
        pg.run_migrations().await?;
        create_sidecar(&pg).await?;
        let owner = owner_fixture();
        let registry = registry_for_test();
        let engine = engine_for(&pg, registry.clone());

        let source_v1 = ingest_fact(&pg, &engine, &owner, &fact("source", "v1"), false).await?;
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let source_v2 = ingest_fact(&pg, &engine, &owner, &fact("source", "v2"), true).await?;
        let target = ingest_fact(&pg, &engine, &owner, &fact("target", "v1"), true).await?;
        let source_entity = memory_fact_entity_id(&pg, source_v1.memory_id.into_inner()).await?;
        let edge_id = append_follow_head_edge(
            &pg,
            &registry,
            &owner,
            source_entity,
            memory_fact_entity_id(&pg, target.memory_id.into_inner()).await?,
        )
        .await?;
        assert_eq!(
            current_memory_id(&pg, source_entity).await?,
            source_v2.memory_id.into_inner()
        );

        age_memory(&pg, source_v2.memory_id.into_inner()).await?;
        let cleanup = set_retention_and_cleanup(&engine, &owner).await?;
        assert_eq!(cleanup.facts_erased, 1);

        assert!(!memory_exists(&pg, source_v2.memory_id.into_inner()).await?);
        assert_eq!(
            current_memory_id(&pg, source_entity).await?,
            source_v1.memory_id.into_inner()
        );
        assert_edge_and_sidecar_exist(&pg, edge_id).await?;
        assert_no_dangling_current_memory_id(&pg).await?;
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn erasing_last_version_deletes_entity_follow_head_edges_and_sidecars()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg_with_sidecars().await;

    let result = async {
        pg.run_migrations().await?;
        create_sidecar(&pg).await?;
        let owner = owner_fixture();
        let registry = registry_for_test();
        let engine = engine_for(&pg, registry.clone());

        let source = ingest_fact(&pg, &engine, &owner, &fact("source", "v1"), true).await?;
        let target = ingest_fact(&pg, &engine, &owner, &fact("target", "v1"), true).await?;
        let source_entity = memory_fact_entity_id(&pg, source.memory_id.into_inner()).await?;
        let target_entity = memory_fact_entity_id(&pg, target.memory_id.into_inner()).await?;
        let edge_id =
            append_follow_head_edge(&pg, &registry, &owner, source_entity, target_entity).await?;
        age_memory(&pg, source.memory_id.into_inner()).await?;

        let cleanup = set_retention_and_cleanup(&engine, &owner).await?;
        assert_eq!(cleanup.facts_erased, 1);

        assert_eq!(
            count_by_id(
                &pg,
                "proxima_core.fact_entities",
                "fact_entity_id",
                source_entity
            )
            .await?,
            0
        );
        assert_edge_and_sidecar_erased(&pg, edge_id).await?;
        assert_no_orphan_edge_sidecars(&pg).await?;
        assert_no_dangling_current_memory_id(&pg).await?;
        assert_eq!(
            current_memory_id(&pg, target_entity).await?,
            target.memory_id.into_inner()
        );
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn cleanup_is_owner_scoped_for_identical_natural_keys()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg_with_sidecars().await;

    let result = async {
        pg.run_migrations().await?;
        create_sidecar(&pg).await?;
        let owner = owner_fixture();
        let other = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let registry = registry_for_test();
        let engine = engine_for(&pg, registry);
        let owner_fact =
            ingest_fact(&pg, &engine, &owner, &fact("same-key", "owner"), true).await?;
        let other_fact =
            ingest_fact(&pg, &engine, &other, &fact("same-key", "other"), true).await?;
        let other_entity = memory_fact_entity_id(&pg, other_fact.memory_id.into_inner()).await?;

        age_memory(&pg, owner_fact.memory_id.into_inner()).await?;
        let cleanup = set_retention_and_cleanup(&engine, &owner).await?;
        assert_eq!(cleanup.facts_erased, 1);

        assert!(!memory_exists(&pg, owner_fact.memory_id.into_inner()).await?);
        assert!(memory_exists(&pg, other_fact.memory_id.into_inner()).await?);
        assert_eq!(
            count_by_id(
                &pg,
                "proxima_core.fact_entities",
                "fact_entity_id",
                other_entity
            )
            .await?,
            1
        );
        assert_no_dangling_current_memory_id(&pg).await?;
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn provenance_tombstone_walk_stays_memory_id_pinned() -> Result<(), Box<dyn std::error::Error>>
{
    let (pg, db_name) = fresh_pg_with_sidecars().await;

    let result = async {
        pg.run_migrations().await?;
        create_sidecar(&pg).await?;
        let owner = owner_fixture();
        let registry = registry_for_test();
        let engine = engine_for(&pg, registry.clone());

        let due = ingest_fact(&pg, &engine, &owner, &fact("due", "v1"), true).await?;
        let neighbor = ingest_fact(&pg, &engine, &owner, &fact("neighbor", "v1"), true).await?;
        let edge_id = append_follow_head_edge(
            &pg,
            &registry,
            &owner,
            memory_fact_entity_id(&pg, neighbor.memory_id.into_inner()).await?,
            memory_fact_entity_id(&pg, due.memory_id.into_inner()).await?,
        )
        .await?;
        let derivative_id =
            insert_direct_derivative(&pg, &owner, due.memory_id.into_inner()).await?;

        age_memory(&pg, due.memory_id.into_inner()).await?;
        let cleanup = set_retention_and_cleanup(&engine, &owner).await?;
        assert_eq!(cleanup.facts_erased, 1);
        assert_eq!(cleanup.derivatives_tombstoned, 1);

        let derivative_tombstoned: Option<time::OffsetDateTime> = sqlx::query_scalar(
            "SELECT tombstoned_at
               FROM proxima_core.memories
              WHERE memory_id = $1",
        )
        .bind(derivative_id)
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert!(derivative_tombstoned.is_some());

        let neighbor_tombstoned: Option<time::OffsetDateTime> = sqlx::query_scalar(
            "SELECT tombstoned_at
               FROM proxima_core.memories
              WHERE memory_id = $1",
        )
        .bind(neighbor.memory_id.into_inner())
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert!(neighbor_tombstoned.is_none());
        assert_edge_and_sidecar_erased(&pg, edge_id).await?;
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

async fn insert_direct_derivative(
    pg: &PgStorage,
    owner: &Owner,
    fact_id: Uuid,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let derivative_id = Uuid::now_v7();
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(owner);
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_kind, owner_id, schema_id, schema_version, kind, text,
             operator_kind, operator_id, input_contract_id, source_batch_id, model_id, prompt_version)
         VALUES ($1, $2, $3, 'test/cleanup-abstraction-v1', 1,
                 'Abstraction', 'derivative', 'AtoA',
                 '00000000-0000-0000-0000-000000000391'::uuid,
                 '00000000-0000-0000-0000-000000000392'::uuid, NULL,
                 'test-model', 'test-prompt')"
    )
    .bind(derivative_id)
    .bind(owner_kind)
    .bind(owner_id)
    .execute(pg.pool_for_tests())
    .await?;

    sqlx::query(
        "INSERT INTO proxima_core.edges
            (edge_id, owner_kind, owner_id, relation, relation_class,
             source_kind, source_memory_id,
             target_kind, target_memory_id,
             authorship_kind, authorship_owner_memory_id)
         VALUES ($1, $2, $3, 'core/derived-from', 'Provenance',
                 'Abstraction', $4,
                 'Fact', $5,
                 'OperatorFtoA', $4)",
    )
    .bind(Uuid::now_v7())
    .bind(owner_kind)
    .bind(owner_id)
    .bind(derivative_id)
    .bind(fact_id)
    .execute(pg.pool_for_tests())
    .await?;
    Ok(derivative_id)
}

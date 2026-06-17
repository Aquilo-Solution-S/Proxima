//! Task 4 fact-entity cleanup fan-out coverage.

#![allow(clippy::too_many_lines)]

use std::sync::Arc;

use crate::common::{drop_db, fresh_pg, owner_fixture};
use proxima_core::engine::Engine;
use proxima_core::storage::Storage;
use proxima_core::verbs::event_ingest::{
    Citation, CitationMappingHint, CitedObjectHint, EventDraft,
};
use proxima_core::verbs::schema::PayloadKind;
use proxima_core::{
    AuthPath, AuthorshipKindMask, AuthzContext, EdgeAuthorshipKind, EdgePayload, EndpointBinding,
    EntityKind, EntityKindMask, FactPayload, FlavorRegistry, FlavorRegistryFrozen, OrgId, Owner,
    Principal, RelationClass, RelationDescriptor, Role, SchemaId, SchemaRef, SchemaVersion,
    SourceBatchId, SourceId, StorageError, UserId, canonical_json_bytes,
};
use proxima_storage_pg::PgStorage;
use proxima_storage_pg::verbs::edge_append::{EdgeDraft, append_edge_in_tx};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
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

fn registry_for_test() -> FlavorRegistryFrozen {
    let mut registry = FlavorRegistry::new();
    registry.add_fact_schema::<StatefulFactV1>();
    registry.add_edge_schema::<FollowEdgeV1>();
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
    registry.add_relation(RelationDescriptor::typed(
        FOLLOW_RELATION,
        RelationClass::Structural,
        SchemaRef::new(FollowEdgeV1::schema_id(), SchemaVersion::new(1)),
        EndpointBinding::FollowHead,
        EndpointBinding::FollowHead,
        EntityKindMask::fact(),
        EntityKindMask::fact(),
        AuthorshipKindMask::external_agent(),
    ));
    registry.freeze()
}

async fn create_sidecar(pg: &PgStorage) -> Result<(), sqlx::Error> {
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

fn engine_for(pg: &PgStorage, registry: FlavorRegistryFrozen) -> Engine {
    let storage: Arc<dyn Storage> = Arc::new(pg.clone());
    Engine::new(registry).with_storage(storage)
}

fn fact(entity_key: &str, body: &str) -> StatefulFactV1 {
    StatefulFactV1 {
        entity_key: entity_key.to_string(),
        body: body.to_string(),
        state: "Present".to_string(),
    }
}

fn draft_for(owner: &Owner, payload_value: &Value, cited: bool) -> EventDraft {
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
    EventDraft {
        source_id: SourceId::new(format!("test/fact-entity-cleanup/{}", Uuid::now_v7())),
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
        citation,
    }
}

async fn ingest_fact(
    pg: &PgStorage,
    engine: &Engine,
    owner: &Owner,
    payload: &StatefulFactV1,
    cited: bool,
) -> Result<proxima_core::EventIngestOutcome, StorageError> {
    let payload_value =
        serde_json::to_value(payload).map_err(|err| StorageError::Internal(err.to_string()))?;
    let draft = draft_for(owner, &payload_value, cited);
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

async fn current_memory_id(pg: &PgStorage, fact_entity_id: Uuid) -> Result<Uuid, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT current_memory_id
           FROM proxima_core.fact_entities
          WHERE fact_entity_id = $1",
    )
    .bind(fact_entity_id)
    .fetch_one(pg.pool())
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
    let payload = json!({
        "reason": "cleanup sidecar proof",
        "confidence": 100
    });
    let mut tx = pg.pool().begin().await?;
    append_edge_in_tx(
        &mut tx,
        &EdgeDraft {
            edge_id,
            relation,
            source_kind: EntityKind::Fact,
            source_memory_id: None,
            source_goal_id: None,
            source_fact_entity_id: Some(source_fact_entity_id),
            target_kind: EntityKind::Fact,
            target_memory_id: None,
            target_goal_id: None,
            target_fact_entity_id: Some(target_fact_entity_id),
            authorship_kind: EdgeAuthorshipKind::ExternalAgent,
            authorship_owner_memory_id: None,
            owner,
        },
        Some(&payload),
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
    .execute(pg.pool())
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
    sqlx::query_scalar(sql).bind(id).fetch_one(pg.pool()).await
}

async fn assert_no_dangling_current_memory_id(pg: &PgStorage) -> Result<(), sqlx::Error> {
    let dangling: i64 = sqlx::query_scalar(
        "SELECT count(*)::bigint
           FROM proxima_core.fact_entities fe
           LEFT JOIN proxima_core.memories m
             ON m.memory_id = fe.current_memory_id
          WHERE m.memory_id IS NULL",
    )
    .fetch_one(pg.pool())
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
    .fetch_one(pg.pool())
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
    let (pg, db_name) = fresh_pg().await;

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
    let (pg, db_name) = fresh_pg().await;

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
    let (pg, db_name) = fresh_pg().await;

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
    let (pg, db_name) = fresh_pg().await;

    let result = async {
        pg.run_migrations().await?;
        create_sidecar(&pg).await?;
        let owner = owner_fixture();
        let other = Owner {
            principal: Principal::User(UserId::new(Uuid::now_v7())),
            org_id: OrgId::new(Uuid::now_v7()),
        };
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
    let (pg, db_name) = fresh_pg().await;

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
        .fetch_one(pg.pool())
        .await?;
        assert!(derivative_tombstoned.is_some());

        let neighbor_tombstoned: Option<time::OffsetDateTime> = sqlx::query_scalar(
            "SELECT tombstoned_at
               FROM proxima_core.memories
              WHERE memory_id = $1",
        )
        .bind(neighbor.memory_id.into_inner())
        .fetch_one(pg.pool())
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
    let (owner_kind, owner_principal_id, owner_org_id) = owner.columns();
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_principal_kind, owner_principal_id, owner_org_id,
             schema_id, schema_version, kind, text, operator_kind, model_id,
             prompt_version, personality_instance_id, wake_chain_depth)
         VALUES ($1, $2, $3, $4, 'test/cleanup-abstraction-v1', 1,
                 'Abstraction', 'derivative', 'FtoA', 'test-model',
                 'test-prompt', '00000000-0000-0000-0000-000000000000'::uuid, 0)",
    )
    .bind(derivative_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .execute(pg.pool())
    .await?;

    sqlx::query(
        "INSERT INTO proxima_core.edges
            (edge_id, relation, relation_class,
             source_kind, source_memory_id,
             target_kind, target_memory_id,
             authorship_kind, authorship_owner_memory_id,
             owner_principal_kind, owner_principal_id, owner_org_id)
         VALUES ($1, 'core/derived-from', 'Provenance',
                 'Abstraction', $2,
                 'Fact', $3,
                 'OperatorFtoA', $2,
                 $4, $5, $6)",
    )
    .bind(Uuid::now_v7())
    .bind(derivative_id)
    .bind(fact_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .execute(pg.pool())
    .await?;
    Ok(derivative_id)
}

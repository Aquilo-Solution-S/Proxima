//! Task 3 fact-entity edge write, graph resolution, and change-log coverage.

#![allow(clippy::too_many_lines)]

use std::sync::Arc;

use crate::common::{drop_db, fresh_pg, owner_fixture};
use proxima_core::engine::Engine;
use proxima_core::mcp::core_tools::list_events::{ListEventsArgs, ListEventsTool};
use proxima_core::mcp::{McpAuthorContext, McpToolCtx, McpToolExtensions, OutputMode};
use proxima_core::storage::Storage;
use proxima_core::verbs::event_history::EventHistoryRequest;
use proxima_core::verbs::event_ingest::EventDraft;
use proxima_core::verbs::query::{QueryRequest, TombstoneFilter};
use proxima_core::{
    AuthPath, AuthorshipKindMask, AuthzContext, ChangeEventKind, EdgeAuthorshipKind,
    EndpointBinding, EntityKind, EntityKindMask, EntityRef, FactPayload, FactTombstone,
    FlavorRegistry, FlavorRegistryFrozen, McpTool, MemoryId, OrgId, Owner, OwnerPrincipalKind,
    PayloadKeyBuilder, Principal, RelationClass, RelationDescriptor, Role, SchemaVersion,
    SidecarPayload, SourceBatchId, SourceId, StorageError, UserId, canonical_json_bytes,
};
use proxima_storage_pg::sidecars::{PgMemoryPayload, PgMemoryPayloadFuture};
use proxima_storage_pg::verbs::edge_append::{EdgeDraft, append_edge_in_tx};
use proxima_storage_pg::verbs::event_ingest::{EventIngestSidecarFuture, PgFactSidecar};
use proxima_storage_pg::{
    PgSidecarRegistry, PgSidecarRegistryFrozen, PgStorage, register_core_pg_sidecars,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

const TEST_FOLLOW_RELATION: &str = "test/follows-head";
const TEST_PIN_RELATION: &str = "test/pins-version";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct StatefulFactV1 {
    entity_key: String,
    body: String,
    state: String,
}

impl FactPayload for StatefulFactV1 {
    const SCHEMA_ID: &'static str = "test/stateful-fact-v1";
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
        Some("proxima_test.stateful_fact_v1")
    }

    fn natural_key_columns() -> &'static [&'static str] {
        &["entity_key"]
    }

    fn tombstone() -> Option<FactTombstone> {
        Some(FactTombstone {
            column: "state",
            value: "Deleted",
        })
    }
}

impl PgFactSidecar for StatefulFactV1 {
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
                "INSERT INTO proxima_test.stateful_fact_v1
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
        _pool: &sqlx::PgPool,
        _memory_id: MemoryId,
    ) -> PgMemoryPayloadFuture<'_> {
        Box::pin(async { Ok(None) })
    }
}

fn registry_for_test() -> FlavorRegistryFrozen {
    let mut registry = FlavorRegistry::new();
    registry.add_fact_schema::<StatefulFactV1>();
    registry.add_relation(RelationDescriptor::substrate(
        TEST_FOLLOW_RELATION,
        RelationClass::Structural,
        EndpointBinding::FollowHead,
        EndpointBinding::FollowHead,
        EntityKindMask::fact(),
        EntityKindMask::fact(),
        AuthorshipKindMask::event_source().union(AuthorshipKindMask::external_agent()),
    ));
    registry.add_relation(RelationDescriptor::substrate(
        TEST_PIN_RELATION,
        RelationClass::Structural,
        EndpointBinding::Pin,
        EndpointBinding::Pin,
        EntityKindMask::fact(),
        EntityKindMask::fact(),
        AuthorshipKindMask::event_source().union(AuthorshipKindMask::external_agent()),
    ));
    registry.freeze()
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
    for sql in [
        "CREATE SCHEMA proxima_test",
        "CREATE TABLE proxima_test.stateful_fact_v1 (
            memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
            entity_key text NOT NULL,
            body text NOT NULL,
            state text NOT NULL
        )",
    ] {
        sqlx::query(sql).execute(pg.pool()).await?;
    }
    Ok(())
}

fn engine_for(pg: &PgStorage, registry: FlavorRegistryFrozen) -> Engine {
    let storage: Arc<dyn Storage> = Arc::new(pg.clone());
    Engine::new(registry).with_storage(storage)
}

fn fact(entity_key: &str, body: &str, state: &str) -> StatefulFactV1 {
    StatefulFactV1 {
        entity_key: entity_key.to_string(),
        body: body.to_string(),
        state: state.to_string(),
    }
}

fn draft_for(owner: &Owner, payload_value: &Value) -> EventDraft {
    let now = time::OffsetDateTime::now_utc();
    EventDraft {
        source_id: SourceId::new(format!("test/fact-entity-edge/{}", Uuid::now_v7())),
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
        citation: None,
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
    let sidecar_payload = SidecarPayload::fact(payload.clone());
    pg.ingest_event_with_typed_sidecar(&authorized, &sidecar_payload, None)
        .await
}

async fn memory_fact_entity_id(
    pg: &PgStorage,
    memory_id: proxima_core::MemoryId,
) -> Result<Uuid, sqlx::Error> {
    let id: Option<Uuid> = sqlx::query_scalar(
        "SELECT fact_entity_id
           FROM proxima_core.memories
          WHERE memory_id = $1",
    )
    .bind(memory_id.into_inner())
    .fetch_one(pg.pool())
    .await?;
    Ok(id.expect("stateful test memory has fact_entity_id"))
}

async fn append_follow_head_edge(
    pg: &PgStorage,
    registry: &FlavorRegistryFrozen,
    owner: &Owner,
    source_fact_entity_id: Uuid,
    target_fact_entity_id: Uuid,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let relation = registry
        .resolve_relation(TEST_FOLLOW_RELATION)
        .expect("follow relation");
    let edge_id = Uuid::now_v7();
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
    )
    .await?;
    tx.commit().await?;
    Ok(edge_id)
}

async fn append_pinned_edge(
    pg: &PgStorage,
    registry: &FlavorRegistryFrozen,
    owner: &Owner,
    source_memory_id: Uuid,
    target_memory_id: Uuid,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let relation = registry
        .resolve_relation(TEST_PIN_RELATION)
        .expect("pin relation");
    let edge_id = Uuid::now_v7();
    let mut tx = pg.pool().begin().await?;
    append_edge_in_tx(
        &mut tx,
        &EdgeDraft {
            edge_id,
            relation,
            source_kind: EntityKind::Fact,
            source_memory_id: Some(source_memory_id),
            source_goal_id: None,
            source_fact_entity_id: None,
            target_kind: EntityKind::Fact,
            target_memory_id: Some(target_memory_id),
            target_goal_id: None,
            target_fact_entity_id: None,
            authorship_kind: EdgeAuthorshipKind::ExternalAgent,
            authorship_owner_memory_id: None,
            owner,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(edge_id)
}

fn owner_parts(owner: &Owner) -> (OwnerPrincipalKind, Uuid, Uuid) {
    owner.columns()
}

async fn raw_insert_follow_edge(
    pg: &PgStorage,
    owner: &Owner,
    source_fact_entity_id: Uuid,
    target_fact_entity_id: Uuid,
) -> Result<(), sqlx::Error> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_parts(owner);
    sqlx::query(
        "INSERT INTO proxima_core.edges
            (edge_id, relation, relation_class,
             source_kind, source_fact_entity_id,
             target_kind, target_fact_entity_id,
             authorship_kind, authorship_owner_memory_id,
             owner_principal_kind, owner_principal_id, owner_org_id)
         VALUES ($1, 'test/raw-follow', 'Structural',
                 'Fact', $2,
                 'Fact', $3,
                 'ExternalAgent', NULL,
                 $4, $5, $6)",
    )
    .bind(Uuid::now_v7())
    .bind(source_fact_entity_id)
    .bind(target_fact_entity_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .execute(pg.pool())
    .await?;
    Ok(())
}

#[tokio::test]
async fn follow_head_edge_writes_log_and_graph_resolves_to_latest_head()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg_with_sidecars().await;

    let result = async {
        pg.run_migrations().await?;
        create_sidecar(&pg).await?;
        let owner = owner_fixture();
        let registry = registry_for_test();
        let engine = engine_for(&pg, registry.clone());

        let source_v1 = ingest_fact(&pg, &engine, &owner, &fact("source", "v1", "Present")).await?;
        let target_v1 = ingest_fact(&pg, &engine, &owner, &fact("target", "v1", "Present")).await?;
        let source_entity = memory_fact_entity_id(&pg, source_v1.memory_id).await?;
        let target_entity = memory_fact_entity_id(&pg, target_v1.memory_id).await?;
        let edge_id =
            append_follow_head_edge(&pg, &registry, &owner, source_entity, target_entity).await?;

        let raw: (Option<Uuid>, Option<Uuid>, Option<Uuid>, Option<Uuid>) = sqlx::query_as(
            "SELECT source_memory_id, source_fact_entity_id,
                    target_memory_id, target_fact_entity_id
               FROM proxima_core.edges
              WHERE edge_id = $1",
        )
        .bind(edge_id)
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(raw, (None, Some(source_entity), None, Some(target_entity)));

        let events = pg
            .list_change_events_after(&owner, Uuid::nil(), 100)
            .await?;
        let edge_event = events
            .iter()
            .find_map(|row| match &row.event.kind {
                ChangeEventKind::EdgeAppend {
                    edge_id: seen,
                    source,
                    target,
                    ..
                } if *seen == edge_id => Some((*source, *target)),
                _ => None,
            })
            .expect("fact-entity EdgeAppend is decoded");
        assert_eq!(
            edge_event.0,
            EntityRef::FactEntity(proxima_core::FactEntityId::new(source_entity))
        );
        assert_eq!(
            edge_event.1,
            EntityRef::FactEntity(proxima_core::FactEntityId::new(target_entity))
        );

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let source_v2 = ingest_fact(&pg, &engine, &owner, &fact("source", "v2", "Present")).await?;

        let authz = AuthzContext::single_owner(&owner, AuthPath::System);
        let mut req = QueryRequest::for_principal(owner.principal.clone());
        req.limit = 100;
        let response = engine.query(&authz, &req).await?;
        let edge = response
            .edges
            .iter()
            .find(|edge| edge.id == edge_id)
            .expect("follow-head graph edge");
        assert_eq!(
            edge.source,
            EntityRef::Memory(source_v2.memory_id),
            "graph rows resolve follow-head endpoints to the current memory"
        );
        assert_eq!(edge.target, EntityRef::Memory(target_v1.memory_id));
        assert!(!matches!(edge.source, EntityRef::FactEntity(_)));
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn follow_head_tombstoned_head_uses_existing_visibility()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg_with_sidecars().await;

    let result = async {
        pg.run_migrations().await?;
        create_sidecar(&pg).await?;
        let owner = owner_fixture();
        let registry = registry_for_test();
        let engine = engine_for(&pg, registry.clone());
        let authz = AuthzContext::single_owner(&owner, AuthPath::System);

        let source = ingest_fact(&pg, &engine, &owner, &fact("source", "v1", "Present")).await?;
        let target_v1 = ingest_fact(&pg, &engine, &owner, &fact("target", "v1", "Present")).await?;
        let source_entity = memory_fact_entity_id(&pg, source.memory_id).await?;
        let target_entity = memory_fact_entity_id(&pg, target_v1.memory_id).await?;
        let edge_id =
            append_follow_head_edge(&pg, &registry, &owner, source_entity, target_entity).await?;

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let target_tombstone =
            ingest_fact(&pg, &engine, &owner, &fact("target", "deleted", "Deleted")).await?;

        let mut req = QueryRequest::for_principal(owner.principal.clone());
        req.limit = 100;
        let response = engine.query(&authz, &req).await?;
        assert!(
            response.edges.iter().all(|edge| edge.id != edge_id),
            "PresentOnly hides edges whose resolved head is a sidecar tombstone"
        );

        req.tombstones = TombstoneFilter::IncludeTombstoned;
        let response = engine.query(&authz, &req).await?;
        let edge = response
            .edges
            .iter()
            .find(|edge| edge.id == edge_id)
            .expect("IncludeTombstoned shows follow-head edge");
        assert_eq!(edge.source, EntityRef::Memory(source.memory_id));
        assert_eq!(edge.target, EntityRef::Memory(target_tombstone.memory_id));
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn endpoint_guards_reject_binding_mismatch_and_invalid_fact_entities()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg_with_sidecars().await;

    let result = async {
        pg.run_migrations().await?;
        create_sidecar(&pg).await?;
        let owner = owner_fixture();
        let registry = registry_for_test();
        let engine = engine_for(&pg, registry.clone());
        let source = ingest_fact(&pg, &engine, &owner, &fact("source", "v1", "Present")).await?;
        let target = ingest_fact(&pg, &engine, &owner, &fact("target", "v1", "Present")).await?;
        let source_entity = memory_fact_entity_id(&pg, source.memory_id).await?;
        let target_entity = memory_fact_entity_id(&pg, target.memory_id).await?;

        let follow_relation = registry
            .resolve_relation(TEST_FOLLOW_RELATION)
            .expect("follow relation");
        let mut tx = pg.pool().begin().await?;
        let err = append_edge_in_tx(
            &mut tx,
            &EdgeDraft {
                edge_id: Uuid::now_v7(),
                relation: follow_relation,
                source_kind: EntityKind::Fact,
                source_memory_id: Some(source.memory_id.into_inner()),
                source_goal_id: None,
                source_fact_entity_id: None,
                target_kind: EntityKind::Fact,
                target_memory_id: Some(target.memory_id.into_inner()),
                target_goal_id: None,
                target_fact_entity_id: None,
                authorship_kind: EdgeAuthorshipKind::ExternalAgent,
                authorship_owner_memory_id: None,
                owner: &owner,
            },
        )
        .await
        .expect_err("FollowHead relation must reject pinned endpoints");
        assert!(matches!(err, StorageError::ConstraintViolation(_)));

        let pin_relation = registry
            .resolve_relation(TEST_PIN_RELATION)
            .expect("pin relation");
        let err = append_edge_in_tx(
            &mut tx,
            &EdgeDraft {
                edge_id: Uuid::now_v7(),
                relation: pin_relation,
                source_kind: EntityKind::Fact,
                source_memory_id: None,
                source_goal_id: None,
                source_fact_entity_id: Some(source_entity),
                target_kind: EntityKind::Fact,
                target_memory_id: None,
                target_goal_id: None,
                target_fact_entity_id: Some(target_entity),
                authorship_kind: EdgeAuthorshipKind::ExternalAgent,
                authorship_owner_memory_id: None,
                owner: &owner,
            },
        )
        .await
        .expect_err("Pin relation must reject fact-entity endpoints");
        assert!(matches!(err, StorageError::ConstraintViolation(_)));

        let err = append_edge_in_tx(
            &mut tx,
            &EdgeDraft {
                edge_id: Uuid::now_v7(),
                relation: follow_relation,
                source_kind: EntityKind::Fact,
                source_memory_id: Some(source.memory_id.into_inner()),
                source_goal_id: None,
                source_fact_entity_id: Some(source_entity),
                target_kind: EntityKind::Fact,
                target_memory_id: None,
                target_goal_id: None,
                target_fact_entity_id: Some(target_entity),
                authorship_kind: EdgeAuthorshipKind::ExternalAgent,
                authorship_owner_memory_id: None,
                owner: &owner,
            },
        )
        .await
        .expect_err("Rust exactly-one guard rejects three-way endpoint");
        assert!(matches!(err, StorageError::ConstraintViolation(_)));
        tx.rollback().await?;

        let (owner_kind, owner_principal_id, owner_org_id) = owner_parts(&owner);
        let err = sqlx::query(
            "INSERT INTO proxima_core.edges
                (edge_id, relation, relation_class,
                 source_kind, source_memory_id, source_fact_entity_id,
                 target_kind, target_fact_entity_id,
                 authorship_kind, authorship_owner_memory_id,
                 owner_principal_kind, owner_principal_id, owner_org_id)
             VALUES ($1, 'test/bad-three-way', 'Structural',
                     'Fact', $2, $3,
                     'Fact', $4,
                     'ExternalAgent', NULL,
                     $5, $6, $7)",
        )
        .bind(Uuid::now_v7())
        .bind(source.memory_id.into_inner())
        .bind(source_entity)
        .bind(target_entity)
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(owner_org_id)
        .execute(pg.pool())
        .await
        .expect_err("SQL exactly-one CHECK rejects memory + fact entity");
        assert!(err.to_string().contains("edges_source_endpoint_chk"));

        let err = sqlx::query(
            "INSERT INTO proxima_core.edges
                (edge_id, relation, relation_class,
                 source_kind, source_fact_entity_id,
                 target_kind, target_fact_entity_id,
                 authorship_kind, authorship_owner_memory_id,
                 owner_principal_kind, owner_principal_id, owner_org_id)
             VALUES ($1, 'test/bad-kind', 'Structural',
                     'Abstraction', $2,
                     'Fact', $3,
                     'ExternalAgent', NULL,
                     $4, $5, $6)",
        )
        .bind(Uuid::now_v7())
        .bind(source_entity)
        .bind(target_entity)
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(owner_org_id)
        .execute(pg.pool())
        .await
        .expect_err("fact_entity endpoint must declare Fact kind");
        assert!(
            err.to_string().contains("edges_source_endpoint_chk")
                || err.to_string().contains("source kind Abstraction")
        );

        let err = raw_insert_follow_edge(&pg, &owner, Uuid::now_v7(), target_entity)
            .await
            .expect_err("missing fact entity endpoint must be rejected");
        assert!(
            err.to_string().contains("endpoint not found")
                || err
                    .as_database_error()
                    .is_some_and(sqlx::error::DatabaseError::is_foreign_key_violation)
        );

        let other = Owner {
            principal: Principal::User(UserId::new(Uuid::now_v7())),
            org_id: OrgId::new(Uuid::now_v7()),
        };
        let other_source =
            ingest_fact(&pg, &engine, &other, &fact("other", "v1", "Present")).await?;
        let other_entity = memory_fact_entity_id(&pg, other_source.memory_id).await?;
        let err = raw_insert_follow_edge(&pg, &owner, source_entity, other_entity)
            .await
            .expect_err("cross-owner fact entity must be rejected");
        assert!(err.to_string().contains("crosses Owner boundary"));

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn event_history_and_list_events_preserve_fact_entity_endpoints()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg_with_sidecars().await;

    let result = async {
        pg.run_migrations().await?;
        create_sidecar(&pg).await?;
        let owner = owner_fixture();
        let registry = registry_for_test();
        let engine = Arc::new(engine_for(&pg, registry.clone()));
        let source = ingest_fact(&pg, &engine, &owner, &fact("source", "v1", "Present")).await?;
        let target = ingest_fact(&pg, &engine, &owner, &fact("target", "v1", "Present")).await?;
        let source_entity = memory_fact_entity_id(&pg, source.memory_id).await?;
        let target_entity = memory_fact_entity_id(&pg, target.memory_id).await?;
        let edge_id =
            append_follow_head_edge(&pg, &registry, &owner, source_entity, target_entity).await?;

        let history = pg
            .event_history(&EventHistoryRequest {
                principal: owner.principal.clone(),
                limit: 100,
                before: None,
            })
            .await?;
        let history_edge = history
            .events
            .iter()
            .find(|event| {
                matches!(
                    event.kind,
                    ChangeEventKind::EdgeAppend { edge_id: seen, .. } if seen == edge_id
                )
            })
            .expect("event_history fact-entity edge");
        assert!(matches!(
            history_edge.kind,
            ChangeEventKind::EdgeAppend {
                source: EntityRef::FactEntity(_),
                target: EntityRef::FactEntity(_),
                ..
            }
        ));

        let ctx = McpToolCtx {
            owner: owner.clone(),
            authz: AuthzContext::single_owner(&owner, AuthPath::System),
            handles: None,
            mode: OutputMode::RawIds,
            registry: Arc::new(registry),
            author: McpAuthorContext {
                model_id: "test".into(),
                client_name: "test".into(),
                client_version: "1".into(),
                personality_instance_id: None,
                caller_self_perspective: None,
            },
            caller_self_perspective: None,
            master_token_id: None,
            extensions: McpToolExtensions::with(pg.pool().clone()),
            engine: Some(engine),
        };
        let listed = ListEventsTool::call(
            ctx,
            ListEventsArgs {
                since: None,
                limit: Some(100),
            },
        )
        .await?;
        let edge_id_text = edge_id.to_string();
        let item = listed
            .events
            .iter()
            .find(|event| event.edge.as_deref() == Some(edge_id_text.as_str()))
            .expect("list_events fact-entity edge");
        let source_text = format!("fact_entity:{source_entity}");
        let target_text = format!("fact_entity:{target_entity}");
        assert_eq!(item.source.as_deref(), Some(source_text.as_str()));
        assert_eq!(item.target.as_deref(), Some(target_text.as_str()));
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn pin_relations_still_round_trip_memory_endpoints() -> Result<(), Box<dyn std::error::Error>>
{
    let (pg, db_name) = fresh_pg_with_sidecars().await;

    let result = async {
        pg.run_migrations().await?;
        create_sidecar(&pg).await?;
        let owner = owner_fixture();
        let registry = registry_for_test();
        let engine = engine_for(&pg, registry.clone());
        let source = ingest_fact(&pg, &engine, &owner, &fact("source", "v1", "Present")).await?;
        let target = ingest_fact(&pg, &engine, &owner, &fact("target", "v1", "Present")).await?;
        let edge_id = append_pinned_edge(
            &pg,
            &registry,
            &owner,
            source.memory_id.into_inner(),
            target.memory_id.into_inner(),
        )
        .await?;

        let raw: (Option<Uuid>, Option<Uuid>, Option<Uuid>, Option<Uuid>) = sqlx::query_as(
            "SELECT source_memory_id, source_fact_entity_id,
                    target_memory_id, target_fact_entity_id
               FROM proxima_core.edges
              WHERE edge_id = $1",
        )
        .bind(edge_id)
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(
            raw,
            (
                Some(source.memory_id.into_inner()),
                None,
                Some(target.memory_id.into_inner()),
                None
            )
        );

        let authz = AuthzContext::single_owner(&owner, AuthPath::System);
        let mut req = QueryRequest::for_principal(owner.principal.clone());
        req.limit = 100;
        let response = engine.query(&authz, &req).await?;
        let edge = response
            .edges
            .iter()
            .find(|edge| edge.id == edge_id)
            .expect("pin edge remains queryable");
        assert_eq!(edge.source, EntityRef::Memory(source.memory_id));
        assert_eq!(edge.target, EntityRef::Memory(target.memory_id));
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

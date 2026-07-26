//! Fact-entity edge write, graph resolution, and change-log coverage.

#![allow(clippy::too_many_lines)]

use proxima_core::FactReceiptDraft;
use std::sync::Arc;

use crate::common::{fresh_pg, owner_write_permit};
use proxima_core::engine::Engine;
use proxima_core::storage_ports::*;
use proxima_core::verbs::fact_ingest::FactWriteCommand;
use proxima_core::verbs::schema::PayloadKind;
use proxima_core::{
    AuthPath, AuthorshipKindMask, AuthzContext, EdgeAuthorshipKind, EdgeId, EndpointBinding,
    EntityKindMask, FactPayload, FactTombstone, FlavorRegistry, FlavorRegistryFrozen, MemoryId,
    Owner, PayloadKeyBuilder, Relation, RelationClass, RelationDescriptor, SchemaVersion,
    SidecarPayload, SourceBatchId, SourceId, StorageError, canonical_json_bytes,
};
use proxima_storage_pg::sidecars::{PgMemoryPayload, PgMemoryPayloadFuture};
use proxima_storage_pg::verbs::edge_write::{CheckedEdgeEndpoint, append_owner_checked_edge};
use proxima_storage_pg::verbs::fact_ingest::{FactIngestSidecarFuture, PgFactSidecar};
use proxima_storage_pg::{
    PgSidecarRegistry, PgSidecarRegistryFrozen, PgStorage, register_core_pg_sidecars,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

const TEST_FOLLOW_RELATION: &str = "test/follows-head";
const TEST_PIN_RELATION: &str = "test/pins-version";
const TEST_FOLLOW_ACTOR_RELATION: &str = "test/follows-actor-head";
const TEST_PIN_ACTOR_RELATION: &str = "test/pins-actor-version";
const CROSS_FLAVOR_ASSIGNED_TO_RELATION: &str = "test-flavor-a/assigned-to";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct StatefulFactV1 {
    entity_key: String,
    body: String,
    state: String,
}

impl FactPayload for StatefulFactV1 {
    const SCHEMA_ID: &'static str = "test/stateful-fact-v1";
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
    ) -> FactIngestSidecarFuture<'t>
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
        _ctx: proxima_storage_pg::sidecars::PgSidecarReadCtx<'_>,
        _memory_id: MemoryId,
    ) -> PgMemoryPayloadFuture<'_> {
        Box::pin(async { Ok(None) })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PlainFactV1 {
    entity_key: String,
    body: String,
    state: String,
}

impl FactPayload for PlainFactV1 {
    const SCHEMA_ID: &'static str = "test/plain-fact-v1";
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

impl PgFactSidecar for PlainFactV1 {
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

impl PgMemoryPayload for PlainFactV1 {
    fn load_memory_payload(
        _ctx: proxima_storage_pg::sidecars::PgSidecarReadCtx<'_>,
        _memory_id: MemoryId,
    ) -> PgMemoryPayloadFuture<'_> {
        Box::pin(async { Ok(None) })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CrossFlavorActorFactV1 {
    entity_key: String,
    body: String,
    state: String,
}

impl FactPayload for CrossFlavorActorFactV1 {
    const SCHEMA_ID: &'static str = "test-flavor-b/actor-fact-v1";
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

impl PgFactSidecar for CrossFlavorActorFactV1 {
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

impl PgMemoryPayload for CrossFlavorActorFactV1 {
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
    registry.add_fact_schema_or_panic_for_tests::<PlainFactV1>();
    registry.add_schema_capability_tags_or_panic_for_tests(
        PayloadKind::Fact,
        StatefulFactV1::schema_id(),
        SchemaVersion::new(StatefulFactV1::SCHEMA_VERSION),
        ["actor"],
    );
    registry.add_relation_or_panic_for_tests(RelationDescriptor::substrate(
        TEST_FOLLOW_RELATION,
        RelationClass::Structural,
        EndpointBinding::FollowHead,
        EndpointBinding::FollowHead,
        EntityKindMask::fact(),
        EntityKindMask::fact(),
        AuthorshipKindMask::source_ingest().union(AuthorshipKindMask::external_agent()),
    ));
    registry.add_relation_or_panic_for_tests(RelationDescriptor::substrate(
        TEST_PIN_RELATION,
        RelationClass::Structural,
        EndpointBinding::Pin,
        EndpointBinding::Pin,
        EntityKindMask::fact(),
        EntityKindMask::fact(),
        AuthorshipKindMask::source_ingest().union(AuthorshipKindMask::external_agent()),
    ));
    registry.add_relation_or_panic_for_tests(
        RelationDescriptor::substrate(
            TEST_FOLLOW_ACTOR_RELATION,
            RelationClass::Structural,
            EndpointBinding::FollowHead,
            EndpointBinding::FollowHead,
            EntityKindMask::fact(),
            EntityKindMask::fact(),
            AuthorshipKindMask::source_ingest().union(AuthorshipKindMask::external_agent()),
        )
        .with_required_tags(&["actor"], &["actor"]),
    );
    registry.add_relation_or_panic_for_tests(
        RelationDescriptor::substrate(
            TEST_PIN_ACTOR_RELATION,
            RelationClass::Structural,
            EndpointBinding::Pin,
            EndpointBinding::Pin,
            EntityKindMask::fact(),
            EntityKindMask::fact(),
            AuthorshipKindMask::source_ingest().union(AuthorshipKindMask::external_agent()),
        )
        .with_required_tags(&["actor"], &["actor"]),
    );
    registry.freeze_or_panic_for_tests()
}

fn pg_sidecars_for_test() -> PgSidecarRegistryFrozen {
    let registry = registry_for_test();
    let mut sidecars = PgSidecarRegistry::new();
    register_core_pg_sidecars(&mut sidecars);
    sidecars.add_fact::<StatefulFactV1>();
    sidecars.add_fact::<PlainFactV1>();
    sidecars
        .freeze_against(registry.schemas())
        .expect("test PG sidecars match test schemas")
}

mod cross_flavor_a {
    use super::*;

    proxima_core::proxima_flavor! {
        name = "test-flavor-a",
        relations = [
            RelationDescriptor::substrate(
                CROSS_FLAVOR_ASSIGNED_TO_RELATION,
                RelationClass::Structural,
                EndpointBinding::FollowHead,
                EndpointBinding::FollowHead,
                EntityKindMask::fact(),
                EntityKindMask::fact(),
                AuthorshipKindMask::external_agent(),
            ).with_required_tags(&[], &["actor"]),
        ],
    }
}

mod cross_flavor_b {
    use super::*;

    proxima_core::proxima_flavor! {
        name = "test-flavor-b",
        fact_schemas = [ CrossFlavorActorFactV1 ],
        schema_capability_tags = [
            (Fact, CrossFlavorActorFactV1) => ["actor"],
        ],
    }
}

trait TestFlavorBundle {
    fn register(registry: &mut FlavorRegistry) -> Result<(), proxima_core::FlavorRegistryError>;
}

impl<A: TestFlavorBundle, B: TestFlavorBundle> TestFlavorBundle for (A, B) {
    fn register(registry: &mut FlavorRegistry) -> Result<(), proxima_core::FlavorRegistryError> {
        A::register(registry)?;
        B::register(registry)
    }
}

struct CrossFlavorA;
struct CrossFlavorB;

impl TestFlavorBundle for CrossFlavorA {
    fn register(registry: &mut FlavorRegistry) -> Result<(), proxima_core::FlavorRegistryError> {
        cross_flavor_a::register(registry)
    }
}

impl TestFlavorBundle for CrossFlavorB {
    fn register(registry: &mut FlavorRegistry) -> Result<(), proxima_core::FlavorRegistryError> {
        cross_flavor_b::register(registry)
    }
}

fn cross_flavor_registry_for_test() -> FlavorRegistryFrozen {
    let mut registry = FlavorRegistry::new();
    <(CrossFlavorA, CrossFlavorB) as TestFlavorBundle>::register(&mut registry).unwrap();
    registry.try_freeze().unwrap()
}

fn cross_flavor_pg_sidecars_for_test() -> PgSidecarRegistryFrozen {
    let registry = cross_flavor_registry_for_test();
    let mut sidecars = PgSidecarRegistry::new();
    register_core_pg_sidecars(&mut sidecars);
    sidecars.add_fact::<CrossFlavorActorFactV1>();
    sidecars
        .freeze_against(registry.schemas())
        .expect("cross-flavor PG sidecars match test schemas")
}

async fn fresh_pg_with_sidecars() -> (PgStorage, String) {
    let (pg, db_name) = fresh_pg().await;
    (pg.with_sidecars(pg_sidecars_for_test()), db_name)
}

async fn fresh_pg_with_cross_flavor_sidecars() -> (PgStorage, String) {
    let (pg, db_name) = fresh_pg().await;
    (
        pg.with_sidecars(cross_flavor_pg_sidecars_for_test()),
        db_name,
    )
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
        // SQL-POLICY: fixed-fragment
        sqlx::query(sql).execute(pg.pool_for_tests()).await?;
    }
    Ok(())
}

fn engine_for(pg: &PgStorage, registry: FlavorRegistryFrozen) -> Engine {
    Engine::new(registry).with_storage_ports(Arc::new(pg.clone()).storage_ports())
}

fn fact(entity_key: &str, body: &str, state: &str) -> StatefulFactV1 {
    StatefulFactV1 {
        entity_key: entity_key.to_string(),
        body: body.to_string(),
        state: state.to_string(),
    }
}

fn plain_fact(entity_key: &str, body: &str, state: &str) -> PlainFactV1 {
    PlainFactV1 {
        entity_key: entity_key.to_string(),
        body: body.to_string(),
        state: state.to_string(),
    }
}

fn cross_actor_fact(entity_key: &str, body: &str, state: &str) -> CrossFlavorActorFactV1 {
    CrossFlavorActorFactV1 {
        entity_key: entity_key.to_string(),
        body: body.to_string(),
        state: state.to_string(),
    }
}

fn draft_for_payload<F: FactPayload>(_owner: &Owner, payload_value: &Value) -> FactWriteCommand {
    let now = time::OffsetDateTime::now_utc();
    FactWriteCommand {
        schema_id: F::schema_id(),
        schema_version: SchemaVersion::new(F::SCHEMA_VERSION),
        payload: canonical_json_bytes(payload_value),
        rendered_text: None,
        receipt: Some(FactReceiptDraft {
            source_id: SourceId::new(format!("test/fact-entity-edge/{}", Uuid::now_v7())),
            source_batch_id: SourceBatchId::new(Uuid::now_v7()),
            observed_at: now,
            occurred_at: now,
        }),
        citation: None,
    }
}

async fn ingest_fact(
    pg: &PgStorage,
    engine: &Engine,
    owner: &Owner,
    payload: &StatefulFactV1,
) -> Result<proxima_core::FactIngestOutcome, StorageError> {
    ingest_fact_payload(pg, engine, owner, payload).await
}

async fn ingest_fact_payload<F>(
    pg: &PgStorage,
    engine: &Engine,
    owner: &Owner,
    payload: &F,
) -> Result<proxima_core::FactIngestOutcome, StorageError>
where
    F: FactPayload + Clone,
{
    let payload_value =
        serde_json::to_value(payload).map_err(|err| StorageError::Internal(err.to_string()))?;
    let draft = draft_for_payload::<F>(owner, &payload_value);
    let authz = AuthzContext::single_owner(owner, AuthPath::HostBearer);
    let authorized = engine
        .authorize_fact_ingest(&authz, Relation::Ingest, draft)
        .await
        .map_err(|err| StorageError::Internal(err.to_string()))?;
    let sidecar_payload = SidecarPayload::fact(payload.clone());
    pg.ingest_fact_with_typed_sidecar(&authorized, &sidecar_payload, None)
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
    .fetch_one(pg.pool_for_tests())
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
    append_follow_head_edge_for_relation(
        pg,
        registry,
        owner,
        TEST_FOLLOW_RELATION,
        source_fact_entity_id,
        target_fact_entity_id,
    )
    .await
}

async fn append_follow_head_edge_for_relation(
    pg: &PgStorage,
    registry: &FlavorRegistryFrozen,
    owner: &Owner,
    relation_id: &str,
    source_fact_entity_id: Uuid,
    target_fact_entity_id: Uuid,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let relation = registry
        .resolve_relation(relation_id)
        .expect("follow relation");
    let edge_id = Uuid::now_v7();
    let permit = owner_write_permit(owner, proxima_core::AccessKind::Fact).await?;
    let mut tx = pg.pool_for_tests().begin().await?;
    append_owner_checked_edge(
        &mut tx,
        &permit,
        EdgeId::new(edge_id),
        relation,
        CheckedEdgeEndpoint::fact_entity(proxima_core::FactEntityId::new(source_fact_entity_id)),
        CheckedEdgeEndpoint::fact_entity(proxima_core::FactEntityId::new(target_fact_entity_id)),
        EdgeAuthorshipKind::ExternalAgent,
        None,
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
    append_pinned_edge_for_relation(
        pg,
        registry,
        owner,
        TEST_PIN_RELATION,
        source_memory_id,
        target_memory_id,
    )
    .await
}

async fn append_pinned_edge_for_relation(
    pg: &PgStorage,
    registry: &FlavorRegistryFrozen,
    owner: &Owner,
    relation_id: &str,
    source_memory_id: Uuid,
    target_memory_id: Uuid,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let relation = registry
        .resolve_relation(relation_id)
        .expect("pin relation");
    let edge_id = Uuid::now_v7();
    let permit = owner_write_permit(owner, proxima_core::AccessKind::Fact).await?;
    let mut tx = pg.pool_for_tests().begin().await?;
    append_owner_checked_edge(
        &mut tx,
        &permit,
        EdgeId::new(edge_id),
        relation,
        CheckedEdgeEndpoint::fact(MemoryId::new(source_memory_id)),
        CheckedEdgeEndpoint::fact(MemoryId::new(target_memory_id)),
        EdgeAuthorshipKind::ExternalAgent,
        None,
    )
    .await?;
    tx.commit().await?;
    Ok(edge_id)
}

async fn raw_insert_follow_edge(
    pg: &PgStorage,
    owner: &Owner,
    source_fact_entity_id: Uuid,
    target_fact_entity_id: Uuid,
) -> Result<(), sqlx::Error> {
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(owner);
    sqlx::query(
        "INSERT INTO proxima_core.edges
            (edge_id, owner_kind, owner_id, relation, relation_class,
             source_kind, source_fact_entity_id,
             target_kind, target_fact_entity_id,
             authorship_kind, authorship_owner_memory_id)
         VALUES ($1, $2, $3, 'test/raw-follow', 'Structural',
                 'Fact', $4,
                 'Fact', $5,
                 'ExternalAgent', NULL)",
    )
    .bind(Uuid::now_v7())
    .bind(owner_kind)
    .bind(owner_id)
    .bind(source_fact_entity_id)
    .bind(target_fact_entity_id)
    .execute(pg.pool_for_tests())
    .await?;
    Ok(())
}

mod endpoints;
mod follow_head;
mod required_tags;

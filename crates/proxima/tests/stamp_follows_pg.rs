//! A1 (#474): `owner: follows` stamps `owner_id` at insert and rehomes on transfer.

#[path = "fixtures/example_engine.rs"]
#[allow(dead_code, reason = "each suite uses part of the shared fixture")]
mod example_engine;

use example_engine::ExampleEngine;
use proxima::{FactPayload, FactWrite};
use proxima_core::{
    AuthPath, AuthzContext, EntityId, GroupId, OwnerId, Role, UserId,
    test_fixtures::authenticated_context,
};
use proxima_example::StampV1;
use uuid::Uuid;

const STAMP_OWNER_SQL: &str = "SELECT owner_id FROM proxima_example.stamp_v1 WHERE t = $1";
const MEMORY_OWNER_SQL: &str = "SELECT owner_id FROM proxima_core.memory WHERE t = $1";

#[tokio::test]
async fn stamp_row_carries_memory_owner_after_insert() {
    let harness = ExampleEngine::new("proxima_stamp_follows_insert", |_| {}).await;
    let user = UserId::new(Uuid::now_v7());
    let owner = OwnerId::from(user);
    let authz = authenticated_context(AuthzContext::for_subject(user, AuthPath::HostBearer));
    let stamp = StampV1 {
        mark: "insert-stamp".to_owned(),
    };
    let outcome = harness
        .engine
        .ingest_fact(&authz, FactWrite::new(owner, StampV1::SCHEMA_ID, &stamp))
        .await
        .expect("stamp is remembered");
    let memory_id = outcome.memory_id;
    let stamp_owner: Uuid = sqlx::query_scalar(STAMP_OWNER_SQL)
        .bind(memory_id.into_inner())
        .fetch_one(&harness.pool)
        .await
        .expect("stamp row");
    let memory_owner: Uuid = sqlx::query_scalar(MEMORY_OWNER_SQL)
        .bind(memory_id.into_inner())
        .fetch_one(&harness.pool)
        .await
        .expect("memory row");
    assert_eq!(stamp_owner, memory_owner);
    assert_eq!(stamp_owner, owner.into_inner());
}

#[tokio::test]
async fn stamp_row_carries_destination_owner_after_core_transfer() {
    let harness = ExampleEngine::new("proxima_stamp_follows_transfer", |_| {}).await;
    let author = UserId::new(Uuid::now_v7());
    let source = OwnerId::from(author);
    let destination = OwnerId::from(GroupId::new(Uuid::now_v7()));
    let write_authz =
        authenticated_context(AuthzContext::for_subject(author, AuthPath::HostBearer));
    let stamp = StampV1 {
        mark: "transfer-stamp".to_owned(),
    };
    let outcome = harness
        .engine
        .ingest_fact(
            &write_authz,
            FactWrite::new(source, StampV1::SCHEMA_ID, &stamp),
        )
        .await
        .expect("stamp is remembered");
    let memory_id = outcome.memory_id;

    let transfer_authz = authenticated_context(AuthzContext::for_subject_with_role(
        author,
        [(destination, Role::admin())],
        AuthPath::HostBearer,
    ));
    harness
        .engine
        .transfer_to_owner(&transfer_authz, EntityId::Memory(memory_id), destination)
        .await
        .expect("transfer moves the stamp with the memory");

    let stamp_owner: Uuid = sqlx::query_scalar(STAMP_OWNER_SQL)
        .bind(memory_id.into_inner())
        .fetch_one(&harness.pool)
        .await
        .expect("stamp row after transfer");
    let memory_owner: Uuid = sqlx::query_scalar(MEMORY_OWNER_SQL)
        .bind(memory_id.into_inner())
        .fetch_one(&harness.pool)
        .await
        .expect("memory row after transfer");
    assert_eq!(stamp_owner, memory_owner);
    assert_eq!(stamp_owner, destination.into_inner());
}

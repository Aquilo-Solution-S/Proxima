//! A1 (#474): `owner: follows` stamps `owner_id` at insert and rehomes on transfer.

#[path = "fixtures/example_engine.rs"]
#[allow(dead_code, reason = "each suite uses part of the shared fixture")]
mod example_engine;

use example_engine::ExampleEngine;
use proxima::{FactPayload, FactWrite};
use proxima_core::{
    AuthPath, AuthzContext, EntityId, GroupId, OwnerId, Role, UserId,
    test_fixtures::{authenticated_context, owner_scope_for_tests},
};
use proxima_example::StampV1;
use uuid::Uuid;

const STAMP_OWNER_SQL: &str = "SELECT owner_id FROM proxima_example.stamp_v1 WHERE t = $1";
const MEMORY_OWNER_SQL: &str = "SELECT owner_id FROM proxima_core.memory WHERE t = $1";
const MEMORY_COUNT_SQL: &str = "SELECT count(*) FROM proxima_core.memory WHERE t = $1";
const STAMP_COUNT_SQL: &str = "SELECT count(*) FROM proxima_example.stamp_v1 WHERE t = $1";

/// A runtime-role transaction scoped to `owner`: owner RLS decides what it
/// sees, not the query. Callers roll it back.
async fn owner_tx(
    runtime: &sqlx::PgPool,
    owner: OwnerId,
) -> sqlx::Transaction<'static, sqlx::Postgres> {
    proxima_storage_pg::begin_owner_transaction(runtime, &owner_scope_for_tests(&[owner]))
        .await
        .expect("owner transaction")
}

/// How many memory rows `t` the `owner` scope sees.
async fn memories_seen_by(runtime: &sqlx::PgPool, owner: OwnerId, t: Uuid) -> i64 {
    let mut tx = owner_tx(runtime, owner).await;
    let seen: i64 = sqlx::query_scalar(MEMORY_COUNT_SQL)
        .bind(t)
        .fetch_one(&mut *tx)
        .await
        .expect("memory count under owner RLS");
    tx.rollback().await.expect("rollback");
    seen
}

/// How many stamp rows for memory `t` the `owner` scope sees.
async fn stamps_seen_by(runtime: &sqlx::PgPool, owner: OwnerId, t: Uuid) -> i64 {
    let mut tx = owner_tx(runtime, owner).await;
    let seen: i64 = sqlx::query_scalar(STAMP_COUNT_SQL)
        .bind(t)
        .fetch_one(&mut *tx)
        .await
        .expect("stamp count under owner RLS");
    tx.rollback().await.expect("rollback");
    seen
}

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

#[tokio::test]
async fn stamp_row_is_visible_to_its_owner_scope_only_across_transfer_forget_and_restore() {
    let harness = ExampleEngine::new("proxima_stamp_follows_rls", |_| {}).await;
    let runtime = sqlx::PgPool::connect(harness.db.runtime_url())
        .await
        .expect("runtime connection");
    let author = UserId::new(Uuid::now_v7());
    let source = OwnerId::from(author);
    let destination = OwnerId::from(GroupId::new(Uuid::now_v7()));
    let write_authz =
        authenticated_context(AuthzContext::for_subject(author, AuthPath::HostBearer));
    let stamp = StampV1 {
        mark: "rls-stamp".to_owned(),
    };
    let memory_id = harness
        .engine
        .ingest_fact(
            &write_authz,
            FactWrite::new(source, StampV1::SCHEMA_ID, &stamp),
        )
        .await
        .expect("stamp is remembered")
        .memory_id;
    let t = memory_id.into_inner();

    assert_eq!(
        stamps_seen_by(&runtime, source, t).await,
        1,
        "source sees its stamp"
    );
    assert_eq!(
        stamps_seen_by(&runtime, destination, t).await,
        0,
        "destination sees nothing before transfer"
    );

    let dest_authz = authenticated_context(AuthzContext::for_subject_with_role(
        author,
        [(destination, Role::admin())],
        AuthPath::HostBearer,
    ));
    harness
        .engine
        .transfer_to_owner(&dest_authz, EntityId::Memory(memory_id), destination)
        .await
        .expect("transfer moves the stamp with the memory");
    assert_eq!(
        stamps_seen_by(&runtime, destination, t).await,
        1,
        "destination sees the stamp after transfer"
    );
    assert_eq!(
        stamps_seen_by(&runtime, source, t).await,
        0,
        "source no longer sees the stamp after transfer"
    );

    harness
        .engine
        .forget_memory(&dest_authz, destination, memory_id)
        .await
        .expect("destination forgets the memory");
    // #488: forget is a state. `DumpThenDelete` resolves to
    // `ForgetLeg::Marked`, so the stamp row stays with the forgotten memory,
    // still carrying the destination's owner_id, while the memory itself is
    // hidden from the owner scope. Only erase walks sidecar rows.
    let after_forget: Uuid = sqlx::query_scalar(STAMP_OWNER_SQL)
        .bind(t)
        .fetch_one(&harness.pool)
        .await
        .expect("forget keeps the stamp row with the memory");
    assert_eq!(after_forget, destination.into_inner());
    assert_eq!(
        memories_seen_by(&runtime, destination, t).await,
        0,
        "the forgotten memory is hidden from its owner scope"
    );

    harness
        .engine
        .restore_memory(&dest_authz, destination, memory_id)
        .await
        .expect("destination restores the memory");
    let stamp_owner: Uuid = sqlx::query_scalar(STAMP_OWNER_SQL)
        .bind(t)
        .fetch_one(&harness.pool)
        .await
        .expect("stamp row after restore");
    let memory_owner: Uuid = sqlx::query_scalar(MEMORY_OWNER_SQL)
        .bind(t)
        .fetch_one(&harness.pool)
        .await
        .expect("memory row after restore");
    assert_eq!(stamp_owner, destination.into_inner());
    assert_eq!(stamp_owner, memory_owner);
    assert_eq!(
        stamps_seen_by(&runtime, destination, t).await,
        1,
        "destination sees the restored stamp"
    );
    assert_eq!(
        stamps_seen_by(&runtime, source, t).await,
        0,
        "source does not see the restored stamp"
    );
}

//! A Spiice schema's `NotTransferable` is refused where its `enforced_by`
//! says: by the engine, and by the storage transfer for a caller that skips
//! the engine. The sealed stamp shares the stamp sidecar, whose rows follow a
//! transfer, so only the schema's declaration keeps it with its owner.

#[path = "fixtures/example_engine.rs"]
#[allow(dead_code, reason = "each suite uses part of the shared fixture")]
mod example_engine;

use example_engine::{ExampleEngine, storage_over};
use proxiima::SpiiceBundle as _;
use proxiima::spiice::SpiiceRegistry;
use proxiima::{FactPayload, FactWrite};
use proxiima_core::error::ErrorCode;
use proxiima_core::owner_inverse::OwnerSurfaces;
use proxiima_core::storage_ports::{OwnerTransferPort, OwnerWritePermit};
use proxiima_core::test_fixtures::authenticated_context;
use proxiima_core::{
    AccessKind, AuthPath, AuthzContext, EntityId, GroupId, MemoryId, OwnerId, Role, StorageError,
    UserId,
};
use proxiima_example::{ExampleSpiice, SealedStampV1};
use uuid::Uuid;

const MEMORY_OWNER_SQL: &str = "SELECT owner_id FROM proxiima_core.memory WHERE t = $1";
const HEAD_OWNER_SQL: &str = "SELECT owner_id FROM proxiima_core.memory_head WHERE t = $1";
const STAMP_OWNER_SQL: &str = "SELECT owner_id FROM proxiima_example.stamp_v1 WHERE t = $1";

/// A sealed stamp written by `author` under their own owner, and the group a
/// transfer would name.
struct Sealed {
    author: UserId,
    source: OwnerId,
    destination: OwnerId,
    memory_id: MemoryId,
}

async fn remember_sealed_as(
    harness: &ExampleEngine,
    write_authz: &AuthzContext,
    owner: OwnerId,
) -> MemoryId {
    let sealed = SealedStampV1 {
        mark: "sealed-mark".to_owned(),
    };
    harness
        .engine
        .ingest_fact(
            write_authz,
            FactWrite::new(owner, SealedStampV1::SCHEMA_ID, &sealed),
        )
        .await
        .expect("the sealed stamp is remembered")
        .memory_id
}

async fn remember_sealed(harness: &ExampleEngine) -> Sealed {
    let author = UserId::new(Uuid::now_v7());
    let source = OwnerId::from(author);
    let write_authz =
        authenticated_context(AuthzContext::for_subject(author, AuthPath::HostBearer));
    Sealed {
        author,
        source,
        destination: OwnerId::from(GroupId::new(Uuid::now_v7())),
        memory_id: remember_sealed_as(harness, &write_authz, source).await,
    }
}

/// Every row a transfer would re-home still carries `source`: the memory, its
/// head and the shared stamp sidecar row.
async fn assert_stays_with_source(harness: &ExampleEngine, sealed: &Sealed) {
    let t = sealed.memory_id.into_inner();
    let memory: Uuid = sqlx::query_scalar(MEMORY_OWNER_SQL)
        .bind(t)
        .fetch_one(&harness.pool)
        .await
        .expect("the memory row");
    let head: Uuid = sqlx::query_scalar(HEAD_OWNER_SQL)
        .bind(t)
        .fetch_one(&harness.pool)
        .await
        .expect("the head row");
    let stamp: Uuid = sqlx::query_scalar(STAMP_OWNER_SQL)
        .bind(t)
        .fetch_one(&harness.pool)
        .await
        .expect("the shared stamp sidecar row");
    assert_eq!([memory, head, stamp], [sealed.source.into_inner(); 3]);
}

#[tokio::test]
async fn the_engine_refuses_a_sealed_stamp_with_the_schemas_reason() {
    let harness = ExampleEngine::new("proxiima_not_transferable_engine", |_| {}).await;
    let sealed = remember_sealed(&harness).await;
    let transfer_authz = authenticated_context(AuthzContext::for_subject_with_role(
        sealed.author,
        [(sealed.destination, Role::admin())],
        AuthPath::HostBearer,
    ));

    let err = harness
        .engine
        .transfer_to_owner(
            &transfer_authz,
            EntityId::Memory(sealed.memory_id),
            sealed.destination,
        )
        .await
        .expect_err("a NotTransferable schema's memory does not transfer");

    // Storage would refuse too, as `Internal`; the engine's own typed
    // refusal is what answers first.
    assert_eq!(err.code, ErrorCode::InvalidArgument);
    assert!(
        err.message.contains("proxiima-example/sealed-stamp-v1")
            && err.message.contains("a sealed stamp cannot change owner"),
        "the refusal names the schema and its declared reason: {}",
        err.message
    );
    assert_stays_with_source(&harness, &sealed).await;
}

#[tokio::test]
async fn the_storage_transfer_refuses_a_sealed_stamp_without_the_engine() {
    let harness = ExampleEngine::new("proxiima_not_transferable_storage", |_| {}).await;
    let sealed = remember_sealed(&harness).await;
    let mut registry = SpiiceRegistry::new();
    ExampleSpiice::register(&mut registry).expect("the example spiice registers");
    let registry = registry.freeze_or_panic_for_tests();
    let pg = storage_over(
        harness.db.runtime_url(),
        harness.db.platform_url(),
        &registry,
    )
    .await;
    let permit =
        OwnerWritePermit::transfer_for_tests(sealed.source, AccessKind::Fact, sealed.destination);

    let err = pg
        .transfer_to_owner(
            &permit,
            EntityId::Memory(sealed.memory_id),
            sealed.destination,
            &OwnerSurfaces::for_registry(&registry),
            &[],
        )
        .await
        .expect_err("the backstop refuses a caller that skips the engine");

    assert!(
        matches!(&err, StorageError::ConstraintViolation(message)
            if message.contains("proxiima-example/sealed-stamp-v1")
                && message.contains("a sealed stamp cannot change owner")),
        "the backstop refuses with the schema's declared reason: {err:?}"
    );
    assert_stays_with_source(&harness, &sealed).await;
}

#[tokio::test]
async fn a_caller_who_may_not_manage_the_destination_is_not_told_the_schema() {
    let harness = ExampleEngine::new("proxiima_not_transferable_forbidden", |_| {}).await;
    let sealed = remember_sealed(&harness).await;
    let no_destination_role = authenticated_context(AuthzContext::for_subject(
        sealed.author,
        AuthPath::HostBearer,
    ));

    let err = harness
        .engine
        .transfer_to_owner(
            &no_destination_role,
            EntityId::Memory(sealed.memory_id),
            sealed.destination,
        )
        .await
        .expect_err("the destination did not consent");

    assert_eq!(err.code, ErrorCode::Forbidden);
    assert!(
        !err.message.contains("sealed-stamp"),
        "the refusal names no schema: {}",
        err.message
    );
}

#[tokio::test]
async fn a_caller_who_may_not_manage_the_source_is_not_told_the_schema() {
    let harness = ExampleEngine::new("proxiima_not_transferable_viewer", |_| {}).await;
    let writer = UserId::new(Uuid::now_v7());
    let group = OwnerId::from(GroupId::new(Uuid::now_v7()));
    let destination = OwnerId::from(GroupId::new(Uuid::now_v7()));
    let write_authz = authenticated_context(AuthzContext::for_subject_with_role(
        writer,
        [(group, Role::admin())],
        AuthPath::HostBearer,
    ));
    let memory_id = remember_sealed_as(&harness, &write_authz, group).await;
    // Reads the group's facts, manages nothing: the most a refusal that
    // names the schema could leak to.
    let viewer = authenticated_context(AuthzContext::for_subject_with_role(
        UserId::new(Uuid::now_v7()),
        [(group, Role::viewer()), (destination, Role::admin())],
        AuthPath::HostBearer,
    ));

    let err = harness
        .engine
        .transfer_to_owner(&viewer, EntityId::Memory(memory_id), destination)
        .await
        .expect_err("a viewer does not manage the source");

    assert_eq!(err.code, ErrorCode::Forbidden);
    assert!(
        !err.message.contains("sealed-stamp"),
        "the refusal names no schema: {}",
        err.message
    );
}

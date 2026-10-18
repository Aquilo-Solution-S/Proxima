//! #488 S5: owner reads exclude forgotten memories; the lifecycle scope sees
//! them. `forgotten_at` is set by raw SQL here — no verb sets it yet.

use proxima_core::storage_ports::{MemoryInspectPort, MemoryReadPort, OwnerWritePermit};
use proxima_core::test_fixtures::owner_scope_for_tests;
use proxima_core::verbs::query::{MemoryLineageDirection, MemoryLineageRequest};
use proxima_core::{AccessKind, OwnerId, UserId};
use sqlx::Connection;
use uuid::Uuid;

use super::PgLifecycleScope;
use crate::owner_scope::bind_owner_scope;
use crate::test_fixtures::fresh_pg;
use crate::verbs::forget::forget_pg_tests::{draft, ingest_stamped};

const AGENT_NOTE: &str = "proxima_core.agent_note_v1";

fn schemas() -> Vec<proxima_core::read_models::MemorySchemaSpec> {
    [
        proxima_core::EntityKind::Fact,
        proxima_core::EntityKind::Abstraction,
    ]
    .into_iter()
    .map(|kind| proxima_core::read_models::MemorySchemaSpec {
        kind,
        schema_id: proxima_core::SchemaId::new("core/test-fact-v1".to_owned()),
        schema_version: proxima_core::SchemaVersion::new(1),
        sidecar_table: None,
        body_field: None,
        tag_field: None,
    })
    .collect()
}

struct Seeded {
    parent: Uuid,
    child: Uuid,
}

/// A live Fact and an Abstraction derived from it that carries an agent note.
async fn seed(pg: &crate::PgStorage, permit: &OwnerWritePermit) -> Seeded {
    let parent = ingest_stamped(pg.pool_for_tests(), permit, &draft(None), &[], &[])
        .await
        .expect("parent")
        .memory_id
        .into_inner();
    // Lineage runs on origins: an Abstraction derived from the parent.
    let mut cites = draft(None);
    cites.kind = "abstraction".into();
    cites.additional_references = vec![proxima_core::EdgeEndpoint::memory(
        proxima_core::EntityKind::Fact,
        proxima_core::MemoryId::new(parent),
    )];
    let derive = OwnerWritePermit::new_for_tests(*permit.owner(), AccessKind::Abstraction);
    let child = ingest_stamped(
        pg.pool_for_tests(),
        &derive,
        &cites,
        &[AGENT_NOTE.to_owned()],
        &[
            "INSERT INTO proxima_core.agent_note_v1 (t, note_id, title, body, tags)
           VALUES ($1, $1, 'n', 'kept', ARRAY['tag'])",
        ],
    )
    .await
    .expect("child")
    .memory_id
    .into_inner();
    Seeded { parent, child }
}

async fn forget_by_hand(pg: &crate::PgStorage, t: Uuid) {
    sqlx::query("UPDATE proxima_core.memory SET forgotten_at = now() WHERE t = $1")
        .bind(t)
        .execute(pg.pool_for_tests())
        .await
        .expect("set forgotten_at");
}

/// What one transaction sees of a memory: its row and its agent notes.
async fn sees(connection: &mut sqlx::PgConnection, t: Uuid) -> (i64, i64) {
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM proxima_core.memory WHERE t = $1")
        .bind(t)
        .fetch_one(&mut *connection)
        .await
        .expect("memory read");
    let notes: i64 =
        sqlx::query_scalar("SELECT count(*) FROM proxima_core.agent_note_v1 WHERE t = $1")
            .bind(t)
            .fetch_one(&mut *connection)
            .await
            .expect("sidecar read");
    (rows, notes)
}

async fn walk_from(
    pg: &crate::PgStorage,
    scope: &proxima_core::OwnerScope,
    owner: OwnerId,
    start: Uuid,
) -> Vec<Uuid> {
    let response = pg
        .walk_memory_lineage(
            Some(scope),
            &[owner],
            &MemoryLineageRequest {
                owner,
                start_memory_id: proxima_core::MemoryId::new(start),
                direction: MemoryLineageDirection::Descendants,
                depth: 2,
                limit: 50,
                after: None,
            },
        )
        .await
        .expect("walk");
    response
        .nodes
        .iter()
        .map(|node| node.memory_id.into_inner())
        .collect()
}

#[tokio::test]
async fn a_forgotten_memory_is_absent_from_owner_reads() {
    let (pg, _guard) = fresh_pg("s5_absent").await;
    let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
    let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
    let scope = owner_scope_for_tests(&[owner]);
    let Seeded { parent, child } = seed(&pg, &permit).await;

    // Control: before the flag, every read sees the child.
    assert!(
        pg.load_memory_by_id(Some(&scope), proxima_core::MemoryId::new(child), &schemas())
            .await
            .expect("by id")
            .is_some()
    );
    assert!(walk_from(&pg, &scope, owner, parent).await.contains(&child));

    forget_by_hand(&pg, child).await;

    assert!(
        pg.load_memory_by_id(Some(&scope), proxima_core::MemoryId::new(child), &schemas())
            .await
            .expect("by id")
            .is_none(),
        "by-id read must not find a forgotten memory"
    );
    assert!(
        !walk_from(&pg, &scope, owner, parent).await.contains(&child),
        "the walk must not reach a forgotten memory"
    );
    let mut tx = crate::owner_scope::begin_owner_transaction(&pg.pool, &scope)
        .await
        .expect("owner tx");
    assert_eq!(
        sees(&mut tx, child).await,
        (0, 0),
        "row and sidecar are hidden"
    );
    assert_eq!(
        sees(&mut tx, parent).await.0,
        1,
        "a live memory is unaffected"
    );
    tx.rollback().await.expect("rollback");
}

#[tokio::test]
async fn the_lifecycle_scope_sees_a_forgotten_memory_and_does_not_leak() {
    let (pg, _guard) = fresh_pg("s5_lifecycle").await;
    let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
    let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
    let scope = owner_scope_for_tests(&[owner]);
    let Seeded { child, .. } = seed(&pg, &permit).await;
    forget_by_hand(&pg, child).await;

    let (_lifecycle, mut tx) = PgLifecycleScope::begin(&pg.pool, &scope)
        .await
        .expect("lifecycle tx");
    assert_eq!(
        sees(&mut tx, child).await,
        (1, 1),
        "lifecycle sees row and sidecar"
    );
    tx.commit().await.expect("commit");

    // The setting is transaction-local: the next owner transaction on the
    // same connection is back to hiding the row.
    let mut connection = pg.pool.acquire().await.expect("connection");
    let mut tx = connection.begin().await.expect("begin");
    bind_owner_scope(&mut tx, &scope).await.expect("bind");
    PgLifecycleScope::enter(&mut tx).await.expect("enter");
    assert_eq!(sees(&mut tx, child).await, (1, 1));
    tx.commit().await.expect("commit");
    let mut tx = connection.begin().await.expect("begin");
    bind_owner_scope(&mut tx, &scope).await.expect("bind");
    assert_eq!(sees(&mut tx, child).await, (0, 0), "lifecycle scope leaked");
    tx.rollback().await.expect("rollback");
}

#[tokio::test]
async fn another_owner_does_not_see_a_forgotten_memory_in_lifecycle_scope() {
    let (pg, _guard) = fresh_pg("s5_other_owner").await;
    let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
    let stranger = OwnerId::from(UserId::new(Uuid::now_v7()));
    let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
    let Seeded { child, .. } = seed(&pg, &permit).await;
    forget_by_hand(&pg, child).await;
    let (_lifecycle, mut tx) =
        PgLifecycleScope::begin(&pg.pool, &owner_scope_for_tests(&[stranger]))
            .await
            .expect("lifecycle tx");
    assert_eq!(
        sees(&mut tx, child).await,
        (0, 0),
        "lifecycle widens forgotten, not owners"
    );
    tx.rollback().await.expect("rollback");
}

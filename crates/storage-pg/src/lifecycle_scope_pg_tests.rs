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
    let _lifecycle = PgLifecycleScope::enter(&mut tx).await.expect("enter");
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

// #488 S7: erase and transfer already run in platform scope, so they see a
// forgotten memory as the row it is. These pin that before forget starts
// setting the flag (S10).

/// A standalone Fact with an agent note, forgotten by hand.
async fn forgotten_note(pg: &crate::PgStorage, owner: OwnerId) -> Uuid {
    let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
    let t = ingest_stamped(
        pg.pool_for_tests(),
        &permit,
        &draft(None),
        &[AGENT_NOTE.to_owned()],
        &[
            "INSERT INTO proxima_core.agent_note_v1 (t, note_id, title, body, tags)
           VALUES ($1, $1, 'n', 'forgotten', ARRAY['tag'])",
        ],
    )
    .await
    .expect("note")
    .memory_id
    .into_inner();
    forget_by_hand(pg, t).await;
    t
}

/// `(memory rows, agent notes)` for `t`, read past every policy.
async fn stored(pg: &crate::PgStorage, t: Uuid) -> (i64, i64) {
    let mut connection = pg.pool_for_tests().acquire().await.expect("admin");
    sees(&mut connection, t).await
}

fn surfaces() -> proxima_core::owner_inverse::OwnerSurfaces {
    proxima_core::owner_inverse::OwnerSurfaces::for_registry(
        &proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests(),
    )
}

#[tokio::test]
async fn an_owner_erase_removes_a_forgotten_memory_and_its_sidecar() {
    use proxima_core::owner_inverse::{EraseAuthorization, OwnerEraseOutcome, OwnerEraseTarget};
    use proxima_core::storage_ports::OwnerInversePort;
    let (pg, _guard) = fresh_pg("s7_owner_erase").await;
    let user = UserId::new(Uuid::now_v7());
    let t = forgotten_note(&pg, OwnerId::from(user)).await;
    assert_eq!(stored(&pg, t).await, (1, 1));
    let auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalOwner {
        user_id: user,
        drop_event_id: "s7-forgotten".into(),
    });
    let outcome = pg
        .erase_personal_owner(&auth, user, &surfaces())
        .await
        .expect("erase");
    assert!(
        matches!(outcome, OwnerEraseOutcome::Completed { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        stored(&pg, t).await,
        (0, 0),
        "no row and no sidecar survive"
    );
}

#[tokio::test]
async fn a_series_erase_selects_a_forgotten_version() {
    let (pg, _guard) = fresh_pg("s7_series_erase").await;
    let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
    let t = forgotten_note(&pg, owner).await;
    let mut tx = pg.pool_for_tests().begin().await.expect("tx");
    let (erased, _) = crate::verbs::forget::erase_memory_series(
        &mut tx,
        &crate::core_pg_sidecars(),
        &crate::PgHostStateEraseContext::for_surfaces_for_tests(surfaces())
            .expect("no host lifecycle tables"),
        &owner,
        &[t],
    )
    .await
    .expect("series erase");
    tx.commit().await.expect("commit");
    assert_eq!(erased, 1, "the forgotten version is selected");
    assert_eq!(stored(&pg, t).await, (0, 0));
}

#[tokio::test]
async fn a_transfer_moves_a_forgotten_memory_and_keeps_the_flag() {
    use proxima_core::storage_ports::OwnerTransferPort;
    let (pg, _guard) = fresh_pg("s7_transfer").await;
    let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
    let dest = OwnerId::from(UserId::new(Uuid::now_v7()));
    sqlx::query("INSERT INTO proxima_core.owners (owner_id, kind) VALUES ($1, 'personal') ON CONFLICT DO NOTHING")
        .bind(dest.into_inner())
        .execute(pg.pool_for_tests())
        .await
        .expect("destination owner");
    let t = forgotten_note(&pg, owner).await;
    let permit = OwnerWritePermit::transfer_for_tests(owner, AccessKind::Fact, dest);
    assert!(
        pg.transfer_to_owner(
            &permit,
            proxima_core::EntityId::Memory(proxima_core::MemoryId::new(t)),
            dest,
            &surfaces(),
            &[],
        )
        .await
        .expect("transfer"),
        "the forgotten memory is moved"
    );
    let (moved_to, still_forgotten): (Uuid, bool) = sqlx::query_as(
        "SELECT owner_id, forgotten_at IS NOT NULL FROM proxima_core.memory WHERE t = $1",
    )
    .bind(t)
    .fetch_one(pg.pool_for_tests())
    .await
    .expect("moved row");
    assert_eq!(moved_to, dest.into_inner());
    assert!(still_forgotten, "transfer keeps the flag");
    assert_eq!(stored(&pg, t).await, (1, 1), "the sidecar moves with it");
}

// #488 S8: export lists forgotten rows; replays see them.

#[tokio::test]
async fn export_lists_a_forgotten_memory_with_its_forgotten_at() {
    use proxima_core::owner_inverse::{ExportAuthorization, OwnerExportTarget};
    use proxima_core::storage_ports::OwnerInversePort;
    let (pg, _guard) = fresh_pg("s8_export").await;
    let user = UserId::new(Uuid::now_v7());
    let owner = OwnerId::from(user);
    let live = ingest_stamped(
        pg.pool_for_tests(),
        &OwnerWritePermit::new_for_tests(owner, AccessKind::Fact),
        &draft(None),
        &[],
        &[],
    )
    .await
    .expect("live")
    .memory_id
    .into_inner();
    let forgotten = forgotten_note(&pg, owner).await;
    let auth =
        ExportAuthorization::new_for_tests(OwnerExportTarget::PersonalOwner { user_id: user });
    let bundle = pg
        .export_owner_bundle(&auth, &surfaces())
        .await
        .expect("export");
    let row = |t: Uuid| {
        bundle
            .table("proxima_core.memory")
            .iter()
            .find(|row| row["t"].as_str() == Some(t.to_string().as_str()))
            .cloned()
            .unwrap_or_else(|| panic!("memory {t} missing from the export"))
    };
    assert!(
        row(forgotten)["forgotten_at"].is_string(),
        "forgotten row carries forgotten_at"
    );
    assert!(
        row(live)["forgotten_at"].is_null(),
        "a live row carries null"
    );
    assert_eq!(
        bundle
            .table(AGENT_NOTE)
            .iter()
            .filter(|note| note["t"].as_str() == Some(forgotten.to_string().as_str()))
            .count(),
        1,
        "the forgotten memory's sidecar is exported"
    );
}

#[tokio::test]
async fn replaying_a_forgotten_write_answers_its_handle_and_writes_nothing() {
    use proxima_core::storage_ports::FactIngestPort;
    let (pg, _guard) = fresh_pg("s8_replay").await;
    let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
    let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
    let write = draft(Some(("s8-source", "s8-key")));
    let first = pg
        .ingest_fact_atomic(&permit, &write, None)
        .await
        .expect("first");
    assert!(!first.forgotten);
    forget_by_hand(&pg, first.memory_id.into_inner()).await;

    let replay = pg
        .ingest_fact_atomic(&permit, &write, None)
        .await
        .expect("replay");
    assert!(replay.idempotent_replay, "the key replays");
    assert!(replay.forgotten, "the replay says the memory is forgotten");
    assert_eq!(replay.memory_id, first.memory_id);
    assert_eq!(replay.handle, first.handle);
    let (rows, still_forgotten): (i64, bool) = sqlx::query_as(
        "SELECT count(*), bool_and(forgotten_at IS NOT NULL)
           FROM proxima_core.memory WHERE owner_id = $1",
    )
    .bind(owner.into_inner())
    .fetch_one(pg.pool_for_tests())
    .await
    .expect("rows");
    assert_eq!(rows, 1, "no row was written");
    assert!(still_forgotten, "the replay does not clear the flag");
}

// #488 review r1 finding 3: a lifecycle verb that fails after entering the
// scope must not leave it on for the rest of the transaction. A unit of
// work that tolerates the error keeps reading and writing in that same
// transaction, which would then see forgotten rows.

/// An owner transaction with a forgotten note `t` in it, plus a `t` that
/// names nothing.
async fn failing_lifecycle_fixture(
    name: &str,
) -> (
    crate::PgStorage,
    impl Drop,
    OwnerId,
    Uuid,
    sqlx::Transaction<'static, sqlx::Postgres>,
) {
    let (pg, guard) = fresh_pg(name).await;
    let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
    let t = forgotten_note(&pg, owner).await;
    let tx =
        crate::owner_scope::begin_owner_transaction(&pg.pool, &owner_scope_for_tests(&[owner]))
            .await
            .expect("owner tx");
    (pg, guard, owner, t, tx)
}

#[tokio::test]
async fn a_forget_that_loses_the_race_leaves_the_scope() {
    let (_pg, _guard, owner, t, mut tx) = failing_lifecycle_fixture("r1_forget_race").await;
    assert_eq!(sees(&mut tx, t).await, (0, 0), "owner scope hides it");
    // `t` is already forgotten: the concurrent forget won. The probe sees
    // it in the lifecycle scope, the live lock does not, so NotFound comes
    // back after the scope was entered.
    let lost = crate::verbs::forget::forget_memory(
        &mut tx,
        &crate::core_pg_sidecars(),
        &surfaces(),
        t,
        owner.into_inner(),
    )
    .await;
    assert!(
        matches!(lost, Err(proxima_core::StorageError::NotFound)),
        "{lost:?}"
    );
    assert_eq!(
        sees(&mut tx, t).await,
        (0, 0),
        "the scope outlived the forget"
    );
    // And a forget of an id with no row at all.
    let absent = crate::verbs::forget::forget_memory(
        &mut tx,
        &crate::core_pg_sidecars(),
        &surfaces(),
        Uuid::now_v7(),
        owner.into_inner(),
    )
    .await;
    assert!(absent.is_err(), "{absent:?}");
    assert_eq!(
        sees(&mut tx, t).await,
        (0, 0),
        "the scope outlived the forget"
    );
    tx.rollback().await.expect("rollback");
}

#[tokio::test]
async fn a_failed_restore_leaves_the_scope() {
    let (_pg, _guard, owner, t, mut tx) = failing_lifecycle_fixture("r1_restore_fail").await;
    let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
    let failed = crate::verbs::forget::restore_marked(
        &mut tx,
        &crate::core_pg_sidecars(),
        Uuid::now_v7(),
        &crate::verbs::forget::Reach::of(&permit),
        crate::verbs::fact_embeddings::RouteSpaces {
            spaces: &[],
            non_embeddable_schemas: &[],
        },
    )
    .await;
    assert!(
        matches!(failed, Err(proxima_core::StorageError::NotFound)),
        "{failed:?}"
    );
    assert_eq!(
        sees(&mut tx, t).await,
        (0, 0),
        "the scope outlived the restore"
    );
    tx.rollback().await.expect("rollback");
}

#[tokio::test]
async fn a_failed_erase_leaves_the_scope() {
    let (_pg, _guard, owner, t, mut tx) = failing_lifecycle_fixture("r1_erase_fail").await;
    let failed = crate::verbs::forget::erase_memory(
        &mut tx,
        &crate::core_pg_sidecars(),
        &crate::PgHostStateEraseContext::for_surfaces_for_tests(surfaces())
            .expect("no host lifecycle tables"),
        &owner,
        Uuid::now_v7(),
    )
    .await;
    assert!(
        matches!(failed, Err(proxima_core::StorageError::NotFound)),
        "{failed:?}"
    );
    assert_eq!(
        sees(&mut tx, t).await,
        (0, 0),
        "the scope outlived the erase"
    );
    tx.rollback().await.expect("rollback");
}

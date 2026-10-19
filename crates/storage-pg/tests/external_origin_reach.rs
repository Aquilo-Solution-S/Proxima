//! #478 item 2: `proxima_core.memory_external_origin` is reached for hot AND
//! forgotten memories by every lifecycle verb, through the runtime role.
//!
//! The table carries no owner column; its owner policy finds the owner
//! through the memory it describes. The tables are FORCE ROW LEVEL SECURITY
//! and the runtime role is NOBYPASSRLS, so a policy that only looks at hot
//! `memory` rows hides a forgotten memory's origins from every statement the
//! runtime runs — including the deletes an erase relies on. These tests run
//! the verbs through `fresh_pg` (runtime role) and count with the admin pool.
#![allow(clippy::doc_markdown, clippy::too_many_lines)]

use proxima_core::owner_inverse::{
    EraseAuthorization, ExportAuthorization, OwnerEraseOutcome, OwnerEraseTarget,
    OwnerExportTarget, OwnerSurfaces,
};
use proxima_core::storage_ports::{
    FactIngestPort, MemoryAuthoringPort, OwnerInversePort, OwnerTransferPort, OwnerWritePermit,
};
use proxima_core::test_fixtures::owner_scope_for_tests;
use proxima_core::verbs::fact_ingest::FactWriteCommand;
use proxima_core::{AccessKind, EntityId, GroupId, OwnerId, SchemaId, SchemaVersion, UserId};
use proxima_storage_pg::test_fixtures::{fresh_pg, register_group_for_tests};
use proxima_storage_pg::verbs::forget::erase_memory_series;
use proxima_storage_pg::{PgStorage, begin_owner_transaction, core_pg_sidecars};
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const TABLE: &str = "proxima_core.memory_external_origin";

fn surfaces() -> OwnerSurfaces {
    OwnerSurfaces::for_registry(&proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests())
}

fn draft() -> FactWriteCommand {
    FactWriteCommand {
        schema_id: SchemaId::new("core/test-fact-v1".to_string()),
        schema_version: SchemaVersion::new(1),
        handle: None,
        source_id: None,
        ingest_key: None,
        payload: Vec::new(),
        rendered_text: Some("a fact with other-store origins".into()),
        lexical_language: None,
        receipt: None,
        occurred_at: None,
        citation: None,
        additional_references: Vec::new(),
        refs: Vec::new(),
        blob_id: None,
        kind: "fact".into(),
        request: None,
    }
}

/// `n` other-store origins on `t`, written with the admin pool the way the
/// derive path's privileged insert lands them.
async fn add_origins(pg: &PgStorage, t: Uuid, n: usize) -> Result<(), sqlx::Error> {
    for _ in 0..n {
        sqlx::query("INSERT INTO proxima_core.memory_external_origin (t, memory, edge_kind) VALUES ($1, $2, 'origin')")
            .bind(t)
            .bind(Uuid::now_v7())
            .execute(pg.pool_for_tests())
            .await?;
    }
    Ok(())
}

/// Every origin row on `ts`, counted with the admin pool (no RLS).
async fn origins_on(pg: &PgStorage, ts: &[Uuid]) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT count(*)::bigint FROM proxima_core.memory_external_origin WHERE t = ANY($1)",
    )
    .bind(ts)
    .fetch_one(pg.pool_for_tests())
    .await
}

/// What `owner` can read of the origins on `t`, through the runtime role.
async fn origins_visible_to(
    pg: &PgStorage,
    owner: OwnerId,
    t: Uuid,
) -> Result<i64, Box<dyn std::error::Error>> {
    let mut tx = begin_owner_transaction(
        &pg.clone_pool_for_backend(),
        &owner_scope_for_tests(&[owner]),
    )
    .await?;
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*)::bigint FROM proxima_core.memory_external_origin WHERE t = $1",
    )
    .bind(t)
    .fetch_one(&mut *tx)
    .await?;
    tx.rollback().await?;
    Ok(n)
}

/// A hot memory with one origin and a FORGOTTEN memory with two, for `owner`.
async fn hot_and_forgotten(
    pg: &PgStorage,
    permit: &OwnerWritePermit,
) -> Result<(Uuid, Uuid), Box<dyn std::error::Error>> {
    let hot = pg
        .ingest_fact_atomic(permit, &draft(), None)
        .await?
        .memory_id;
    let forgotten = pg
        .ingest_fact_atomic(permit, &draft(), None)
        .await?
        .memory_id;
    add_origins(pg, hot.into_inner(), 1).await?;
    add_origins(pg, forgotten.into_inner(), 2).await?;
    MemoryAuthoringPort::forget_memory(pg, permit, forgotten).await?;
    let forgotten_rows: i64 = sqlx::query_scalar(
        "SELECT count(*)::bigint FROM proxima_core.memory WHERE t = $1 AND forgotten_at IS NOT NULL",
    )
    .bind(forgotten.into_inner())
    .fetch_one(pg.pool_for_tests())
    .await?;
    assert_eq!(forgotten_rows, 1, "the second memory is forgotten");
    Ok((hot.into_inner(), forgotten.into_inner()))
}

/// The leak (red first): a hard erase of a FORGOTTEN admission runs under the
/// owner scope, where the live-only policy hides the forgotten memory's origins
/// from its delete, so they outlive the memory they described.
#[tokio::test]
async fn erasing_a_forgotten_admission_deletes_its_external_origins() -> TestResult {
    let (pg, _db) = fresh_pg("proxima_ext_origin_erase_one").await;
    let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
    let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
    let (hot, forgotten) = hot_and_forgotten(&pg, &permit).await?;

    let mut tx = begin_owner_transaction(
        &pg.clone_pool_for_backend(),
        &owner_scope_for_tests(&[owner]),
    )
    .await?;
    let context = proxima_storage_pg::PgHostStateEraseContext::for_surfaces_for_tests(surfaces())
        .expect("no host lifecycle tables");
    let (erased, _plan) =
        erase_memory_series(&mut tx, &core_pg_sidecars(), &context, &owner, &[forgotten]).await?;
    tx.commit().await?;
    assert_eq!(erased, 1);
    assert_eq!(
        origins_on(&pg, &[forgotten]).await?,
        0,
        "the erased forgotten admission's origins survived"
    );
    assert_eq!(
        origins_on(&pg, &[hot]).await?,
        1,
        "the other memory keeps its own"
    );
    Ok(())
}

/// Owner erase must leave no origin of a forgotten memory behind, and the
/// receipt must say how many it destroyed. (Owner erase already reached
/// them before #478; this is a guard, plus the new receipt counter.)
#[tokio::test]
async fn owner_erase_deletes_a_forgotten_memorys_external_origins() -> TestResult {
    let (pg, _db) = fresh_pg("proxima_ext_origin_erase").await;
    let user = UserId::new(Uuid::now_v7());
    let owner = OwnerId::from(user);
    let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
    let (hot, forgotten) = hot_and_forgotten(&pg, &permit).await?;
    assert_eq!(origins_on(&pg, &[hot, forgotten]).await?, 3);

    let auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalOwner {
        user_id: user,
        drop_event_id: "test-drop-ext-origin".into(),
    });
    let outcome = pg.erase_personal_owner(&auth, user, &surfaces()).await?;
    let OwnerEraseOutcome::Completed { counts, .. } = outcome else {
        panic!("expected a completed erase, got {outcome:?}");
    };
    assert_eq!(
        origins_on(&pg, &[forgotten]).await?,
        0,
        "the forgotten memory's origins outlived the owner erase"
    );
    assert_eq!(origins_on(&pg, &[hot]).await?, 0, "hot origins are erased");
    assert_eq!(
        counts.get("external_origins"),
        3,
        "the receipt counts every origin row the erase destroyed"
    );
    Ok(())
}

/// The export carries the table, with the hot and the forgotten memory's rows.
#[tokio::test]
async fn export_carries_hot_and_forgotten_external_origins() -> TestResult {
    let (pg, _db) = fresh_pg("proxima_ext_origin_export").await;
    let user = UserId::new(Uuid::now_v7());
    let owner = OwnerId::from(user);
    let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
    let (hot, forgotten) = hot_and_forgotten(&pg, &permit).await?;
    // Another owner's origins never appear.
    let stranger = OwnerId::from(UserId::new(Uuid::now_v7()));
    let stranger_permit = OwnerWritePermit::new_for_tests(stranger, AccessKind::Fact);
    let foreign = pg
        .ingest_fact_atomic(&stranger_permit, &draft(), None)
        .await?
        .memory_id
        .into_inner();
    add_origins(&pg, foreign, 1).await?;

    let auth =
        ExportAuthorization::new_for_tests(OwnerExportTarget::PersonalOwner { user_id: user });
    let bundle = pg.export_owner_bundle(&auth, &surfaces()).await?;
    let rows = bundle
        .tables
        .get(TABLE)
        .unwrap_or_else(|| panic!("the bundle has no {TABLE}: {:?}", bundle.tables.keys()));
    let mut ts: Vec<String> = rows
        .iter()
        .map(|row| row["t"].as_str().unwrap_or_default().to_owned())
        .collect();
    ts.sort();
    let mut expected = vec![
        hot.to_string(),
        forgotten.to_string(),
        forgotten.to_string(),
    ];
    expected.sort();
    assert_eq!(ts, expected, "hot and forgotten origins, nobody else's");
    assert_eq!(bundle.count(TABLE), 3);
    // #480: every exported row says which kind of edge it is.
    assert!(
        rows.iter().all(|row| row["edge_kind"] == "origin"),
        "exported external rows carry edge_kind: {rows:?}"
    );
    Ok(())
}

/// Forget keeps the origins (they outlive the forget); the owner's ordinary
/// reads hide them with their memory (#488 D1), and restore brings them back
/// (`forget_and_restore_keep_external_origins`).
#[tokio::test]
async fn forget_keeps_external_origins() -> TestResult {
    let (pg, _db) = fresh_pg("proxima_ext_origin_forget").await;
    let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
    let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
    let (_hot, forgotten) = hot_and_forgotten(&pg, &permit).await?;
    assert_eq!(origins_on(&pg, &[forgotten]).await?, 2, "forget keeps them");
    assert_eq!(
        origins_visible_to(&pg, owner, forgotten).await?,
        0,
        "the owner's ordinary reads hide a forgotten memory's origins"
    );
    Ok(())
}

/// #488 S14: forget keeps a memory's external origins and restore finds them
/// again; the owner reads them once the memory is back.
#[tokio::test]
async fn forget_and_restore_keep_external_origins() -> TestResult {
    let (pg, _db) = fresh_pg("proxima_ext_origin_restore").await;
    let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
    let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
    let t = pg
        .ingest_fact_atomic(&permit, &draft(), None)
        .await?
        .memory_id;
    add_origins(&pg, t.into_inner(), 2).await?;
    pg.forget_memory(&permit, t).await?;
    assert_eq!(
        origins_on(&pg, &[t.into_inner()]).await?,
        2,
        "forget keeps them"
    );
    pg.restore_memory(&permit, t, &[]).await?;
    assert_eq!(
        origins_on(&pg, &[t.into_inner()]).await?,
        2,
        "restore finds them again"
    );
    assert_eq!(origins_visible_to(&pg, owner, t.into_inner()).await?, 2);
    Ok(())
}

/// Transfer leaves the rows on their key: the destination reads them, the
/// source no longer does.
#[tokio::test]
async fn transfer_moves_read_access_to_external_origins() -> TestResult {
    let (pg, _db) = fresh_pg("proxima_ext_origin_transfer").await;
    let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
    let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
    let group = GroupId::new(Uuid::now_v7());
    register_group_for_tests(pg.pool_for_tests(), group).await?;
    let destination = OwnerId::from(group);
    let written = pg.ingest_fact_atomic(&permit, &draft(), None).await?;
    let t = written.memory_id.into_inner();
    add_origins(&pg, t, 2).await?;
    assert_eq!(origins_visible_to(&pg, owner, t).await?, 2);

    let moved = pg
        .transfer_to_owner(
            &OwnerWritePermit::transfer_for_tests(owner, AccessKind::Fact, destination),
            EntityId::Memory(written.memory_id),
            destination,
            &surfaces(),
            &[],
        )
        .await?;
    assert!(moved);
    assert_eq!(origins_visible_to(&pg, destination, t).await?, 2);
    assert_eq!(origins_visible_to(&pg, owner, t).await?, 0);
    assert_eq!(origins_on(&pg, &[t]).await?, 2, "StaysOnKey moves no row");
    Ok(())
}

/// Bundle bytes are a function of contents: a memory with several
/// other-store origins exports byte-identically whatever order its rows were
/// written in, because the primary key breaks the declared key's ties.
#[tokio::test]
async fn several_origins_export_byte_identically_across_insert_orders() -> TestResult {
    let (pg, _db) = fresh_pg("proxima_ext_origin_order").await;
    let user = UserId::new(Uuid::now_v7());
    let owner = OwnerId::from(user);
    let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
    let t = pg
        .ingest_fact_atomic(&permit, &draft(), None)
        .await?
        .memory_id
        .into_inner();
    let mut remotes: Vec<Uuid> = (0..5).map(|_| Uuid::now_v7()).collect();
    remotes.sort();
    let auth =
        ExportAuthorization::new_for_tests(OwnerExportTarget::PersonalOwner { user_id: user });

    let mut exports = Vec::new();
    for order in [
        remotes.iter().rev().copied().collect::<Vec<_>>(),
        remotes.clone(),
        vec![remotes[2], remotes[0], remotes[4], remotes[1], remotes[3]],
    ] {
        sqlx::query("DELETE FROM proxima_core.memory_external_origin WHERE t = $1")
            .bind(t)
            .execute(pg.pool_for_tests())
            .await?;
        for memory in order {
            sqlx::query(
                "INSERT INTO proxima_core.memory_external_origin (t, memory, edge_kind) VALUES ($1, $2, 'origin')",
            )
            .bind(t)
            .bind(memory)
            .execute(pg.pool_for_tests())
            .await?;
        }
        let bundle = pg.export_owner_bundle(&auth, &surfaces()).await?;
        let rows = bundle.tables.get(TABLE).cloned().unwrap_or_default();
        exports.push(serde_json::to_vec(&rows)?);
        let memories: Vec<String> = rows
            .iter()
            .map(|row| row["memory"].as_str().unwrap_or_default().to_owned())
            .collect();
        let expected: Vec<String> = remotes.iter().map(Uuid::to_string).collect();
        assert_eq!(
            memories, expected,
            "rows of one memory export in primary-key order"
        );
    }
    assert_eq!(
        exports[0], exports[1],
        "insert order leaked into the bundle bytes"
    );
    assert_eq!(
        exports[1], exports[2],
        "insert order leaked into the bundle bytes"
    );
    Ok(())
}

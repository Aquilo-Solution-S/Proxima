//! `UnitOfWork::erase_own_series` against real Postgres, through the
//! example flavor's note series (see `docs/data-and-erasure.md`).
//!
//! Every Fact here is admitted and forgotten through the Engine, so the rows
//! the erase must reach — sidecar, head, cooled locator, cold object — are
//! the ones production writes, not a fixture's idea of them.
#![allow(clippy::too_many_lines)]

#[path = "fixtures/example_engine.rs"]
mod example_engine;

use example_engine::{ExampleEngine, next_millisecond};
use proxima::flavor::{
    EraseMode, MAX_ERASE_SERIES_PER_CALL, SeriesEraseError, SeriesEraseReceipt,
    SeriesEraseRefusalKind, SeriesSelection, SidecarAtom,
};
use proxima_core::{
    AgentNoteV1, AuthPath, AuthzContext, ColdObjectStore, Engine, ErrorCode, FactPayload,
    FactWrite, GroupId, MemoryId, OwnerId, Role, SchemaId, SeriesHandle, UserId,
};
use proxima_example::{FLAVOR_ID, NoteV1};
use uuid::Uuid;

const EXAMPLE: &str = FLAVOR_ID;

type Harness = ExampleEngine;

async fn harness() -> Harness {
    ExampleEngine::new("proxima_erase_own_series", |_| {}).await
}

fn personal() -> (OwnerId, AuthzContext) {
    let user = UserId::new(Uuid::now_v7());
    (
        OwnerId::from(user),
        proxima_core::test_fixtures::authenticated_context(AuthzContext::for_subject(
            user,
            AuthPath::HostBearer,
        )),
    )
}

fn note(notebook_id: Uuid, key: &str) -> NoteV1 {
    note_at(notebook_id, key, time::OffsetDateTime::now_utc())
}

fn note_at(notebook_id: Uuid, key: &str, written_at: time::OffsetDateTime) -> NoteV1 {
    NoteV1 {
        notebook_id,
        key: key.to_owned(),
        body: format!("note {key}"),
        written_at,
    }
}

fn note_schema() -> SchemaId {
    SchemaId::new(NoteV1::SCHEMA_ID.into())
}

async fn ingest(
    engine: &Engine,
    authz: &AuthzContext,
    owner: OwnerId,
    payload: &NoteV1,
    handle: Option<SeriesHandle>,
) -> MemoryId {
    let mut write = FactWrite::new(owner, "erase-fixture", payload);
    if let Some(handle) = handle {
        write = write.handle(handle);
    }
    engine
        .ingest_fact(authz, write)
        .await
        .expect("the fixture note is admitted")
        .memory_id
}

async fn handle_of(pool: &sqlx::PgPool, t: MemoryId) -> SeriesHandle {
    let handle: Uuid = sqlx::query_scalar("SELECT handle FROM proxima_core.memory WHERE t = $1")
        .bind(t.into_inner())
        .fetch_one(pool)
        .await
        .expect("the admission is hot");
    SeriesHandle::new(handle)
}

/// Every row the erase must reach for these admissions, as one number per
/// surface, so "nothing changed" and "everything went" are both one
/// comparison.
async fn footprint(pool: &sqlx::PgPool, ts: &[MemoryId]) -> [i64; 5] {
    let ts: Vec<Uuid> = ts.iter().map(|t| t.into_inner()).collect();
    let (memory, cooled, note, ingest_keys, witnesses): (i64, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM proxima_core.memory WHERE t = ANY($1)),
                    (SELECT count(*) FROM proxima_core.cooled WHERE t = ANY($1)),
                    (SELECT count(*) FROM proxima_example.note_v1 WHERE t = ANY($1)),
                    (SELECT count(*) FROM proxima_core.ingest_keys WHERE t = ANY($1)),
                    (SELECT count(*) FROM proxima_core.erased_pin_target WHERE t = ANY($1))",
    )
    .bind(ts)
    .fetch_one(pool)
    .await
    .expect("footprint census");
    [memory, cooled, note, ingest_keys, witnesses]
}

async fn erase(
    engine: &Engine,
    authz: &AuthzContext,
    owner: OwnerId,
    selection: SeriesSelection,
    mode: EraseMode,
) -> Result<SeriesEraseReceipt, SeriesEraseError> {
    let mut unit = engine.unit_of_work(authz).await.expect("a unit opens");
    let receipt = unit
        .erase_own_series(EXAMPLE, owner, selection, mode)
        .await?;
    unit.commit().await.expect("the erase commits");
    Ok(receipt)
}

fn refusal_kind(result: Result<SeriesEraseReceipt, SeriesEraseError>) -> SeriesEraseRefusalKind {
    match result {
        Err(SeriesEraseError::Refused(refusal)) => refusal.kind,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// The message of an `InvalidArgument` protocol error; anything else fails.
fn invalid_argument(result: Result<SeriesEraseReceipt, SeriesEraseError>) -> String {
    match result {
        Err(SeriesEraseError::Protocol(err)) if err.code == ErrorCode::InvalidArgument => {
            err.message
        }
        other => panic!("expected an invalid-argument error, got {other:?}"),
    }
}

async fn run(test: impl AsyncFnOnce(&Harness) -> Result<(), Box<dyn std::error::Error>>) {
    let harness = harness().await;
    let result = test(&harness).await;
    result.expect("the test body");
    harness.pool.close().await;
}

/// One id selects its whole series, hot and cooled, and every surface of
/// every version goes: sidecar, receipt, head, cooled locator and — after
/// commit — the cold object. The erase witness records each `t`. A series
/// the selection did not name is untouched.
#[tokio::test]
async fn one_id_erases_its_whole_series_and_everything_hanging_off_it() {
    run(async |h| {
        let (owner, authz) = personal();
        let notebook_id = h.notebook(&owner).await;
        let v1 = ingest(&h.engine, &authz, owner, &note(notebook_id, "a1"), None).await;
        let handle = handle_of(&h.pool, v1).await;
        let v2 = ingest(
            &h.engine,
            &authz,
            owner,
            &note(notebook_id, "a2"),
            Some(handle),
        )
        .await;
        let other = ingest(&h.engine, &authz, owner, &note(notebook_id, "b1"), None).await;
        h.engine.forget_memory(&authz, owner, v1).await?;
        let cold_key = proxima_core::cold_object_key(v1.into_inner());
        assert!(
            h.cold.get(&cold_key).await.is_err(),
            "forget marks the row and writes no cold object"
        );
        assert_eq!(footprint(&h.pool, &[v1, v2]).await, [2, 0, 2, 2, 0]);

        let receipt = erase(
            &h.engine,
            &authz,
            owner,
            SeriesSelection::Ids(vec![v2]),
            EraseMode::Erase,
        )
        .await?;

        assert_eq!(
            receipt.versions,
            vec![v1, v2],
            "the id selects its whole series"
        );
        assert_eq!(receipt.series_erased, 1);
        assert_eq!(receipt.versions_erased, 2);
        assert_eq!(receipt.referencing_series, 0);
        assert_eq!(
            receipt.cold_objects_pending, 0,
            "a forgotten version has no cold object"
        );
        assert!(!receipt.more_remaining);
        assert_eq!(
            footprint(&h.pool, &[v1, v2]).await,
            [0, 0, 0, 0, 2],
            "every surface is gone and each erased t is witnessed"
        );
        let head: i64 =
            sqlx::query_scalar("SELECT count(*) FROM proxima_core.memory_head WHERE handle = $1")
                .bind(handle.into_inner())
                .fetch_one(&h.pool)
                .await?;
        assert_eq!(head, 0, "the series head goes with its last version");
        let pending: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM proxima_core.cold_purge_pending WHERE object_key = $1",
        )
        .bind(&cold_key)
        .fetch_one(&h.pool)
        .await?;
        assert_eq!(pending, 0, "the committed erase settled its cold debt");
        assert!(
            h.cold.get(&cold_key).await.is_err(),
            "the cold object is destroyed after commit"
        );
        assert_eq!(footprint(&h.pool, &[other]).await, [1, 0, 1, 1, 0]);

        let again = erase(
            &h.engine,
            &authz,
            owner,
            SeriesSelection::Ids(vec![v2]),
            EraseMode::Erase,
        )
        .await?;
        assert_eq!(
            again.series_erased, 0,
            "re-erasing an erased series is a no-op"
        );
        Ok(())
    })
    .await;
}

/// The dry run is the erase, rolled back: the same receipt, and a database
/// in which nothing moved.
#[tokio::test]
async fn a_dry_run_reports_what_the_erase_then_does_and_changes_nothing() {
    run(async |h| {
        let (owner, authz) = personal();
        let notebook_id = h.notebook(&owner).await;
        let v1 = ingest(&h.engine, &authz, owner, &note(notebook_id, "d1"), None).await;
        let handle = handle_of(&h.pool, v1).await;
        let v2 = ingest(
            &h.engine,
            &authz,
            owner,
            &note(notebook_id, "d2"),
            Some(handle),
        )
        .await;
        h.engine.forget_memory(&authz, owner, v1).await?;
        let before = footprint(&h.pool, &[v1, v2]).await;

        let dry = erase(
            &h.engine,
            &authz,
            owner,
            SeriesSelection::Ids(vec![v1]),
            EraseMode::DryRun,
        )
        .await?;
        assert_eq!(
            footprint(&h.pool, &[v1, v2]).await,
            before,
            "a dry run changes nothing"
        );
        assert!(
            h.cold
                .get(&proxima_core::cold_object_key(v1.into_inner()))
                .await
                .is_err(),
            "forget wrote no cold object for the dry run to keep"
        );

        let wet = erase(
            &h.engine,
            &authz,
            owner,
            SeriesSelection::Ids(vec![v1]),
            EraseMode::Erase,
        )
        .await?;
        assert_eq!(dry.mode, EraseMode::DryRun);
        assert_eq!(
            SeriesEraseReceipt {
                mode: EraseMode::Erase,
                ..dry
            },
            wet,
            "the dry run's receipt is the erase's"
        );
        Ok(())
    })
    .await;
}

/// Every out-of-scope selection is refused, and refused before anything is
/// deleted: a core schema, another flavor's schema, core named as the
/// flavor, another owner's admission, and a caller below Admin.
#[tokio::test]
async fn a_selection_outside_the_flavor_the_owner_or_the_role_is_refused_before_any_delete() {
    run(async |h| {
        let (owner, authz) = personal();
        let notebook_id = h.notebook(&owner).await;
        let own = ingest(&h.engine, &authz, owner, &note(notebook_id, "r1"), None).await;
        let core_note = h
            .engine
            .ingest_fact(
                &authz,
                FactWrite::new(
                    owner,
                    "erase-fixture",
                    &AgentNoteV1 {
                        note_id: Uuid::now_v7(),
                        title: "core".into(),
                        body: "a core schema".into(),
                        tags: Vec::new(),
                        idempotency_key: None,
                    },
                ),
            )
            .await?
            .memory_id;
        let (stranger, stranger_authz) = personal();
        let stranger_notebook = h.notebook(&stranger).await;
        let theirs = ingest(
            &h.engine,
            &stranger_authz,
            stranger,
            &note(stranger_notebook, "s1"),
            None,
        )
        .await;
        let before = footprint(&h.pool, &[own, core_note, theirs]).await;

        let core_schema = erase(
            &h.engine,
            &authz,
            owner,
            SeriesSelection::Ids(vec![own, core_note]),
            EraseMode::Erase,
        )
        .await;
        match core_schema {
            Err(SeriesEraseError::Refused(refusal)) => {
                assert_eq!(refusal.kind, SeriesEraseRefusalKind::ForeignSchema);
                assert!(
                    refusal
                        .offending
                        .iter()
                        .any(|entry| entry.contains(&core_note.into_inner().to_string())
                            && entry.contains(AgentNoteV1::SCHEMA_ID)),
                    "the refusal names the core admission and its schema: {refusal}"
                );
            }
            other => panic!("a core schema must be refused, got {other:?}"),
        }
        assert_eq!(
            refusal_kind(
                erase(
                    &h.engine,
                    &authz,
                    owner,
                    SeriesSelection::AdmittedBefore {
                        schema: SchemaId::new("another-flavor/thing-v1".into()),
                        cutoff: time::OffsetDateTime::now_utc(),
                    },
                    EraseMode::Erase,
                )
                .await
            ),
            SeriesEraseRefusalKind::ForeignSchema,
            "another flavor's schema is refused by name"
        );
        match erase(
            &h.engine,
            &authz,
            owner,
            SeriesSelection::Ids(vec![theirs]),
            EraseMode::Erase,
        )
        .await
        {
            Err(SeriesEraseError::Refused(refusal)) => {
                assert_eq!(refusal.kind, SeriesEraseRefusalKind::CrossOwner);
                assert!(
                    refusal
                        .offending
                        .iter()
                        .any(|entry| entry.contains(&theirs.into_inner().to_string())),
                    "the refusal names the other owner's admission: {refusal}"
                );
            }
            other => panic!("another owner's admission must be refused, got {other:?}"),
        }

        let mut unit = h.engine.unit_of_work(&authz).await?;
        match unit
            .erase_own_series(
                "core",
                owner,
                SeriesSelection::Ids(vec![core_note]),
                EraseMode::Erase,
            )
            .await
        {
            Err(SeriesEraseError::Protocol(err)) => assert_eq!(err.code, ErrorCode::Forbidden),
            other => panic!("core is not a flavor that erases its own series: {other:?}"),
        }

        let group = OwnerId::from(GroupId::new(Uuid::now_v7()));
        for role in [Role::ingest(), Role::editor()] {
            let below_admin = proxima_core::test_fixtures::authenticated_context(
                AuthzContext::for_subject_with_role(
                    UserId::new(Uuid::now_v7()),
                    [(group, role)],
                    AuthPath::HostBearer,
                ),
            );
            match erase(
                &h.engine,
                &below_admin,
                group,
                SeriesSelection::Ids(vec![own]),
                EraseMode::Erase,
            )
            .await
            {
                Err(SeriesEraseError::Protocol(err)) => {
                    assert_eq!(err.code, ErrorCode::Forbidden, "{role:?}");
                }
                other => panic!("{role:?} is below Admin and must be refused: {other:?}"),
            }
        }

        assert_eq!(
            footprint(&h.pool, &[own, core_note, theirs]).await,
            before,
            "no refusal deleted anything"
        );
        Ok(())
    })
    .await;
}

/// Retention erases whole series by their NEWEST admission: a series with
/// one version after the cutoff keeps every version, the old one included.
#[tokio::test]
async fn admitted_before_takes_only_series_whose_newest_version_is_older() {
    run(async |h| {
        let (owner, authz) = personal();
        let notebook_id = h.notebook(&owner).await;
        let old = ingest(&h.engine, &authz, owner, &note(notebook_id, "o1"), None).await;
        let kept_v1 = ingest(&h.engine, &authz, owner, &note(notebook_id, "k1"), None).await;
        let kept = handle_of(&h.pool, kept_v1).await;
        let cutoff = next_millisecond().await;
        next_millisecond().await;
        let kept_v2 = ingest(
            &h.engine,
            &authz,
            owner,
            &note(notebook_id, "k2"),
            Some(kept),
        )
        .await;
        let young = ingest(&h.engine, &authz, owner, &note(notebook_id, "y1"), None).await;

        let receipt = erase(
            &h.engine,
            &authz,
            owner,
            SeriesSelection::AdmittedBefore {
                schema: SchemaId::new(NoteV1::SCHEMA_ID.into()),
                cutoff,
            },
            EraseMode::Erase,
        )
        .await?;
        assert_eq!(
            receipt.versions,
            vec![old],
            "only the wholly old series goes"
        );
        assert!(!receipt.more_remaining);
        assert_eq!(
            footprint(&h.pool, &[kept_v1, kept_v2, young]).await,
            [3, 0, 3, 3, 0],
            "a series with a newer version keeps all of its versions"
        );
        Ok(())
    })
    .await;
}

/// The cap bounds one call. An `Ids` selection over it is refused whole;
/// `AdmittedBefore` erases up to it, says more remain, and the next call
/// takes the rest.
#[tokio::test]
async fn the_cap_refuses_an_oversized_id_list_and_pages_a_cutoff() {
    run(async |h| {
        let (owner, authz) = personal();
        let notebook_id = h.notebook(&owner).await;
        let mut ids = Vec::new();
        for n in 0..=MAX_ERASE_SERIES_PER_CALL {
            ids.push(
                ingest(
                    &h.engine,
                    &authz,
                    owner,
                    &note(notebook_id, &format!("c{n}")),
                    None,
                )
                .await,
            );
        }
        let before = footprint(&h.pool, &ids).await;

        assert_eq!(
            refusal_kind(
                erase(
                    &h.engine,
                    &authz,
                    owner,
                    SeriesSelection::Ids(ids.clone()),
                    EraseMode::Erase,
                )
                .await
            ),
            SeriesEraseRefusalKind::OverCap
        );
        assert_eq!(
            footprint(&h.pool, &ids).await,
            before,
            "refused before any delete"
        );

        let cutoff = time::OffsetDateTime::now_utc() + time::Duration::seconds(1);
        let selection = || SeriesSelection::AdmittedBefore {
            schema: SchemaId::new(NoteV1::SCHEMA_ID.into()),
            cutoff,
        };
        let first = erase(&h.engine, &authz, owner, selection(), EraseMode::Erase).await?;
        assert_eq!(
            first.versions,
            ids[..MAX_ERASE_SERIES_PER_CALL].to_vec(),
            "oldest first"
        );
        assert!(first.more_remaining, "one series is left for the next call");
        let second = erase(&h.engine, &authz, owner, selection(), EraseMode::Erase).await?;
        assert_eq!(second.versions, vec![ids[MAX_ERASE_SERIES_PER_CALL]]);
        assert!(!second.more_remaining);
        let third = erase(&h.engine, &authz, owner, selection(), EraseMode::Erase).await?;
        assert_eq!(third.series_erased, 0, "nothing is left");
        Ok(())
    })
    .await;
}

/// The host's retention runs with no user and no tool: a unit opened from
/// the engine's system authority erases under the same scope rules, and
/// authorizes nothing else.
#[tokio::test]
async fn the_host_erases_under_system_authority_with_the_same_scope_rules() {
    let harness = harness().await;
    let (owner, authz) = personal();
    let notebook_id = harness.notebook(&owner).await;
    let Harness {
        pool, engine, db, ..
    } = harness;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let (engine, system) = engine.into_system_authority();
        let fact = ingest(&engine, &authz, owner, &note(notebook_id, "h1"), None).await;
        let core_note = engine
            .ingest_fact(
                &authz,
                FactWrite::new(
                    owner,
                    "erase-fixture",
                    &AgentNoteV1 {
                        note_id: Uuid::now_v7(),
                        title: "core".into(),
                        body: "a core schema".into(),
                        tags: Vec::new(),
                        idempotency_key: None,
                    },
                ),
            )
            .await?
            .memory_id;

        let mut refused = engine.system_unit_of_work(&system)?;
        match refused
            .erase_own_series(
                EXAMPLE,
                owner,
                SeriesSelection::Ids(vec![core_note]),
                EraseMode::Erase,
            )
            .await
        {
            Err(SeriesEraseError::Refused(refusal)) => {
                assert_eq!(refusal.kind, SeriesEraseRefusalKind::ForeignSchema);
            }
            other => panic!("system authority does not widen the flavor's scope: {other:?}"),
        }

        let mut writes = engine.system_unit_of_work(&system)?;
        let err = writes
            .ingest_fact(FactWrite::new(
                owner,
                "erase-fixture",
                &note(notebook_id, "h2"),
            ))
            .await
            .expect_err("a system unit authorizes no cognitive write");
        assert_eq!(err.code, ErrorCode::Forbidden);

        let mut unit = engine.system_unit_of_work(&system)?;
        let receipt = unit
            .erase_own_series(
                EXAMPLE,
                owner,
                SeriesSelection::Ids(vec![fact]),
                EraseMode::Erase,
            )
            .await?;
        unit.commit().await?;
        assert_eq!(receipt.versions, vec![fact]);
        assert_eq!(footprint(&pool, &[fact]).await, [0, 0, 0, 0, 1]);
        assert_eq!(footprint(&pool, &[core_note]).await[0], 1);
        Ok(())
    }
    .await;
    result.expect("the host erase");
    pool.close().await;
    drop(db);
}

/// The erase takes its locks before anything else in the transaction, so it
/// is the unit's first and only operation.
#[tokio::test]
async fn an_erase_is_the_first_and_only_operation_of_its_unit() {
    run(async |h| {
        let (owner, authz) = personal();
        let notebook_id = h.notebook(&owner).await;
        let fact = ingest(&h.engine, &authz, owner, &note(notebook_id, "u1"), None).await;

        let mut written = h.engine.unit_of_work(&authz).await?;
        written
            .ingest_fact(FactWrite::new(
                owner,
                "erase-fixture",
                &note(notebook_id, "u2"),
            ))
            .await?;
        match written
            .erase_own_series(
                EXAMPLE,
                owner,
                SeriesSelection::Ids(vec![fact]),
                EraseMode::Erase,
            )
            .await
        {
            Err(SeriesEraseError::Protocol(err)) => {
                assert_eq!(err.code, ErrorCode::InvalidArgument);
            }
            other => panic!("an erase after a write must be refused: {other:?}"),
        }
        drop(written);

        let mut erased = h.engine.unit_of_work(&authz).await?;
        erased
            .erase_own_series(
                EXAMPLE,
                owner,
                SeriesSelection::Ids(vec![fact]),
                EraseMode::Erase,
            )
            .await?;
        erased
            .ingest_fact(FactWrite::new(
                owner,
                "erase-fixture",
                &note(notebook_id, "u3"),
            ))
            .await
            .expect_err("nothing runs on an erase's session after it");
        erased.commit().await?;
        assert_eq!(footprint(&h.pool, &[fact]).await[0], 0);
        Ok(())
    })
    .await;
}

/// Sidecar equality selects every series with a hot version whose row
/// matches — a version, not only the newest, so a series whose history was
/// filed under the notebook goes whole — and every version of it, the
/// cooled one included. The dry run is the erase, rolled back.
#[tokio::test]
async fn sidecar_equals_erases_every_series_with_a_matching_version() {
    run(async |h| {
        let (owner, authz) = personal();
        let notebook_a = h.notebook(&owner).await;
        let notebook_b = h.notebook(&owner).await;
        let single = ingest(&h.engine, &authz, owner, &note(notebook_a, "a1"), None).await;
        let moved_v1 = ingest(&h.engine, &authz, owner, &note(notebook_a, "m1"), None).await;
        let moved = handle_of(&h.pool, moved_v1).await;
        let moved_v2 = ingest(
            &h.engine,
            &authz,
            owner,
            &note(notebook_b, "m2"),
            Some(moved),
        )
        .await;
        let cooled_v1 = ingest(&h.engine, &authz, owner, &note(notebook_a, "c1"), None).await;
        let cooled = handle_of(&h.pool, cooled_v1).await;
        let cooled_v2 = ingest(
            &h.engine,
            &authz,
            owner,
            &note(notebook_a, "c2"),
            Some(cooled),
        )
        .await;
        h.engine.forget_memory(&authz, owner, cooled_v1).await?;
        let other = ingest(&h.engine, &authz, owner, &note(notebook_b, "b1"), None).await;
        let selected = [single, moved_v1, moved_v2, cooled_v1, cooled_v2];
        let before = footprint(&h.pool, &selected).await;
        let selection = || SeriesSelection::SidecarEquals {
            schema: note_schema(),
            predicates: vec![("notebook_id".to_owned(), SidecarAtom::Uuid(notebook_a))],
        };

        let dry = erase(&h.engine, &authz, owner, selection(), EraseMode::DryRun).await?;
        assert_eq!(
            footprint(&h.pool, &selected).await,
            before,
            "a dry run changes nothing"
        );
        let wet = erase(&h.engine, &authz, owner, selection(), EraseMode::Erase).await?;
        assert_eq!(
            SeriesEraseReceipt {
                mode: EraseMode::Erase,
                ..dry
            },
            wet,
            "the dry run's receipt is the erase's"
        );
        let mut expected = selected.to_vec();
        expected.sort_unstable();
        assert_eq!(
            wet.versions, expected,
            "each matching series goes whole, the one that left the notebook included"
        );
        assert_eq!(wet.series_erased, 3);
        assert_eq!(
            wet.cold_objects_pending, 0,
            "a forgotten version has no cold object"
        );
        assert!(!wet.more_remaining);
        assert_eq!(footprint(&h.pool, &selected).await, [0, 0, 0, 0, 5]);
        assert_eq!(
            footprint(&h.pool, &[other]).await,
            [1, 0, 1, 1, 0],
            "a series that never matched is untouched"
        );

        let again = erase(&h.engine, &authz, owner, selection(), EraseMode::Erase).await?;
        assert_eq!(again.series_erased, 0, "nothing matches any more");
        Ok(())
    })
    .await;
}

/// The predicates are AND-joined and narrow the owner's series only: a row
/// of another owner with the same values is never selected.
#[tokio::test]
async fn sidecar_equals_joins_its_predicates_inside_the_owner() {
    run(async |h| {
        let (owner, authz) = personal();
        let notebook = h.notebook(&owner).await;
        let hit = ingest(&h.engine, &authz, owner, &note(notebook, "shared"), None).await;
        let other_key = ingest(&h.engine, &authz, owner, &note(notebook, "other"), None).await;
        let (stranger, stranger_authz) = personal();
        let stranger_notebook = h.notebook(&stranger).await;
        let theirs = ingest(
            &h.engine,
            &stranger_authz,
            stranger,
            &note(stranger_notebook, "shared"),
            None,
        )
        .await;

        let receipt = erase(
            &h.engine,
            &authz,
            owner,
            SeriesSelection::SidecarEquals {
                schema: note_schema(),
                predicates: vec![
                    ("notebook_id".to_owned(), SidecarAtom::Uuid(notebook)),
                    ("key".to_owned(), SidecarAtom::Text("shared".to_owned())),
                ],
            },
            EraseMode::Erase,
        )
        .await?;
        assert_eq!(receipt.versions, vec![hit]);
        assert_eq!(footprint(&h.pool, &[other_key]).await[0], 1);

        let receipt = erase(
            &h.engine,
            &authz,
            owner,
            SeriesSelection::SidecarEquals {
                schema: note_schema(),
                predicates: vec![("key".to_owned(), SidecarAtom::Text("shared".to_owned()))],
            },
            EraseMode::Erase,
        )
        .await?;
        assert_eq!(
            receipt.series_erased, 0,
            "another owner's matching row is out of the selection"
        );
        assert_eq!(footprint(&h.pool, &[theirs]).await, [1, 0, 1, 1, 0]);
        Ok(())
    })
    .await;
}

/// The declared clock is the payload's, not admission: a backfilled note
/// ages by its `written_at`. The NEWEST version's value decides — a series
/// whose newest version is recent keeps every version, one whose newest
/// version is old goes whole — and a forgotten newest version keeps its
/// sidecar row, so it still carries the value that ages its series.
#[tokio::test]
async fn declared_before_ages_series_by_their_newest_versions_declared_time() {
    run(async |h| {
        let (owner, authz) = personal();
        let notebook = h.notebook(&owner).await;
        let now = time::OffsetDateTime::now_utc();
        let old = now - time::Duration::days(30);
        let cutoff = now - time::Duration::days(1);

        let backfilled = ingest(
            &h.engine,
            &authz,
            owner,
            &note_at(notebook, "f1", old),
            None,
        )
        .await;
        let revived_v1 = ingest(
            &h.engine,
            &authz,
            owner,
            &note_at(notebook, "r1", old),
            None,
        )
        .await;
        let revived = handle_of(&h.pool, revived_v1).await;
        let revived_v2 = ingest(
            &h.engine,
            &authz,
            owner,
            &note_at(notebook, "r2", now),
            Some(revived),
        )
        .await;
        let corrected_v1 = ingest(
            &h.engine,
            &authz,
            owner,
            &note_at(notebook, "k1", now),
            None,
        )
        .await;
        let corrected = handle_of(&h.pool, corrected_v1).await;
        let corrected_v2 = ingest(
            &h.engine,
            &authz,
            owner,
            &note_at(notebook, "k2", old),
            Some(corrected),
        )
        .await;
        let forgotten_v1 = ingest(
            &h.engine,
            &authz,
            owner,
            &note_at(notebook, "w1", old),
            None,
        )
        .await;
        let forgotten = handle_of(&h.pool, forgotten_v1).await;
        let forgotten_v2 = ingest(
            &h.engine,
            &authz,
            owner,
            &note_at(notebook, "w2", old),
            Some(forgotten),
        )
        .await;
        h.engine.forget_memory(&authz, owner, forgotten_v2).await?;

        let by_admission = erase(
            &h.engine,
            &authz,
            owner,
            SeriesSelection::AdmittedBefore {
                schema: note_schema(),
                cutoff,
            },
            EraseMode::DryRun,
        )
        .await?;
        assert_eq!(
            by_admission.series_erased, 0,
            "every note was admitted after the cutoff"
        );

        let receipt = erase(
            &h.engine,
            &authz,
            owner,
            SeriesSelection::DeclaredBefore {
                schema: note_schema(),
                column: "written_at".to_owned(),
                cutoff,
            },
            EraseMode::Erase,
        )
        .await?;
        let mut expected = vec![
            backfilled,
            corrected_v1,
            corrected_v2,
            forgotten_v1,
            forgotten_v2,
        ];
        expected.sort_unstable();
        assert_eq!(receipt.versions, expected);
        assert!(!receipt.more_remaining);
        assert_eq!(
            footprint(&h.pool, &[revived_v1, revived_v2]).await,
            [2, 0, 2, 2, 0],
            "a series whose newest version is recent keeps its old version too"
        );
        assert_eq!(
            footprint(&h.pool, &[forgotten_v1, forgotten_v2]).await,
            [0, 0, 0, 0, 2],
            "a forgotten newest version keeps its declared value, so its old series goes"
        );
        Ok(())
    })
    .await;
}

/// Both sidecar selections page like `AdmittedBefore`: a call stops at the
/// cap, says more remain, and takes its oldest first — by the declared
/// value for `DeclaredBefore`, by admission for `SidecarEquals`.
#[tokio::test]
async fn sidecar_selections_page_at_the_cap_oldest_first() {
    run(async |h| {
        let (owner, authz) = personal();
        let notebook = h.notebook(&owner).await;
        let base = time::OffsetDateTime::now_utc() - time::Duration::days(30);
        let mut ids = Vec::new();
        for n in 0..=MAX_ERASE_SERIES_PER_CALL {
            // The first admitted carries the newest written time.
            let offset = i64::try_from(MAX_ERASE_SERIES_PER_CALL - n)?;
            let payload = note_at(
                notebook,
                &format!("p{n}"),
                base + time::Duration::seconds(offset),
            );
            ids.push(ingest(&h.engine, &authz, owner, &payload, None).await);
        }

        let declared = erase(
            &h.engine,
            &authz,
            owner,
            SeriesSelection::DeclaredBefore {
                schema: note_schema(),
                column: "written_at".to_owned(),
                cutoff: base + time::Duration::days(1),
            },
            EraseMode::DryRun,
        )
        .await?;
        assert_eq!(
            declared.versions,
            ids[1..].to_vec(),
            "the oldest written times first, so the first admission waits"
        );
        assert!(declared.more_remaining);

        let selection = || SeriesSelection::SidecarEquals {
            schema: note_schema(),
            predicates: vec![("notebook_id".to_owned(), SidecarAtom::Uuid(notebook))],
        };
        let first = erase(&h.engine, &authz, owner, selection(), EraseMode::Erase).await?;
        assert_eq!(first.versions, ids[..MAX_ERASE_SERIES_PER_CALL].to_vec());
        assert!(first.more_remaining);
        let second = erase(&h.engine, &authz, owner, selection(), EraseMode::Erase).await?;
        assert_eq!(second.versions, vec![ids[MAX_ERASE_SERIES_PER_CALL]]);
        assert!(!second.more_remaining);
        Ok(())
    })
    .await;
}

/// A sidecar selection outside the flavor, or naming a column the sidecar
/// does not hold — or, for `DeclaredBefore`, one that is not `timestamptz`
/// — is refused before any delete.
#[tokio::test]
async fn a_sidecar_selection_outside_its_declaration_is_refused_before_any_delete() {
    run(async |h| {
        let (owner, authz) = personal();
        let notebook = h.notebook(&owner).await;
        let old = time::OffsetDateTime::now_utc() - time::Duration::days(30);
        let fact = ingest(
            &h.engine,
            &authz,
            owner,
            &note_at(notebook, "x1", old),
            None,
        )
        .await;
        let before = footprint(&h.pool, &[fact]).await;
        let cutoff = time::OffsetDateTime::now_utc();
        let foreign = || SchemaId::new("another-flavor/thing-v1".into());

        for selection in [
            SeriesSelection::SidecarEquals {
                schema: foreign(),
                predicates: vec![("notebook_id".to_owned(), SidecarAtom::Uuid(notebook))],
            },
            SeriesSelection::DeclaredBefore {
                schema: foreign(),
                column: "written_at".to_owned(),
                cutoff,
            },
        ] {
            assert_eq!(
                refusal_kind(erase(&h.engine, &authz, owner, selection, EraseMode::Erase).await),
                SeriesEraseRefusalKind::ForeignSchema
            );
        }

        let empty = invalid_argument(
            erase(
                &h.engine,
                &authz,
                owner,
                SeriesSelection::SidecarEquals {
                    schema: note_schema(),
                    predicates: Vec::new(),
                },
                EraseMode::Erase,
            )
            .await,
        );
        assert!(empty.contains("at least one column predicate"), "{empty}");

        let unknown = invalid_argument(
            erase(
                &h.engine,
                &authz,
                owner,
                SeriesSelection::SidecarEquals {
                    schema: note_schema(),
                    predicates: vec![("no_such_column".to_owned(), SidecarAtom::Bool(true))],
                },
                EraseMode::Erase,
            )
            .await,
        );
        assert!(unknown.contains("no_such_column"), "{unknown}");

        for column in ["key", "no_such_column"] {
            let refused = invalid_argument(
                erase(
                    &h.engine,
                    &authz,
                    owner,
                    SeriesSelection::DeclaredBefore {
                        schema: note_schema(),
                        column: column.to_owned(),
                        cutoff,
                    },
                    EraseMode::Erase,
                )
                .await,
            );
            assert!(refused.contains(column), "{refused}");
        }

        assert_eq!(
            footprint(&h.pool, &[fact]).await,
            before,
            "no refusal deleted anything"
        );
        Ok(())
    })
    .await;
}

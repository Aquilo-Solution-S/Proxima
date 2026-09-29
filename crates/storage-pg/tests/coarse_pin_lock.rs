//! Coarse pin-lock mode (0022): a transaction holding SHARE ROW EXCLUSIVE or
//! stronger on `memory`, `cooled` and `goal` stops taking one advisory lock
//! per pin target once it has taken 256. Requires local PG.
#![allow(clippy::too_many_lines)]

use std::error::Error;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use proxima_core::storage_ports::{FactIngestPort, MemoryAuthoringPort, OwnerWritePermit};
use proxima_core::verbs::fact_ingest::FactWriteCommand;
use proxima_core::{
    AccessKind, EdgeEndpoint, EntityKind, MemoryId, OwnerRef, SchemaId, SchemaVersion,
    StorageError, UserId,
};
use proxima_pg_testkit::{create_db, db_url, drop_db, split_role_urls};
use proxima_storage_pg::test_fixtures::create_core_db;
use proxima_storage_pg::verbs::forget::erase_memory;
use proxima_storage_pg::{PgPoolConfig, PgStorage, PgTuning, core_migrator, core_pg_sidecars};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

type TestResult = Result<(), Box<dyn Error>>;

const COARSE: &str = "LOCK TABLE proxima_core.cooled, proxima_core.memory, proxima_core.goal \
                      IN SHARE ROW EXCLUSIVE MODE";

/// Target locks a transaction takes before it reads `pg_locks`.
const DECIDE_AFTER: i64 = 256;

fn fact_draft() -> FactWriteCommand {
    FactWriteCommand {
        schema_id: SchemaId::new("core/test-fact-v1".to_owned()),
        schema_version: SchemaVersion::new(1),
        handle: None,
        source_id: None,
        ingest_key: None,
        payload: Vec::new(),
        rendered_text: None,
        lexical_language: None,
        receipt: None,
        citation: None,
        additional_references: Vec::new(),
        refs: Vec::new(),
        blob_id: None,
        kind: "fact".into(),
    }
}

fn abstraction_draft(origin: MemoryId) -> FactWriteCommand {
    let mut draft = fact_draft();
    draft.kind = "abstraction".into();
    draft.additional_references = vec![EdgeEndpoint::memory(EntityKind::Fact, origin)];
    draft
}

/// `rows` Facts under one new handle, in one statement.
async fn insert_facts(
    conn: &mut PgConnection,
    owner_id: Uuid,
    rows: i64,
) -> Result<(), sqlx::Error> {
    let handle: Uuid = sqlx::query_scalar(
        "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
         VALUES (uuidv7(), 'fact', 'core/test-fact-v1', $1, uuidv7())
         RETURNING handle",
    )
    .bind(owner_id)
    .fetch_one(&mut *conn)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.memory (handle, kind, owner_id, schema_id)
         SELECT $1, 'fact', $2, 'core/test-fact-v1' FROM generate_series(1, $3)",
    )
    .bind(handle)
    .bind(owner_id)
    .bind(rows)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// One non-Fact row pinning `origins`, with its own handle and content.
async fn insert_derived(
    conn: &mut PgConnection,
    owner_id: Uuid,
    kind: &str,
    origins: &[Uuid],
) -> Result<Uuid, sqlx::Error> {
    let (handle, t) = (Uuid::now_v7(), Uuid::now_v7());
    let content_id: Uuid = sqlx::query_scalar(
        "INSERT INTO proxima_core.content (owner_id, schema_id, content_hash)
         VALUES ($1, 'core/test-fact-v1', $2)
         RETURNING content_id",
    )
    .bind(owner_id)
    .bind([t.as_bytes().as_slice(), t.as_bytes().as_slice()].concat())
    .fetch_one(&mut *conn)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
         VALUES ($1, $2::proxima_core.memory_kind, 'core/test-fact-v1', $3, $4)",
    )
    .bind(handle)
    .bind(kind)
    .bind(owner_id)
    .bind(t)
    .execute(&mut *conn)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.memory (handle, t, kind, owner_id, schema_id, content_id, origins)
         VALUES ($1, $2, $3::proxima_core.memory_kind, $4, 'core/test-fact-v1', $5, $6)",
    )
    .bind(handle)
    .bind(t)
    .bind(kind)
    .bind(owner_id)
    .bind(content_id)
    .bind(origins)
    .execute(&mut *conn)
    .await?;
    Ok(t)
}

/// The mode this transaction bound and the advisory locks it holds.
async fn pin_lock_state(conn: &mut PgConnection) -> Result<(Option<String>, i64), sqlx::Error> {
    sqlx::query_as(
        "SELECT current_setting('proxima_core.pin_lock_mode', true),
                (SELECT count(*) FROM pg_locks
                  WHERE pid = pg_backend_pid() AND locktype = 'advisory')",
    )
    .fetch_one(conn)
    .await
}

/// Wait until another session of this database waits on a lock.
async fn wait_for_lock_waiter(pool: &PgPool) -> TestResult {
    for _ in 0..1000 {
        let waiting: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_stat_activity
              WHERE datname = current_database()
                AND pid <> pg_backend_pid()
                AND wait_event_type = 'Lock'",
        )
        .fetch_one(pool)
        .await?;
        if waiting > 0 {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    Err("no session waited on a lock within 20 s".into())
}

async fn ungrounded_memories(pool: &PgPool) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT count(*) FROM proxima_core.memory m
          WHERE m.kind <> 'fact'
            AND NOT proxima_core.pins_have_grounding_support(m.origins || m.refs, NULL, NULL)",
    )
    .fetch_one(pool)
    .await
}

/// Every core migration, in order, applied by the platform role. The storage
/// connects as the test superuser first, as the upgrade tests do: the runtime
/// RLS guard refuses a superuser pool once owner RLS is active.
async fn migrated_db(name: &str) -> Result<PgStorage, Box<dyn Error>> {
    create_db(name).await?;
    let pg = PgStorage::connect(&db_url(name)).await?;
    let (_, platform_url) = split_role_urls(name).await?;
    PgStorage::connect_for_migrations_with_config(
        &platform_url,
        PgPoolConfig::default(),
        PgTuning::default(),
    )
    .await?
    .run_migrations()
    .await?;
    Ok(pg)
}

/// The pre-owner-RLS fixture lane plus 0022, staged the way the enforced
/// fixtures stage theirs. The engine's test permits carry no `OwnerScope`,
/// which enforced RLS requires; the pin check under test is the same routine.
async fn engine_db(name: &str) -> Result<PgStorage, Box<dyn Error>> {
    create_core_db(name).await?;
    let pg = PgStorage::connect(&db_url(name)).await?;
    let coarse = core_migrator()
        .iter()
        .find(|migration| migration.version == 22)
        .ok_or("0022 is embedded")?
        .sql
        .clone();
    sqlx::raw_sql(coarse).execute(pg.pool_for_tests()).await?;
    Ok(pg)
}

/// Seed the owner row through the engine; returns the owner and one hot Fact.
async fn seed_owner(
    pg: &PgStorage,
) -> Result<(OwnerRef, OwnerWritePermit, MemoryId), StorageError> {
    let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
    let fact = pg.ingest_fact_atomic(&permit, &fact_draft(), None).await?;
    Ok((owner, permit, fact.memory_id))
}

#[tokio::test]
async fn coarse_mode_needs_all_three_tables_in_share_row_exclusive() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    let result: TestResult = async {
        let pg = engine_db(&db_name).await?;
        let (owner, _, _) = seed_owner(&pg).await?;
        let pool = pg.pool_for_tests();
        let cases = [
            ("no lock", None, false),
            (
                "memory",
                Some(sqlx::query(
                    "LOCK TABLE proxima_core.memory IN SHARE ROW EXCLUSIVE MODE",
                )),
                false,
            ),
            (
                "memory, cooled",
                Some(sqlx::query(
                    "LOCK TABLE proxima_core.memory, proxima_core.cooled \
                     IN SHARE ROW EXCLUSIVE MODE",
                )),
                false,
            ),
            (
                "cooled, goal",
                Some(sqlx::query(
                    "LOCK TABLE proxima_core.cooled, proxima_core.goal IN ACCESS EXCLUSIVE MODE",
                )),
                false,
            ),
            (
                "all three in SHARE",
                Some(sqlx::query(
                    "LOCK TABLE proxima_core.memory, proxima_core.cooled, proxima_core.goal \
                     IN SHARE MODE",
                )),
                false,
            ),
            ("all three", Some(sqlx::query(COARSE)), true),
            (
                "all three in ACCESS EXCLUSIVE",
                Some(sqlx::query(
                    "LOCK TABLE proxima_core.memory, proxima_core.cooled, proxima_core.goal \
                     IN ACCESS EXCLUSIVE MODE",
                )),
                true,
            ),
        ];
        for (held, lock, coarse) in cases {
            let mut tx = pool.begin().await?;
            if let Some(lock) = lock {
                lock.execute(&mut *tx).await?;
            }
            insert_facts(&mut tx, owner.stored_owner_id(), 400).await?;
            let (mode, advisory) = pin_lock_state(&mut tx).await?;
            tx.rollback().await?;
            if coarse {
                assert_eq!(mode.as_deref(), Some("coarse"), "{held}");
                assert_eq!(
                    advisory, DECIDE_AFTER,
                    "{held}: target locks stop at the decision"
                );
            } else {
                assert_eq!(mode.as_deref(), Some("fine"), "{held}");
                assert_eq!(advisory, 400, "{held}: every row keeps its target lock");
            }
        }

        // A small write never reads pg_locks: the setting still counts.
        let mut tx = pool.begin().await?;
        sqlx::query(COARSE).execute(&mut *tx).await?;
        insert_facts(&mut tx, owner.stored_owner_id(), 3).await?;
        assert_eq!(pin_lock_state(&mut tx).await?, (Some("3".to_owned()), 3));
        tx.rollback().await?;

        // The setting is not trusted: bound without the locks, the trigger
        // takes them instead of skipping the target locks unprotected.
        let mut tx = pool.begin().await?;
        sqlx::query("SET LOCAL proxima_core.pin_lock_mode = 'coarse'")
            .execute(&mut *tx)
            .await?;
        insert_facts(&mut tx, owner.stored_owner_id(), 3).await?;
        assert_eq!(
            pin_lock_state(&mut tx).await?,
            (Some("coarse".to_owned()), 0)
        );
        let held: i64 = sqlx::query_scalar(
            "SELECT count(DISTINCT relation) FROM pg_locks
              WHERE pid = pg_backend_pid() AND granted
                AND mode = 'ShareRowExclusiveLock'
                AND relation IN ('proxima_core.memory'::regclass,
                                 'proxima_core.cooled'::regclass,
                                 'proxima_core.goal'::regclass)",
        )
        .fetch_one(&mut *tx)
        .await?;
        assert_eq!(held, 3, "a bound setting acquires the coarse locks");
        tx.rollback().await?;
        Ok(())
    }
    .await;
    drop_db(&db_name).await.expect("drop test database");
    result.expect("coarse mode detection");
}

#[tokio::test]
async fn coarse_mode_still_refuses_missing_and_erased_targets() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    let result: TestResult = async {
        let pg = engine_db(&db_name).await?;
        let (owner, permit, erased) = seed_owner(&pg).await?;
        let owner_id = owner.stored_owner_id();
        let pool = pg.pool_for_tests();
        // A live pin grounds the row, so the refusal is the target's own.
        let live = pg
            .ingest_fact_atomic(&permit, &fact_draft(), None)
            .await?
            .memory_id;
        let mut tx = pool.begin().await?;
        erase_memory(
            &mut tx,
            &core_pg_sidecars(),
            &pg.host_state_erase_context()?,
            &owner,
            erased.into_inner(),
        )
        .await?;
        tx.commit().await?;
        let before: i64 = sqlx::query_scalar("SELECT count(*) FROM proxima_core.memory")
            .fetch_one(pool)
            .await?;

        for (target, reason) in [(Uuid::now_v7(), "missing"), (erased.into_inner(), "erased")] {
            let mut tx = pool.begin().await?;
            sqlx::query(COARSE).execute(&mut *tx).await?;
            insert_facts(&mut tx, owner_id, DECIDE_AFTER + 1).await?;
            assert_eq!(pin_lock_state(&mut tx).await?.0.as_deref(), Some("coarse"));
            let error = insert_derived(
                &mut tx,
                owner_id,
                "abstraction",
                &[live.into_inner(), target],
            )
            .await
            .expect_err(reason);
            let database = error.as_database_error().expect("database refusal");
            assert_eq!(
                database.code().as_deref(),
                Some("23503"),
                "{reason}: {error}"
            );
            assert!(
                database.message().contains(&target.to_string()),
                "{reason}: {error}"
            );
            tx.rollback().await?;
        }
        let after: i64 = sqlx::query_scalar("SELECT count(*) FROM proxima_core.memory")
            .fetch_one(pool)
            .await?;
        assert_eq!(after, before, "a refused pin fails the whole transaction");
        Ok(())
    }
    .await;
    drop_db(&db_name).await.expect("drop test database");
    result.expect("coarse mode pin checks");
}

#[tokio::test]
async fn concurrent_forget_waits_for_the_bulk_transaction() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    let result: TestResult = async {
        let pg = engine_db(&db_name).await?;
        let (owner, permit, fact) = seed_owner(&pg).await?;
        let owner_id = owner.stored_owner_id();
        let pool = pg.pool_for_tests();
        let abstraction = pg
            .ingest_fact_atomic(&permit, &abstraction_draft(fact), None)
            .await?
            .memory_id;

        // The bulk writes a Perspective whose only grounding is the
        // Abstraction a concurrent forget is about to cool.
        let mut bulk = pool.begin().await?;
        sqlx::query(COARSE).execute(&mut *bulk).await?;
        insert_facts(&mut bulk, owner_id, DECIDE_AFTER + 1).await?;
        assert_eq!(
            pin_lock_state(&mut bulk).await?.0.as_deref(),
            Some("coarse")
        );
        let perspective = insert_derived(
            &mut bulk,
            owner_id,
            "perspective",
            &[abstraction.into_inner()],
        )
        .await?;

        let done = AtomicBool::new(false);
        let forget = async {
            let forgot = MemoryAuthoringPort::forget_memory(&pg, &permit, abstraction).await;
            done.store(true, Ordering::SeqCst);
            forgot
        };
        let driver = async {
            wait_for_lock_waiter(pool).await?;
            tokio::time::sleep(Duration::from_millis(300)).await;
            assert!(
                !done.load(Ordering::SeqCst),
                "forget must wait for the bulk"
            );
            bulk.commit().await?;
            TestResult::Ok(())
        };
        let (forget_result, driver_result) = tokio::join!(forget, driver);
        driver_result?;
        let error = forget_result.expect_err("forget sees the committed Perspective");
        assert!(
            matches!(error, StorageError::ConstraintViolation(ref message)
                if message.contains("ungrounded")),
            "{error}"
        );

        let hot: Vec<Uuid> =
            sqlx::query_scalar("SELECT t FROM proxima_core.memory WHERE t = ANY($1) ORDER BY t")
                .bind(vec![abstraction.into_inner(), perspective])
                .fetch_all(pool)
                .await?;
        assert_eq!(hot, vec![abstraction.into_inner(), perspective]);
        assert_eq!(ungrounded_memories(pool).await?, 0);
        Ok(())
    }
    .await;
    drop_db(&db_name).await.expect("drop test database");
    result.expect("forget against a bulk transaction");
}

#[tokio::test]
async fn concurrent_erase_waits_for_the_bulk_transaction() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    let result: TestResult = async {
        let pg = engine_db(&db_name).await?;
        let (owner, permit, first) = seed_owner(&pg).await?;
        let owner_id = owner.stored_owner_id();
        let pool = pg.pool_for_tests();
        let context = pg.host_state_erase_context()?;
        let sidecars = core_pg_sidecars();

        // Bulk first: the erase waits, then runs against the committed pin,
        // as if the two had run one after the other.
        let mut bulk = pool.begin().await?;
        sqlx::query(COARSE).execute(&mut *bulk).await?;
        insert_facts(&mut bulk, owner_id, DECIDE_AFTER + 1).await?;
        let pinned =
            insert_derived(&mut bulk, owner_id, "abstraction", &[first.into_inner()]).await?;
        let done = AtomicBool::new(false);
        let erase = async {
            let mut tx = pool.begin().await?;
            erase_memory(&mut tx, &sidecars, &context, &owner, first.into_inner()).await?;
            tx.commit().await?;
            done.store(true, Ordering::SeqCst);
            TestResult::Ok(())
        };
        let driver = async {
            wait_for_lock_waiter(pool).await?;
            tokio::time::sleep(Duration::from_millis(300)).await;
            assert!(!done.load(Ordering::SeqCst), "erase must wait for the bulk");
            bulk.commit().await?;
            TestResult::Ok(())
        };
        let (erase_result, driver_result) = tokio::join!(erase, driver);
        driver_result?;
        erase_result?;
        let origins: Vec<Uuid> =
            sqlx::query_scalar("SELECT origins FROM proxima_core.memory WHERE t = $1")
                .bind(pinned)
                .fetch_one(pool)
                .await?;
        assert_eq!(
            origins,
            vec![first.into_inner()],
            "erase never rewrites a pin"
        );
        let witnessed: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM proxima_core.erased_pin_target WHERE t = $1)",
        )
        .bind(first.into_inner())
        .fetch_one(pool)
        .await?;
        assert!(witnessed);

        // Erase first: the bulk's lock waits for it and then sees the witness.
        let second = pg
            .ingest_fact_atomic(&permit, &fact_draft(), None)
            .await?
            .memory_id;
        let live = pg
            .ingest_fact_atomic(&permit, &fact_draft(), None)
            .await?
            .memory_id;
        let mut erase = pool.begin().await?;
        erase_memory(&mut erase, &sidecars, &context, &owner, second.into_inner()).await?;
        let done = AtomicBool::new(false);
        let bulk = async {
            let mut tx = pool.begin().await?;
            sqlx::query(COARSE).execute(&mut *tx).await?;
            done.store(true, Ordering::SeqCst);
            insert_facts(&mut tx, owner_id, DECIDE_AFTER + 1).await?;
            let pinned = insert_derived(
                &mut tx,
                owner_id,
                "abstraction",
                &[live.into_inner(), second.into_inner()],
            )
            .await;
            tx.rollback().await?;
            Result::<_, Box<dyn Error>>::Ok(pinned)
        };
        let driver = async {
            wait_for_lock_waiter(pool).await?;
            tokio::time::sleep(Duration::from_millis(300)).await;
            assert!(
                !done.load(Ordering::SeqCst),
                "the coarse lock must wait for the erase"
            );
            erase.commit().await?;
            TestResult::Ok(())
        };
        let (bulk_result, driver_result) = tokio::join!(bulk, driver);
        driver_result?;
        let error = bulk_result?.expect_err("the bulk sees the erased target");
        let database = error.as_database_error().expect("database refusal");
        assert_eq!(database.code().as_deref(), Some("23503"), "{error}");
        assert!(
            database
                .message()
                .contains(&second.into_inner().to_string()),
            "{error}"
        );
        Ok(())
    }
    .await;
    drop_db(&db_name).await.expect("drop test database");
    result.expect("erase against a bulk transaction");
}

fn deadlocked(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .is_some_and(|database| database.code().as_deref() == Some("40P01"))
}

/// `LOCK` takes its tables one at a time. In the documented order a writer
/// that takes `cooled` then `memory` (forget, erase) finds its second table
/// free and finishes first; one that takes `memory` then `cooled`
/// (hydration, owner erase) deadlocks with it, and Postgres aborts one side.
#[tokio::test]
async fn documented_lock_order_meets_lifecycle_writers() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    let result: TestResult = async {
        let pg = engine_db(&db_name).await?;
        let pool = pg.pool_for_tests();

        let mut writer = pool.begin().await?;
        sqlx::query("DELETE FROM proxima_core.cooled WHERE false")
            .execute(&mut *writer)
            .await?;
        let locked = AtomicBool::new(false);
        let bulk = async {
            let mut tx = pool.begin().await?;
            sqlx::query(COARSE).execute(&mut *tx).await?;
            locked.store(true, Ordering::SeqCst);
            tx.rollback().await?;
            TestResult::Ok(())
        };
        let driver = async {
            wait_for_lock_waiter(pool).await?;
            sqlx::query("DELETE FROM proxima_core.memory WHERE false")
                .execute(&mut *writer)
                .await?;
            assert!(
                !locked.load(Ordering::SeqCst),
                "the lock waits for the writer"
            );
            writer.commit().await?;
            TestResult::Ok(())
        };
        let (bulk_result, driver_result) = tokio::join!(bulk, driver);
        driver_result?;
        bulk_result?;

        let mut writer = pool.begin().await?;
        sqlx::query("DELETE FROM proxima_core.memory WHERE false")
            .execute(&mut *writer)
            .await?;
        let bulk = async {
            let mut tx = pool.begin().await?;
            let locked = sqlx::query(COARSE).execute(&mut *tx).await;
            tx.rollback().await?;
            Result::<_, Box<dyn Error>>::Ok(locked)
        };
        let driver = async {
            wait_for_lock_waiter(pool).await?;
            let wrote = sqlx::query("DELETE FROM proxima_core.cooled WHERE false")
                .execute(&mut *writer)
                .await;
            writer.rollback().await?;
            Result::<_, Box<dyn Error>>::Ok(wrote)
        };
        let (bulk_result, driver_result) = tokio::join!(bulk, driver);
        let (locked, wrote) = (bulk_result?, driver_result?);
        match (&locked, &wrote) {
            (Err(error), Ok(_)) | (Ok(_), Err(error)) => assert!(deadlocked(error), "{error}"),
            _ => panic!("exactly one side is aborted: {locked:?} / {wrote:?}"),
        }
        Ok(())
    }
    .await;
    drop_db(&db_name).await.expect("drop test database");
    result.expect("lock order against lifecycle writers");
}

/// The issue's acceptance: 100,000 Memories in one transaction on default
/// lock-table settings, as the platform role under enforced owner RLS.
#[tokio::test]
async fn hundred_thousand_memories_commit_under_the_coarse_lock() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    let result: TestResult = async {
        migrated_db(&db_name).await?;
        let (_, platform_url) = split_role_urls(&db_name).await?;
        let platform = PgPool::connect(&platform_url).await?;
        let slots: i64 = sqlx::query_scalar(
            "SELECT current_setting('max_locks_per_transaction')::bigint
                  * (current_setting('max_connections')::bigint
                     + current_setting('max_prepared_transactions')::bigint)",
        )
        .fetch_one(&platform)
        .await?;
        assert!(
            slots < 100_000,
            "{slots} lock slots: per-target locks would not overflow this server"
        );

        let owner_id = Uuid::now_v7();
        let mut tx = platform.begin().await?;
        sqlx::query("SELECT set_config('app.proxima_scope', 'platform', true)")
            .execute(&mut *tx)
            .await?;
        sqlx::query(COARSE).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO proxima_core.owners (owner_id, kind) VALUES ($1, 'personal')")
            .bind(owner_id)
            .execute(&mut *tx)
            .await?;
        for _ in 0..10 {
            insert_facts(&mut tx, owner_id, 9_900).await?;
        }
        let facts: Vec<Uuid> = sqlx::query_scalar(
            "SELECT t FROM proxima_core.memory WHERE owner_id = $1 ORDER BY t LIMIT 2000",
        )
        .bind(owner_id)
        .fetch_all(&mut *tx)
        .await?;
        for pair in facts.chunks(2) {
            insert_derived(&mut tx, owner_id, "abstraction", pair).await?;
        }
        let (mode, advisory) = pin_lock_state(&mut tx).await?;
        assert_eq!(mode.as_deref(), Some("coarse"));
        assert_eq!(advisory, DECIDE_AFTER);
        tx.commit().await?;

        let mut tx = platform.begin().await?;
        sqlx::query("SELECT set_config('app.proxima_scope', 'platform', true)")
            .execute(&mut *tx)
            .await?;
        let committed: i64 =
            sqlx::query_scalar("SELECT count(*) FROM proxima_core.memory WHERE owner_id = $1")
                .bind(owner_id)
                .fetch_one(&mut *tx)
                .await?;
        tx.rollback().await?;
        assert_eq!(committed, 100_000);
        platform.close().await;
        Ok(())
    }
    .await;
    drop_db(&db_name).await.expect("drop test database");
    result.expect("bulk insert under the coarse lock");
}

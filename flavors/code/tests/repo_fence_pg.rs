//! The repository lifecycle fence, under real interleavings.
//!
//! Every test here stages its race with a lock and waits for the database
//! to say a backend is blocked. Nothing sleeps: a sleep would make these
//! pass on a fast machine and flake on a loaded one, and the thing being
//! pinned is an ordering, which a clock cannot observe.

mod common;

use common::{code_pg_sidecars, migrated_db, test_owner};
use proxima_code::testkit::{
    build_engine, erase_repo, ingest_commit, lock_repo_fence_exclusive_tx, register_repo,
};
use proxima_code::{CodeFlavorStore, CommitV1, RepoScope};
use proxima_core::{AuthPath, AuthzContext, Owner};
use proxima_pg_testkit::{db_url, drop_db};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

/// Every `proxima_code` row filed under `repo_id`, whatever table it lives
/// in, counted off the catalog rather than off a list.
///
/// A hand-written list of tables is the thing this test exists to distrust:
/// a new repo-scoped sidecar would be added to the schema and not to the
/// list, and the census would report zero for a repository that still has
/// rows. `information_schema` knows which tables carry a `repo_id`, and
/// `query_to_xml` is how one static statement counts across all of them.
const REPO_ROW_CENSUS_SQL: &str = "\
SELECT coalesce(sum(
           (xpath(
               '/row/c/text()',
               query_to_xml(
                   format('SELECT count(*) AS c FROM %I.%I WHERE repo_id = %L',
                          table_schema, table_name, $1::text),
                   false, true, '')
           ))[1]::text::bigint
       ), 0)::bigint
  FROM information_schema.columns
 WHERE table_schema = 'proxima_code' AND column_name = 'repo_id'";

/// Backends blocked on an advisory lock in this database.
///
/// The fence IS an advisory lock, and these tests each run in their own
/// database, so an ungranted advisory lock here is this test's writer
/// waiting on this test's fence and nothing else. A row-lock wait registers
/// as a `transactionid` lock whose `pg_locks.database` is NULL, so it cannot
/// be mistaken for one.
const ADVISORY_WAITERS_SQL: &str = "\
SELECT count(*)::bigint
  FROM pg_locks
 WHERE locktype = 'advisory'
   AND NOT granted
   AND database = (SELECT oid FROM pg_database WHERE datname = current_database())";

/// Backends blocked on any lock in this database, `pg_stat_activity`'s
/// answer — the only one that sees a row-lock wait.
const LOCK_WAITERS_SQL: &str = "\
SELECT count(*)::bigint
  FROM pg_stat_activity
 WHERE datname = current_database()
   AND wait_event_type = 'Lock'";

/// The erase's own repo-row lock, held by the test so the erase parks
/// between taking its fence and reading its footprint.
const HOLD_REPO_ROW_SQL: &str = "\
SELECT repo_id FROM proxima_code.repos
 WHERE owner_kind = $1 AND owner_id = $2 AND repo_id = $3
   FOR UPDATE";

fn commit_payload(repo_id: Uuid, sha: &str) -> CommitV1 {
    let now = time::OffsetDateTime::now_utc();
    CommitV1 {
        repo_id,
        sha: sha.to_string(),
        parents: Vec::new(),
        author_name: "Fence".to_string(),
        author_email: "fence@example.test".to_string(),
        author_time: now,
        committer_name: "Fence".to_string(),
        committer_email: "fence@example.test".to_string(),
        committer_time: now,
        message: format!("commit {sha}"),
    }
}

async fn register(
    pool: &sqlx::PgPool,
    owner: &Owner,
    repo_id: Uuid,
) -> Result<(), Box<dyn std::error::Error>> {
    register_repo(
        pool,
        owner,
        repo_id,
        &format!("/tmp/proxima-repo-fence-{repo_id}"),
        "fence fixture",
        &RepoScope::default(),
    )
    .await?;
    Ok(())
}

async fn census(pool: &sqlx::PgPool, repo_id: Uuid) -> i64 {
    sqlx::query_scalar(REPO_ROW_CENSUS_SQL)
        .bind(repo_id.to_string())
        .fetch_one(pool)
        .await
        .expect("repo row census")
}

/// Which wait the test is asking about.
#[derive(Clone, Copy)]
enum Probe {
    /// Blocked on the fence itself.
    Advisory,
    /// Blocked on anything, including the barrier's row lock.
    AnyLock,
}

/// Poll `probe` until it reports at least `at_least`, or give up.
///
/// The bound is a bound on the TEST, not a timing assumption: the condition
/// is "the database says a backend is blocked", which is true or false at
/// each poll and never becomes true by waiting longer than the interleaving
/// needs. A machine slow enough to exhaust this has failed to set the race
/// up at all, which the assertion then says.
async fn wait_for_count(pool: &sqlx::PgPool, probe: Probe, at_least: i64) -> bool {
    for _ in 0..400 {
        // Two literal statements rather than one statement-shaped argument:
        // the probe SQL stays a constant the reader can see at the call.
        let seen: i64 = match probe {
            Probe::Advisory => {
                sqlx::query_scalar(ADVISORY_WAITERS_SQL)
                    .fetch_one(pool)
                    .await
            }
            Probe::AnyLock => sqlx::query_scalar(LOCK_WAITERS_SQL).fetch_one(pool).await,
        }
        .expect("lock-wait probe");
        if seen >= at_least {
            return true;
        }
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    false
}

/// A second, independent connection to the same migrated database, wired
/// the way the suite wires the first one.
///
/// Its own pool on purpose: a task that blocks on the fence holds a
/// connection for as long as it blocks, and taking that connection from the
/// pool the test's own probes run on would starve the probes.
async fn connect_storage(db_name: &str) -> Result<PgStorage, String> {
    Ok(PgStorage::connect(&db_url(db_name))
        .await
        .map_err(|err| err.to_string())?
        .with_sidecars(code_pg_sidecars())
        .with_flavors(&proxima_code::schema_registry()))
}

/// One ingest of one commit into `repo_id`, on its own pool, spawned so it
/// can block.
fn spawn_ingest(
    db_name: String,
    owner: Owner,
    repo_id: Uuid,
    sha: &str,
) -> tokio::task::JoinHandle<Result<proxima_core::verbs::fact_ingest::FactIngestOutcome, String>> {
    let payload = commit_payload(repo_id, sha);
    tokio::spawn(async move {
        let pg = connect_storage(&db_name).await?;
        let store = CodeFlavorStore::from_backend_pool_for_tests(pg.pool_for_tests().clone());
        let engine = build_engine(pg.clone());
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        ingest_commit(
            &engine,
            &authz,
            &store,
            owner,
            &payload,
            time::OffsetDateTime::now_utc(),
        )
        .await
        .map_err(|err| err.to_string())
    })
}

/// Erase first: the ingest waits on the fence, and what it finds when it
/// gets it is a repository that is gone.
///
/// The erase is parked exactly where the fence has to be taken for any of
/// this to work — after the fence, before the footprint — by holding the
/// repo row it locks one statement later. Without the fence the ingest
/// would not be waiting at all: it would commit a `commit_v1` row for a
/// repository whose footprint had already been computed, and the erase
/// would succeed while leaving it behind. That is proxima-docs #37.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_same_repo_ingest_waits_for_the_erase_and_is_then_refused() {
    let (db_name, pg) = migrated_db().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pool = pg.pool_for_tests();
        let owner = test_owner();
        let repo_id = Uuid::now_v7();
        register(pool, &owner, repo_id).await?;

        let store = CodeFlavorStore::from_backend_pool_for_tests(pool.clone());
        let engine = build_engine(pg.clone());
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        ingest_commit(
            &engine,
            &authz,
            &store,
            owner,
            &commit_payload(repo_id, "0000000000000000000000000000000000000001"),
            time::OffsetDateTime::now_utc(),
        )
        .await?;
        assert!(census(pool, repo_id).await > 0, "the fixture wrote rows");

        // The barrier: hold the repo row the erase locks immediately after
        // its fence.
        let barrier_pool = sqlx::PgPool::connect(&db_url(&db_name)).await?;
        let mut barrier = barrier_pool.begin().await?;
        let (kind, owner_id) = owner.columns();
        sqlx::query(HOLD_REPO_ROW_SQL)
            .bind(kind)
            .bind(owner_id)
            .bind(repo_id)
            .fetch_all(&mut *barrier)
            .await?;

        let erase_db = db_name.clone();
        let erase = tokio::spawn(async move {
            let pg = connect_storage(&erase_db).await?;
            let store = CodeFlavorStore::from_backend_pool_for_tests(pg.pool_for_tests().clone());
            erase_repo(&store, &owner, repo_id)
                .await
                .map_err(|err| err.to_string())
        });
        assert!(
            wait_for_count(pool, Probe::AnyLock, 1).await,
            "the erase never parked on the repo row; the interleaving did not set up"
        );

        // With the erase parked ON THE FENCE IT ALREADY HOLDS, a
        // same-repository ingest must wait for it — on the advisory lock,
        // which is what says the fence is doing the waiting and not some
        // row it happened to touch.
        let ingest = spawn_ingest(
            db_name.clone(),
            owner,
            repo_id,
            "0000000000000000000000000000000000000002",
        );
        assert!(
            wait_for_count(pool, Probe::Advisory, 1).await,
            "the same-repo ingest did not wait on the repository fence"
        );
        assert!(!ingest.is_finished(), "the ingest ran past the fence");

        barrier.rollback().await?;
        barrier_pool.close().await;

        erase.await?.expect("the erase completes once unparked");
        let refusal = ingest
            .await?
            .expect_err("an ingest into an erased repository must be refused");
        assert!(
            refusal.contains(&repo_id.to_string()) && refusal.contains("repo not found"),
            "the refusal must name the repository as not found; got {refusal}"
        );
        assert_eq!(
            census(pool, repo_id).await,
            0,
            "a successful erase leaves no repo-scoped row behind"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("a_same_repo_ingest_waits_for_the_erase_and_is_then_refused failed");
}

/// Ingest first: the erase waits, and then erases what the ingest wrote.
///
/// The writer here holds the fence the way an admission holds it and
/// commits a `commit_v1` row that no earlier snapshot could have seen. The
/// erase must not compute its footprint until that row is committed —
/// which is what taking the fence BEFORE the first read buys, and what the
/// census then checks on real rows.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_erase_waits_for_an_in_flight_same_repo_write_and_sweeps_it() {
    let (db_name, pg) = migrated_db().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pool = pg.pool_for_tests();
        let owner = test_owner();
        let repo_id = Uuid::now_v7();
        register(pool, &owner, repo_id).await?;

        // The writer: the fence first, then a repo-scoped admission, then
        // commit — the production order, staged so the erase arrives in the
        // middle of it.
        let writer_pool = sqlx::PgPool::connect(&db_url(&db_name)).await?;
        let mut writer = writer_pool.begin().await?;
        lock_repo_fence_exclusive_tx(&mut writer, &owner, repo_id).await?;
        let late_t = Uuid::now_v7();
        insert_commit_row(&mut writer, &owner, repo_id, late_t).await?;

        let erase_db = db_name.clone();
        let erase = tokio::spawn(async move {
            let pg = connect_storage(&erase_db).await?;
            let store = CodeFlavorStore::from_backend_pool_for_tests(pg.pool_for_tests().clone());
            erase_repo(&store, &owner, repo_id)
                .await
                .map_err(|err| err.to_string())
        });
        assert!(
            wait_for_count(pool, Probe::Advisory, 1).await,
            "the erase did not wait on the repository fence a writer was holding"
        );
        assert!(!erase.is_finished(), "the erase ran past the fence");

        writer.commit().await?;
        writer_pool.close().await;

        let receipt = erase.await?.expect("the erase completes once unparked");
        assert!(
            receipt.repo_record_deleted,
            "the erase deleted the repository registration"
        );
        assert_eq!(
            census(pool, repo_id).await,
            0,
            "the footprint must include the row committed while the erase waited"
        );
        let survivor: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory WHERE t = $1")
                .bind(late_t)
                .fetch_one(pool)
                .await?;
        assert_eq!(survivor, 0, "and the admission behind it");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("an_erase_waits_for_an_in_flight_same_repo_write_and_sweeps_it failed");
}

/// The fence is per repository, not per owner.
///
/// An erase parked on repository A holds A's key and nothing else, so a
/// write into B goes through while it waits. A fence keyed on the owner —
/// or on the one constant source id the local-git lane uses — would stop
/// this ingest dead, which is why neither of those is the key.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_erase_of_one_repository_does_not_fence_another() {
    let (db_name, pg) = migrated_db().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pool = pg.pool_for_tests();
        let owner = test_owner();
        let erased = Uuid::now_v7();
        let untouched = Uuid::now_v7();
        register(pool, &owner, erased).await?;
        register(pool, &owner, untouched).await?;

        let barrier_pool = sqlx::PgPool::connect(&db_url(&db_name)).await?;
        let mut barrier = barrier_pool.begin().await?;
        let (kind, owner_id) = owner.columns();
        sqlx::query(HOLD_REPO_ROW_SQL)
            .bind(kind)
            .bind(owner_id)
            .bind(erased)
            .fetch_all(&mut *barrier)
            .await?;

        let erase_db = db_name.clone();
        let erase = tokio::spawn(async move {
            let pg = connect_storage(&erase_db).await?;
            let store = CodeFlavorStore::from_backend_pool_for_tests(pg.pool_for_tests().clone());
            erase_repo(&store, &owner, erased)
                .await
                .map_err(|err| err.to_string())
        });
        assert!(
            wait_for_count(pool, Probe::AnyLock, 1).await,
            "the erase never parked; the interleaving did not set up"
        );

        // Held fence on `erased`, and this lands anyway.
        let other = spawn_ingest(
            db_name.clone(),
            owner,
            untouched,
            "0000000000000000000000000000000000000003",
        );
        other
            .await?
            .expect("an ingest into a different repository must not wait on this erase");
        assert!(
            census(pool, untouched).await > 0,
            "the other repository's write landed while the erase was parked"
        );

        barrier.rollback().await?;
        barrier_pool.close().await;
        erase.await?.expect("the erase completes once unparked");
        assert_eq!(census(pool, erased).await, 0);
        assert!(census(pool, untouched).await > 0, "and B survived it");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("an_erase_of_one_repository_does_not_fence_another failed");
}

/// One admitted `commit_v1` and the `proxima_core` rows behind it, written
/// by hand so the write can be held open across the erase's arrival.
///
/// The stamp and the row it promises land in one transaction: a memory row
/// that names a sidecar table it has no row in is refused at COMMIT.
async fn insert_commit_row(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    owner: &Owner,
    repo_id: Uuid,
    t: Uuid,
) -> Result<(), Box<dyn std::error::Error>> {
    use proxima_core::FactPayload;

    let owner_id = owner.stored_owner_id();
    sqlx::query(
        "INSERT INTO proxima_core.owners (owner_id, kind)
         VALUES ($1, 'personal') ON CONFLICT DO NOTHING",
    )
    .bind(owner_id)
    .execute(&mut **tx)
    .await?;
    let handle = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
         VALUES ($1, 'fact', $2, $3, $4)",
    )
    .bind(handle)
    .bind(CommitV1::SCHEMA_ID)
    .bind(owner_id)
    .bind(t)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.memory (handle, t, kind, owner_id, schema_id, sidecar_tables)
         VALUES ($1, $2, 'fact', $3, $4, $5)",
    )
    .bind(handle)
    .bind(t)
    .bind(owner_id)
    .bind(CommitV1::SCHEMA_ID)
    .bind(vec!["proxima_code.commit_v1".to_string()])
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_code.commit_v1
            (t, repo_id, sha, parents, author_name, author_email,
             author_time, committer_name, committer_email, committer_time, message)
         VALUES ($1, $2, 'late0001', ARRAY[]::text[], 'Fence', 'fence@example.test',
             now(), 'Fence', 'fence@example.test', now(), 'committed while the erase waited')",
    )
    .bind(t)
    .bind(repo_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

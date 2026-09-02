//! The declared `code-repo` lifecycle scope, under real interleavings.
//!
//! Every test here stages its race with a lock and waits for the database
//! to say a backend is blocked. Nothing sleeps: a sleep would make these
//! pass on a fast machine and flake on a loaded one, and the thing being
//! pinned is an ordering, which a clock cannot observe.
//!
//! The fence these tests hold is the SUBSTRATE's — one key generated from
//! `CODE_REPO_SCOPE_DECL` — so a barrier here takes exactly the lock the
//! Engine takes on an admission, rather than a flavor-side copy of it.

mod common;

use common::{code_pg_sidecars, migrated_db, test_owner};
use proxima::flavor::{lock_scope_fence_exclusive_tx, lock_scope_fence_shared_tx};
use proxima_code::testkit::{build_engine, erase_repo, ingest_commit, register_repo};
use proxima_code::{CODE_REPO_SCOPE, CodeFlavorStore, CommitSummaryV1, CommitV1, RepoScope};
use proxima_core::{
    AbstractionPayload, AuthPath, AuthorDerivedRequestInput, AuthzContext, EdgeEndpoint, Engine,
    EntityKind, InputContractId, MemoryId, MemoryOperatorKind, OperatorId, Owner, ProtocolError,
    SchemaVersion, SidecarPayload,
};
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
        let engine = build_engine(pg.clone());
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        ingest_commit(&engine, &authz, &payload, time::OffsetDateTime::now_utc())
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

        let engine = build_engine(pg.clone());
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        ingest_commit(
            &engine,
            &authz,
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
            refusal.contains(&repo_id.to_string()) && refusal.contains("scope not registered"),
            "the refusal must name the scope as unregistered; got {refusal}"
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
        lock_scope_fence_exclusive_tx(&mut writer, CODE_REPO_SCOPE, &owner, repo_id).await?;
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

/// Two writers into one repository hold the fence at the same time.
///
/// The fence separates writers from the ERASE, not writers from each other,
/// and this is the test that says so. The parked writer takes the shared
/// mode through the substrate helper and stays open; the second writer goes
/// through the production admission path, whose fence is the same key,
/// generated from the same declaration. If the admission took the exclusive
/// mode the second writer would block behind the first, and this test would
/// run out its bound instead of finishing, which is the failure the
/// assertion names.
///
/// The erase then queues behind the writer that is still holding, and
/// sweeps both writers' rows: shared writers are not exempt from the
/// footprint, they are inside it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_same_repo_writers_hold_the_fence_at_once_and_the_erase_sweeps_both() {
    let (db_name, pg) = migrated_db().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pool = pg.pool_for_tests();
        let owner = test_owner();
        let repo_id = Uuid::now_v7();
        register(pool, &owner, repo_id).await?;

        // The owner row, committed, BEFORE anyone parks. First-use owner
        // arbitration is an upsert on `proxima_core.owners`, so a parked
        // writer that created that row would hold a row lock the second
        // writer waits on - a wait that has nothing to do with the fence and
        // would make this test pass or fail for the wrong reason.
        sqlx::query(
            "INSERT INTO proxima_core.owners (owner_id, kind)
             VALUES ($1, 'personal') ON CONFLICT DO NOTHING",
        )
        .bind(owner.stored_owner_id())
        .execute(pool)
        .await?;

        // Writer one: the fence, shared, then a repo-scoped row, then it
        // stops. Held open across the whole of the rest of this test.
        let parked_pool = sqlx::PgPool::connect(&db_url(&db_name)).await?;
        let mut parked = parked_pool.begin().await?;
        lock_scope_fence_shared_tx(&mut parked, CODE_REPO_SCOPE, &owner, repo_id).await?;
        let parked_t = Uuid::now_v7();
        insert_commit_row(&mut parked, &owner, repo_id, parked_t).await?;

        // Writer two, through the real path, into the SAME repository.
        let second = spawn_ingest(
            db_name.clone(),
            owner,
            repo_id,
            "0000000000000000000000000000000000000004",
        );
        // A bound on the test, not a synchronization step: this writer
        // either does not wait on the parked one, in which case it finishes
        // at once, or it waits forever, and no amount of waiting
        // distinguishes "slow" from "blocked on a lock nothing will
        // release".
        let second = tokio::time::timeout(std::time::Duration::from_secs(45), second)
            .await
            .map_err(|_| "the second same-repo writer blocked on the first one's fence")??;
        let second = second.expect("a second writer into a fenced repository is admitted");
        assert!(
            census(pool, repo_id).await >= 1,
            "the second writer committed while the first still held the fence"
        );

        // And the erase, behind the writer that is still holding.
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
            "the erase did not queue behind the writer still holding the fence"
        );
        assert!(!erase.is_finished(), "the erase ran past a held fence");

        parked.commit().await?;
        parked_pool.close().await;

        let receipt = erase.await?.expect("the erase completes once unparked");
        assert!(receipt.repo_record_deleted);
        assert_eq!(
            census(pool, repo_id).await,
            0,
            "both writers' rows are in the footprint"
        );
        let survivors: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.memory WHERE t = ANY($1)",
        )
        .bind(vec![parked_t, second.memory_id.into_inner()])
        .fetch_one(pool)
        .await?;
        assert_eq!(survivors, 0, "and so are the admissions behind them");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("two_same_repo_writers_hold_the_fence_at_once_and_the_erase_sweeps_both failed");
}

/// A host writes a repo-scoped payload straight through `Engine`, and is
/// fenced anyway. **This is the hole the declaration closes.**
///
/// `commit_summary_v1` has no writer in this flavor: nothing in
/// `flavors/code/src` ever admits one, so under the retired flavor-side
/// fence there was no place to put the fence call and no fence was ever
/// taken. A host that wrote a summary through `Engine::author_derived_authorized`
/// raced every repository erase, and could commit a row after the erase had
/// already computed its footprint. Declaring `CODE_REPO_SCOPE` on the
/// payload is the whole of the fix: the fence and the liveness probe are
/// now the Engine's, in the write transaction, before its handle and `t`.
///
/// Both halves are pinned here. The first summary commits before the erase
/// parks and must be SWEPT; the second arrives while the erase holds the
/// fence and must WAIT and then be REFUSED.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_host_write_of_a_scoped_payload_through_the_engine_waits_and_is_refused() {
    let (db_name, pg) = migrated_db().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pool = pg.pool_for_tests();
        let owner = test_owner();
        let repo_id = Uuid::now_v7();
        register(pool, &owner, repo_id).await?;

        // A commit for the summaries to derive from: an Abstraction must
        // pin at least one Fact, which is the Model's rule and not this
        // test's.
        let engine = build_engine(pg.clone());
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        let commit = ingest_commit(
            &engine,
            &authz,
            &commit_payload(repo_id, "0000000000000000000000000000000000000005"),
            time::OffsetDateTime::now_utc(),
        )
        .await?
        .memory_id;

        // The write that gets in first. No flavor code in its path — this is
        // the host's own call.
        let early =
            host_write_commit_summary(&engine, &authz, owner, repo_id, "early0001", commit).await?;
        assert!(
            census(pool, repo_id).await > 0,
            "the host's summary landed before the erase"
        );

        // The barrier: hold the repo row the erase locks immediately after
        // its fence, so the erase parks holding the fence.
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

        // The late host write, on its own pool so it can block.
        let late_db = db_name.clone();
        let late = tokio::spawn(async move {
            let pg = connect_storage(&late_db).await?;
            let engine = build_engine(pg.clone());
            let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
            host_write_commit_summary(&engine, &authz, owner, repo_id, "late00001", commit)
                .await
                .map_err(|err| err.to_string())
        });
        assert!(
            wait_for_count(pool, Probe::Advisory, 1).await,
            "the host's write did not wait on the declared scope fence"
        );
        assert!(!late.is_finished(), "the host write ran past the fence");

        barrier.rollback().await?;
        barrier_pool.close().await;

        erase.await?.expect("the erase completes once unparked");
        let refusal = late
            .await?
            .expect_err("a host write into an erased scope must be refused");
        assert!(
            refusal.contains(&repo_id.to_string()) && refusal.contains("scope not registered"),
            "the refusal must name the scope as unregistered; got {refusal}"
        );
        assert_eq!(
            census(pool, repo_id).await,
            0,
            "the summary the host wrote first is inside the erase's footprint"
        );
        let survivor: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory WHERE t = $1")
                .bind(early.into_inner())
                .fetch_one(pool)
                .await?;
        assert_eq!(survivor, 0, "and so is the admission behind it");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
        .expect("a_host_write_of_a_scoped_payload_through_the_engine_waits_and_is_refused failed");
}

/// One `commit_summary_v1`, written the way a host writes it: the Engine's
/// own authored-derived verb, with no repository check and no fence of its
/// own anywhere in the call.
#[allow(clippy::too_many_arguments)]
async fn host_write_commit_summary(
    engine: &Engine,
    authz: &AuthzContext,
    owner: Owner,
    repo_id: Uuid,
    commit_sha: &str,
    commit: MemoryId,
) -> Result<MemoryId, ProtocolError> {
    let payload = CommitSummaryV1 {
        repo_id,
        commit_sha: commit_sha.to_string(),
        summary: format!("host summary of {commit_sha}"),
        key_files: Vec::new(),
        change_kind: "fix".to_string(),
    };
    let text = payload.summary.clone();
    let origins = [EdgeEndpoint::memory(EntityKind::Fact, commit)];
    let outcome = engine
        .author_derived_authorized(
            authz,
            AuthorDerivedRequestInput {
                memory_id: MemoryId::new(Uuid::now_v7()),
                owner,
                kind: EntityKind::Abstraction,
                text,
                schema_id: <CommitSummaryV1 as AbstractionPayload>::schema_id(),
                schema_version: SchemaVersion::new(CommitSummaryV1::SCHEMA_VERSION),
                operator_kind: MemoryOperatorKind::FtoA,
                operator_id: OperatorId::new(Uuid::new_v5(
                    &Uuid::NAMESPACE_URL,
                    b"proxima-code/tests/repo-fence/host-summary",
                )),
                input_contract_id: InputContractId::new(Uuid::new_v5(
                    &Uuid::NAMESPACE_URL,
                    commit_sha.as_bytes(),
                )),
                model_id: "proxima-code/tests/repo-fence",
                sidecar_payload: SidecarPayload::abstraction(payload),
                derived_from: &origins,
                extra_refs: &[],
                supersedes: None,
                lexical_language: None,
            },
        )
        .await?;
    Ok(outcome.memory_id)
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

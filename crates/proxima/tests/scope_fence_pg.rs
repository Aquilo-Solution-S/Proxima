//! The declared `notebook` lifecycle scope, under real interleavings,
//! erase side.
//!
//! `crates/storage-pg/tests/declared_scope_fence.rs` pins the admission
//! side against a fence a test holds by hand. What it cannot pin is the
//! scope ERASE: that it takes the fence before its last read, that a write
//! parked behind it is refused once the scope is gone, that it sweeps a row
//! a writer committed while it waited, that the key is per scope rather
//! than per owner, and that writers share the fence with each other. Those
//! interleavings are here, on the example flavor's notebook scope and its
//! testkit's `erase_notebook_with_scope`.
//!
//! Every test stages its race with a lock and waits for the database to say
//! a backend is blocked. Nothing sleeps as a synchronization step: the thing
//! being pinned is an ordering, which a clock cannot observe.

#[path = "fixtures/example_engine.rs"]
#[allow(dead_code, reason = "each suite uses part of the shared fixture")]
mod example_engine;

use example_engine::{compose, migrated_db};
use proxima::flavor::{lock_scope_fence_exclusive_tx, lock_scope_fence_shared_tx};
use proxima::testkit::SplitRoleDb;
use proxima_core::test_fixtures::{authenticated_context, owner_scope_for_tests};
use proxima_core::{
    AuthPath, AuthzContext, DerivationIdentity, DerivedMemory, Engine, FactPayload, MemoryId,
    MemoryTarget, OperatorId, OwnerId, ProtocolError, SeriesHandle, UserId,
};
use proxima_example::payload::NOTEBOOK_SCOPE;
use proxima_example::testkit::{
    NotebookEraseReceipt, erase_notebook_with_scope, register_notebook, remember_note,
};
use proxima_example::{DigestV1, NoteV1};
use uuid::Uuid;

/// Every `proxima_example` row filed under `notebook_id`, whatever table it
/// lives in, counted off the catalog rather than off a list: a new
/// notebook-scoped sidecar missing from a hand-written list would report
/// zero for a notebook that still has rows. One static statement counts
/// across every table `information_schema` says carries a `notebook_id`.
const NOTEBOOK_ROW_CENSUS_SQL: &str = "\
SELECT coalesce(sum(
           (xpath(
               '/row/c/text()',
               query_to_xml(
                   format('SELECT count(*) AS c FROM %I.%I WHERE notebook_id = %L',
                          table_schema, table_name, $1::text),
                   false, true, '')
           ))[1]::text::bigint
       ), 0)::bigint
  FROM information_schema.columns
 WHERE table_schema = 'proxima_example' AND column_name = 'notebook_id'
   AND table_name <> 'notebooks'";

/// Backends blocked on an advisory lock in this database. The fence IS an
/// advisory lock and each test has its own database, so an ungranted one
/// here is this test's writer waiting on this test's fence. A row-lock wait
/// registers as a `transactionid` lock with a NULL database, so it cannot
/// be mistaken for one.
const ADVISORY_WAITERS_SQL: &str = "\
SELECT count(*)::bigint
  FROM pg_locks
 WHERE locktype = 'advisory'
   AND NOT granted
   AND database = (SELECT oid FROM pg_database WHERE datname = current_database())";

/// Backends blocked on any lock in this database: the only probe that sees
/// a row-lock wait.
const LOCK_WAITERS_SQL: &str = "\
SELECT count(*)::bigint
  FROM pg_stat_activity
 WHERE datname = current_database()
   AND wait_event_type = 'Lock'";

/// The erase's own registry-row lock, held by the test so the erase parks
/// in its retirement: every page erased, the fence held, the last read not
/// yet taken.
const HOLD_NOTEBOOK_ROW_SQL: &str = "\
SELECT notebook_id FROM proxima_example.notebooks
 WHERE owner_id = $1 AND notebook_id = $2
   FOR UPDATE";

const COUNT_MEMORY_SQL: &str = "SELECT count(*)::bigint FROM proxima_core.memory WHERE t = ANY($1)";

fn owner() -> OwnerId {
    OwnerId::from(UserId::new(Uuid::now_v7()))
}

fn authz(owner: OwnerId) -> AuthzContext {
    authenticated_context(AuthzContext::single_owner(&owner, AuthPath::HostBearer))
}

fn note(notebook_id: Uuid, key: &str) -> NoteV1 {
    NoteV1 {
        notebook_id,
        key: key.to_owned(),
        body: format!("note {key}"),
        written_at: time::OffsetDateTime::now_utc(),
    }
}

/// Which wait the test is asking about.
#[derive(Clone, Copy)]
enum Probe {
    /// Blocked on the fence itself.
    Advisory,
    /// Blocked on anything, including the barrier's row lock.
    AnyLock,
}

/// One test's database, its admin pool, and an Engine on pools of its own.
struct Fixture {
    admin: sqlx::PgPool,
    engine: Engine,
    db: SplitRoleDb,
}

impl Fixture {
    async fn new(prefix: &str) -> Self {
        let db = migrated_db(prefix).await;
        proxima_storage_pg::PgMigrationStorage::connect_with_config(
            db.platform_url(),
            proxima_storage_pg::PgPoolConfig::default(),
            proxima_storage_pg::PgTuning::default(),
        )
        .await
        .expect("migration connection")
        .initialize_store_number(0)
        .await
        .expect("store number");
        let engine = compose(db.runtime_url(), db.platform_url(), |_| {}).await;
        let admin = sqlx::PgPool::connect(&db.admin_url())
            .await
            .expect("admin connection");
        Self { admin, engine, db }
    }

    async fn notebook(&self, owner: OwnerId) -> Uuid {
        register_notebook(
            &self.admin,
            &owner_scope_for_tests(&[owner]),
            owner,
            "fence fixture",
        )
        .await
        .expect("the fixture notebook registers")
    }

    async fn census(&self, notebook_id: Uuid) -> i64 {
        sqlx::query_scalar(NOTEBOOK_ROW_CENSUS_SQL)
            .bind(notebook_id.to_string())
            .fetch_one(&self.admin)
            .await
            .expect("notebook row census")
    }

    async fn surviving_memory(&self, ts: Vec<Uuid>) -> i64 {
        sqlx::query_scalar(COUNT_MEMORY_SQL)
            .bind(ts)
            .fetch_one(&self.admin)
            .await
            .expect("memory census")
    }

    /// A barrier transaction holding the notebook's registry row.
    async fn hold_notebook_row(
        &self,
        owner: OwnerId,
        notebook_id: Uuid,
    ) -> (sqlx::PgPool, sqlx::Transaction<'static, sqlx::Postgres>) {
        let pool = sqlx::PgPool::connect(&self.db.admin_url())
            .await
            .expect("barrier connection");
        let mut barrier = pool.begin().await.expect("barrier transaction");
        sqlx::query(HOLD_NOTEBOOK_ROW_SQL)
            .bind(owner.into_inner())
            .bind(notebook_id)
            .fetch_all(&mut *barrier)
            .await
            .expect("hold the notebook row");
        (pool, barrier)
    }

    /// A fresh connection that takes the notebook fence itself, the way a
    /// writer holds it.
    async fn writer(&self) -> (sqlx::PgPool, sqlx::Transaction<'static, sqlx::Postgres>) {
        let pool = sqlx::PgPool::connect(&self.db.admin_url())
            .await
            .expect("writer connection");
        let tx = pool.begin().await.expect("writer transaction");
        (pool, tx)
    }

    /// Poll `probe` until it reports at least `at_least` while `task` is
    /// still running.
    ///
    /// The condition is "the database says a backend is blocked", which no
    /// amount of waiting makes true if the race did not set up. An `Err`
    /// says which way it did not: the task ended first, and the error
    /// carries the task's own outcome (its error, or its panic message), or
    /// the deadline passed with the task still running and nothing blocked.
    /// A task that failed before it reached the fence is then reported at
    /// its cause, not as a wait the database never showed.
    async fn wait_for<T: std::fmt::Debug>(
        &self,
        probe: Probe,
        at_least: i64,
        task: &mut tokio::task::JoinHandle<T>,
    ) -> Result<(), String> {
        let deadline = tokio::time::Instant::now() + WAIT_DEADLINE;
        loop {
            // Two literal statements rather than one statement-shaped
            // argument: the probe SQL stays a constant visible at the call.
            let seen: i64 = match probe {
                Probe::Advisory => {
                    sqlx::query_scalar(ADVISORY_WAITERS_SQL)
                        .fetch_one(&self.admin)
                        .await
                }
                Probe::AnyLock => {
                    sqlx::query_scalar(LOCK_WAITERS_SQL)
                        .fetch_one(&self.admin)
                        .await
                }
            }
            .expect("lock-wait probe");
            if seen >= at_least {
                return Ok(());
            }
            if task.is_finished() {
                return Err(format!(
                    "the operation ended before it waited: {}",
                    task_outcome(task.await)
                ));
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(format!(
                    "no wait observed within {WAIT_DEADLINE:?}; the operation is still running"
                ));
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    /// An Engine on pools of its own, composed BEFORE its task is spawned:
    /// a boot that fails or stalls does so here, at its cause, and the task
    /// starts at the operation the test is about.
    async fn task_engine(&self) -> Engine {
        compose(self.db.runtime_url(), self.db.platform_url(), |_| {}).await
    }

    /// One note written through its own Engine on its own pools, spawned so
    /// it can block.
    async fn spawn_note(
        &self,
        owner: OwnerId,
        notebook_id: Uuid,
        key: &'static str,
    ) -> tokio::task::JoinHandle<Result<MemoryId, String>> {
        let engine = self.task_engine().await;
        tokio::spawn(async move {
            remember_note(&engine, &authz(owner), owner, &note(notebook_id, key))
                .await
                .map_err(|err| err.to_string())
        })
    }

    /// One notebook erase on its own Engine and pools, spawned so it can
    /// block.
    async fn spawn_erase(
        &self,
        owner: OwnerId,
        notebook_id: Uuid,
    ) -> tokio::task::JoinHandle<Result<NotebookEraseReceipt, String>> {
        let engine = self.task_engine().await;
        let pool = sqlx::PgPool::connect(self.db.runtime_url())
            .await
            .expect("erase connection");
        tokio::spawn(async move {
            erase_notebook_with_scope(&engine, &authz(owner), &pool, owner, notebook_id)
                .await
                .map_err(|err| err.to_string())
        })
    }
}

/// How long [`Fixture::wait_for`] polls. A bound on the TEST, well inside
/// nextest's slow-timeout, and long enough that a loaded runner reaching
/// the fence late is not mistaken for one that never reaches it.
const WAIT_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

/// A spawned task's outcome as text: its value, or its panic message.
fn task_outcome<T: std::fmt::Debug>(joined: Result<T, tokio::task::JoinError>) -> String {
    match joined {
        Ok(value) => format!("{value:?}"),
        Err(error) if error.is_panic() => {
            let payload = error.into_panic();
            payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| {
                    payload
                        .downcast_ref::<&str>()
                        .map(|text| (*text).to_owned())
                })
                .map_or_else(|| "panicked".to_owned(), |text| format!("panicked: {text}"))
        }
        Err(error) => format!("{error}"),
    }
}

fn assert_scope_refusal(refusal: &str, notebook_id: Uuid) {
    assert!(
        refusal.contains(&notebook_id.to_string()) && refusal.contains("scope not registered"),
        "the refusal must name the scope as unregistered; got {refusal}"
    );
}

/// Erase first: the write waits on the fence, and finds the notebook gone.
///
/// The erase is parked exactly where the fence must already be held (every
/// page erased, the retirement's last read not yet taken) by holding the
/// registry row it locks one statement after the fence. Without the fence
/// the write would not wait at all: it would commit a note after the last
/// page, and the erase would retire the notebook and leave it behind.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_same_notebook_write_waits_for_the_erase_and_is_then_refused() {
    let fx = Fixture::new("proxima_scope_fence").await;
    let owner = owner();
    let notebook_id = fx.notebook(owner).await;
    remember_note(
        &fx.engine,
        &authz(owner),
        owner,
        &note(notebook_id, "first"),
    )
    .await
    .expect("the fixture note");
    assert!(fx.census(notebook_id).await > 0, "the fixture wrote rows");

    let (barrier_pool, barrier) = fx.hold_notebook_row(owner, notebook_id).await;
    let mut erase = fx.spawn_erase(owner, notebook_id).await;
    if let Err(why) = fx.wait_for(Probe::AnyLock, 1, &mut erase).await {
        panic!(
            "the erase never parked on the notebook row; the interleaving did not set up: {why}"
        );
    }

    // With the erase parked ON THE FENCE IT ALREADY HOLDS, a same-notebook
    // write must wait on the advisory lock: the fence is doing the waiting,
    // not some row it happened to touch.
    let mut write = fx.spawn_note(owner, notebook_id, "second").await;
    if let Err(why) = fx.wait_for(Probe::Advisory, 1, &mut write).await {
        panic!("the same-notebook write did not wait on the notebook fence: {why}");
    }
    assert!(!write.is_finished(), "the write ran past the fence");

    barrier.rollback().await.expect("release the barrier");
    barrier_pool.close().await;

    erase
        .await
        .expect("erase task")
        .expect("the erase completes once unparked");
    let refusal = write
        .await
        .expect("write task")
        .expect_err("a write into an erased notebook must be refused");
    assert_scope_refusal(&refusal, notebook_id);
    assert_eq!(
        fx.census(notebook_id).await,
        0,
        "a successful erase leaves no notebook-scoped row behind"
    );
}

/// Write first: the erase waits, then erases what the writer committed.
///
/// The writer holds the fence the way an admission holds it and commits a
/// note no earlier snapshot could have seen. The erase's pages cannot see
/// it, so the retirement must not take its last read until it is committed:
/// the read finds it, the attempt re-runs and erases it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_erase_waits_for_an_in_flight_same_notebook_write_and_sweeps_it() {
    let fx = Fixture::new("proxima_scope_fence").await;
    let owner = owner();
    let notebook_id = fx.notebook(owner).await;

    let (writer_pool, mut writer) = fx.writer().await;
    lock_scope_fence_exclusive_tx(&mut writer, NOTEBOOK_SCOPE, &owner, notebook_id)
        .await
        .expect("the writer takes the fence");
    let late_t = Uuid::now_v7();
    insert_note_row(&mut writer, owner, notebook_id, late_t)
        .await
        .expect("the late note");

    let mut erase = fx.spawn_erase(owner, notebook_id).await;
    if let Err(why) = fx.wait_for(Probe::Advisory, 1, &mut erase).await {
        panic!("the erase did not wait on the notebook fence a writer was holding: {why}");
    }
    assert!(!erase.is_finished(), "the erase ran past the fence");

    writer.commit().await.expect("the writer commits");
    writer_pool.close().await;

    let receipt = erase
        .await
        .expect("erase task")
        .expect("the erase completes once unparked");
    assert!(
        receipt.notebook_record_deleted,
        "the erase deleted the notebook registration"
    );
    assert_eq!(
        fx.census(notebook_id).await,
        0,
        "the footprint must include the row committed while the erase waited"
    );
    assert_eq!(
        fx.surviving_memory(vec![late_t]).await,
        0,
        "and the admission behind it"
    );
}

/// The fence is per notebook, not per owner: an erase parked on notebook A
/// holds A's key and nothing else, so a write into B goes through while it
/// waits.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_erase_of_one_notebook_does_not_fence_another() {
    let fx = Fixture::new("proxima_scope_fence").await;
    let owner = owner();
    let erased = fx.notebook(owner).await;
    let untouched = fx.notebook(owner).await;

    let (barrier_pool, barrier) = fx.hold_notebook_row(owner, erased).await;
    let mut erase = fx.spawn_erase(owner, erased).await;
    if let Err(why) = fx.wait_for(Probe::AnyLock, 1, &mut erase).await {
        panic!("the erase never parked; the interleaving did not set up: {why}");
    }

    fx.spawn_note(owner, untouched, "elsewhere")
        .await
        .await
        .expect("write task")
        .expect("a write into a different notebook must not wait on this erase");
    assert!(
        fx.census(untouched).await > 0,
        "the other notebook's write landed while the erase was parked"
    );

    barrier.rollback().await.expect("release the barrier");
    barrier_pool.close().await;
    erase
        .await
        .expect("erase task")
        .expect("the erase completes once unparked");
    assert_eq!(fx.census(erased).await, 0);
    assert!(fx.census(untouched).await > 0, "and B survived it");
}

/// Two writers into one notebook hold the fence at the same time.
///
/// The fence separates writers from the ERASE, not writers from each other.
/// The parked writer takes the shared mode through the substrate helper and
/// stays open; the second writer goes through the Engine's admission, whose
/// fence is the same key, generated from the same declaration. If the
/// admission took the exclusive mode, the second writer would block behind
/// the first and run out the bound instead of finishing.
///
/// The erase then queues behind the writer still holding, and sweeps both
/// writers' rows: shared writers are inside the footprint.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_same_notebook_writers_hold_the_fence_at_once_and_the_erase_sweeps_both() {
    let fx = Fixture::new("proxima_scope_fence").await;
    let owner = owner();
    let notebook_id = fx.notebook(owner).await;

    // The owner row, committed, BEFORE anyone parks: first-use owner
    // arbitration is an upsert on `proxima_core.owners`, and a parked writer
    // that created it would hold a row lock the second writer waits on,
    // which has nothing to do with the fence.
    sqlx::query(
        "INSERT INTO proxima_core.owners (owner_id, kind)
         VALUES ($1, 'personal') ON CONFLICT DO NOTHING",
    )
    .bind(owner.into_inner())
    .execute(&fx.admin)
    .await
    .expect("the owner row");

    let (parked_pool, mut parked) = fx.writer().await;
    lock_scope_fence_shared_tx(&mut parked, NOTEBOOK_SCOPE, &owner, notebook_id)
        .await
        .expect("the parked writer takes the fence, shared");
    let parked_t = Uuid::now_v7();
    insert_note_row(&mut parked, owner, notebook_id, parked_t)
        .await
        .expect("the parked note");

    // A bound on the test, not a synchronization step: the second writer
    // either does not wait on the parked one and finishes at once, or waits
    // on a lock nothing will release.
    let second = fx.spawn_note(owner, notebook_id, "second").await;
    let second = tokio::time::timeout(std::time::Duration::from_secs(45), second)
        .await
        .expect("the second same-notebook writer blocked on the first one's fence")
        .expect("write task")
        .expect("a second writer into a fenced notebook is admitted");
    assert!(
        fx.census(notebook_id).await >= 1,
        "the second writer committed while the first still held the fence"
    );

    let mut erase = fx.spawn_erase(owner, notebook_id).await;
    if let Err(why) = fx.wait_for(Probe::Advisory, 1, &mut erase).await {
        panic!("the erase did not queue behind the writer still holding the fence: {why}");
    }
    assert!(!erase.is_finished(), "the erase ran past a held fence");

    parked.commit().await.expect("the parked writer commits");
    parked_pool.close().await;

    let receipt = erase
        .await
        .expect("erase task")
        .expect("the erase completes once unparked");
    assert!(receipt.notebook_record_deleted);
    assert_eq!(
        fx.census(notebook_id).await,
        0,
        "both writers' rows are in the footprint"
    );
    assert_eq!(
        fx.surviving_memory(vec![parked_t, second.into_inner()])
            .await,
        0,
        "and so are the admissions behind them"
    );
}

/// A host writes a notebook-scoped Abstraction straight through `Engine`,
/// and is fenced anyway: the example flavor has no writer of `DigestV1`, so
/// only the declaration can put a fence in that path.
///
/// The first digest commits before the erase parks and must be SWEPT; the
/// second arrives while the erase holds the fence and must WAIT and then be
/// REFUSED.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_host_write_of_a_scoped_payload_through_the_engine_waits_and_is_refused() {
    let fx = Fixture::new("proxima_scope_fence").await;
    let owner = owner();
    let notebook_id = fx.notebook(owner).await;
    let authz = authz(owner);

    // A note for the digests to derive from: an Abstraction must pin at
    // least one Fact.
    let pinned = remember_note(&fx.engine, &authz, owner, &note(notebook_id, "pinned"))
        .await
        .expect("the pinned note");
    // The late digest pins a note of ANOTHER notebook. The erase pages out
    // this notebook's note before it takes the fence, so a late write
    // pinning that one would be refused for its missing pin before it ever
    // reached the fence.
    let elsewhere = fx.notebook(owner).await;
    let surviving = remember_note(&fx.engine, &authz, owner, &note(elsewhere, "survives"))
        .await
        .expect("the surviving note");

    let early = host_write_digest(&fx.engine, &authz, owner, notebook_id, "early", pinned)
        .await
        .expect("the early digest");
    assert!(
        fx.census(notebook_id).await > 0,
        "the host's digest landed before the erase"
    );

    let (barrier_pool, barrier) = fx.hold_notebook_row(owner, notebook_id).await;
    let mut erase = fx.spawn_erase(owner, notebook_id).await;
    if let Err(why) = fx.wait_for(Probe::AnyLock, 1, &mut erase).await {
        panic!(
            "the erase never parked on the notebook row; the interleaving did not set up: {why}"
        );
    }

    let engine = fx.task_engine().await;
    let mut late = tokio::spawn(async move {
        host_write_digest(
            &engine,
            &authenticated_context(AuthzContext::single_owner(&owner, AuthPath::HostBearer)),
            owner,
            notebook_id,
            "late",
            surviving,
        )
        .await
        .map_err(|err| err.to_string())
    });
    if let Err(why) = fx.wait_for(Probe::Advisory, 1, &mut late).await {
        panic!("the host's write did not wait on the declared scope fence: {why}");
    }
    assert!(!late.is_finished(), "the host write ran past the fence");

    barrier.rollback().await.expect("release the barrier");
    barrier_pool.close().await;

    erase
        .await
        .expect("erase task")
        .expect("the erase completes once unparked");
    let refusal = late
        .await
        .expect("late task")
        .expect_err("a host write into an erased scope must be refused");
    assert_scope_refusal(&refusal, notebook_id);
    assert_eq!(
        fx.census(notebook_id).await,
        0,
        "the digest the host wrote first is inside the erase's footprint"
    );
    assert_eq!(
        fx.surviving_memory(vec![early.into_inner()]).await,
        0,
        "and so is the admission behind it"
    );
}

/// An operation that ends before it reaches the fence is reported at its
/// cause, not as a wait the database never showed.
///
/// A run whose spawned operations failed in setup used to read as "the
/// erase never parked" or "did not wait on the notebook fence", which is
/// what a broken fence also says. The wait now watches the task: a refusal
/// arrives with the refusal's text, and a panic with the panic's message.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_operation_that_ends_before_the_fence_is_reported_at_its_cause() {
    let fx = Fixture::new("proxima_scope_fence").await;
    let owner = owner();
    let unregistered = Uuid::now_v7();

    // Nothing holds this notebook's fence, so nothing can wait on it: the
    // write is refused for its unregistered scope and the task ends.
    let mut write = fx.spawn_note(owner, unregistered, "orphan").await;
    let why = fx
        .wait_for(Probe::Advisory, 1, &mut write)
        .await
        .expect_err("no backend waits on a fence nobody holds");
    assert!(
        why.starts_with("the operation ended before it waited"),
        "the task's end is named: {why}"
    );
    assert_scope_refusal(&why, unregistered);

    // A task that fails in its own setup carries the panic's message.
    let mut setup = tokio::spawn(async {
        panic!("runtime connection: pool timed out while waiting for an open connection")
    });
    let why = fx
        .wait_for(Probe::AnyLock, 1, &mut setup)
        .await
        .expect_err("a panicked task waits on nothing");
    assert!(
        why.contains("panicked: runtime connection: pool timed out"),
        "the panic's message is the reported cause: {why}"
    );
}

/// One `DigestV1`, written the way a host writes it: the Engine's own
/// authored-derived verb, with no notebook check and no fence of its own.
async fn host_write_digest(
    engine: &Engine,
    authz: &AuthzContext,
    owner: OwnerId,
    notebook_id: Uuid,
    label: &str,
    pinned: MemoryId,
) -> Result<MemoryId, ProtocolError> {
    let payload = DigestV1 {
        notebook_id,
        summary: format!("host digest {label}"),
    };
    let text = payload.summary.clone();
    let outcome = engine
        .derive_memory(
            authz,
            DerivedMemory::abstraction(
                MemoryTarget::Series(SeriesHandle::new(Uuid::now_v7())),
                owner,
                text,
                payload,
                [pinned],
                DerivationIdentity::new(
                    OperatorId::new(Uuid::new_v5(
                        &Uuid::NAMESPACE_URL,
                        b"proxima/tests/scope-fence/host-digest",
                    )),
                    proxima_core::InputContractId::new(Uuid::new_v5(
                        &Uuid::NAMESPACE_URL,
                        label.as_bytes(),
                    )),
                ),
            )?,
        )
        .await?;
    Ok(outcome.memory_id)
}

/// One admitted note and the `proxima_core` rows behind it, written by hand
/// so the write can be held open across the erase's arrival. The stamp and
/// the row it promises land in one transaction: a memory row naming a
/// sidecar table it has no row in is refused at COMMIT.
async fn insert_note_row(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    owner: OwnerId,
    notebook_id: Uuid,
    t: Uuid,
) -> Result<(), sqlx::Error> {
    let owner_id = owner.into_inner();
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
    .bind(NoteV1::SCHEMA_ID)
    .bind(owner_id)
    .bind(t)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.memory (handle, t, kind, owner_id, schema_id, sidecar_tables, occurred_at, \"by\", via)
         VALUES ($1, $2, 'fact', $3, $4, $5, uuid_extract_timestamp($2), $3, NULL)",
    )
    .bind(handle)
    .bind(t)
    .bind(owner_id)
    .bind(NoteV1::SCHEMA_ID)
    .bind(vec!["proxima_example.note_v1".to_owned()])
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_example.note_v1 (t, notebook_id, key, body, written_at)
         VALUES ($1, $2, 'late', 'committed while the erase waited', now())",
    )
    .bind(t)
    .bind(notebook_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

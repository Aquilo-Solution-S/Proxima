//! `pending_migrations` answers what a migration run would apply, in the
//! runner's order, from a consistent snapshot, and writes nothing.
//!
//! The measure is the run itself: each shape plans, runs, and compares the
//! plan with the ledger rows the run then wrote (a flavor's cutover copies
//! excluded, in the order their transactions wrote them). Each refusal
//! compares the plan's error with the run's, as `Debug` text and as a variant.

#[path = "fixtures/split_core_db.rs"]
mod split_core_db;

use std::borrow::Cow;
use std::time::Duration;

use proxima::{
    LedgerConflict, MigrationError, NamedMigrator, PendingMigration, pending_migrations,
    run_core_and_flavor_migrations,
};
use proxima_pg_testkit::{
    DbGuard, SplitRoleDb, create_db, db_url, split_role_urls, unique_db_name,
};
use proxima_storage_pg::{PgPoolConfig, PgStorage, PgTuning, core_migrator};
use split_core_db::clone_split_core_db;
use sqlx::migrate::{MigrateError, Migration, MigrationType, Migrator};
use sqlx::{Connection, PgConnection, PgPool, SqlSafeStr};

const CORE: &str = "public._sqlx_migrations";
const FORGE: &str = "public._sqlx_migrations_forge";

const F1: i64 = 20_990_201_000_010;
const F2: i64 = 20_990_202_000_010;
const F3: i64 = 20_990_203_000_010;
const F4: i64 = 20_990_204_000_010;
const F5: i64 = 20_990_205_000_010;
const S1: i64 = 20_990_301_000_010;
const S2: i64 = 20_990_302_000_010;
const N1: i64 = 20_990_401_000_010;
/// Holds `pg_advisory_xact_lock(BLOCK_KEY)` until the test lets go of it.
const BLOCKED: i64 = 20_990_501_000_010;
const NO_TX: i64 = 20_990_601_000_010;

const BLOCK_KEY: i64 = 7001;
/// `MIGRATION_LOCK_KEY`: ASCII `proxmigr`.
const MIGRATION_KEY: i64 = i64::from_be_bytes(*b"proxmigr");

/// One version's SQL, the same in every lane that ships it.
fn sql(version: i64) -> &'static str {
    match version {
        F1 => "CREATE TABLE public.plan_f1 (id int)",
        F2 => "CREATE TABLE public.plan_f2 (id int)",
        F3 => "CREATE TABLE public.plan_f3 (id int)",
        F4 => "CREATE TABLE public.plan_f4 (id int)",
        F5 => "CREATE TABLE public.plan_f5 (id int)",
        S1 => "CREATE TABLE public.plan_s1 (id int)",
        S2 => "CREATE TABLE public.plan_s2 (id int)",
        N1 => "CREATE TABLE public.plan_n1 (id int)",
        BLOCKED => "SELECT pg_advisory_xact_lock(7001); CREATE TABLE public.plan_blocked (id int)",
        NO_TX => "CREATE TABLE public.plan_no_tx (id int)",
        other => panic!("no SQL for {other}"),
    }
}

fn migrator(versions: &[i64]) -> Migrator {
    let migrations = versions
        .iter()
        .map(|version| {
            Migration::new(
                *version,
                Cow::Owned(format!("plan {version}")),
                MigrationType::Simple,
                sqlx::AssertSqlSafe(sql(*version).to_owned()).into_sql_str(),
                *version == NO_TX,
            )
        })
        .collect();
    Migrator {
        migrations: Cow::Owned(migrations),
        ..Migrator::DEFAULT
    }
}

/// A flavor on its own ledger, `public._sqlx_migrations_forge`.
fn forge(versions: &[i64]) -> NamedMigrator {
    NamedMigrator::flavor("forge", migrator(versions))
}

/// A host migrator still recording on core's ledger.
fn shared(versions: &[i64]) -> NamedMigrator {
    NamedMigrator::new("shared", migrator(versions))
}

/// What drops the database when the test ends.
enum Guard {
    /// A database made empty, or migrated by hand.
    Created(DbGuard),
    /// A clone of the split-core template.
    Cloned(SplitRoleDb),
}

impl Guard {
    fn name(&self) -> &str {
        match self {
            Self::Created(guard) => guard.name(),
            Self::Cloned(clone) => clone.name(),
        }
    }
}

/// A disposable database with the split roles provisioned.
struct Db {
    guard: Guard,
    admin: PgPool,
    /// Runs migrations as the platform role.
    pg: PgStorage,
    platform_url: String,
    /// Pool of the platform role.
    platform: PgPool,
    /// Pool of the DML-only runtime role.
    runtime: PgPool,
}

impl Db {
    async fn open(guard: Guard) -> Self {
        let (runtime_url, platform_url) = split_role_urls(guard.name())
            .await
            .expect("split roles on the database");
        Self {
            admin: PgPool::connect(&db_url(guard.name()))
                .await
                .expect("admin pool"),
            pg: PgStorage::connect_for_migrations_with_config(
                &platform_url,
                PgPoolConfig::default(),
                PgTuning::default(),
            )
            .await
            .expect("platform storage"),
            platform: PgPool::connect(&platform_url).await.expect("platform pool"),
            runtime: PgPool::connect(&runtime_url).await.expect("runtime pool"),
            platform_url,
            guard,
        }
    }

    /// No migration has run; the ledgers role provisioning creates are empty.
    async fn empty() -> Self {
        let name = unique_db_name("proxima_plan");
        create_db(&name).await.expect("PG required");
        Self::open(Guard::Created(DbGuard::adopt(name))).await
    }

    /// Core is migrated; no flavor ledger exists.
    async fn core_current() -> Self {
        let clone = clone_split_core_db("proxima_plan")
            .await
            .expect("PG required");
        Self::open(Guard::Cloned(clone)).await
    }

    /// A live v0.0.8 database: the baseline applied, nothing after it.
    async fn core_at_the_v008_baseline() -> Self {
        let name = unique_db_name("proxima_plan");
        create_db(&name).await.expect("PG required");
        let guard = DbGuard::adopt(name);
        let admin = PgPool::connect(&db_url(guard.name()))
            .await
            .expect("admin pool");
        let core = core_migrator();
        let baseline = core
            .iter()
            .find(|migration| migration.version == 1)
            .expect("the embedded core set carries the v008 baseline");
        // SQL-POLICY: fixed-fragment — the migration's own embedded text, the
        // bytes `core_migrator().run()` would execute.
        sqlx::raw_sql(baseline.sql.clone())
            .execute(&admin)
            .await
            .expect("baseline");
        sqlx::query(
            "CREATE TABLE public._sqlx_migrations (
                 version bigint PRIMARY KEY,
                 description text NOT NULL,
                 installed_on timestamptz NOT NULL DEFAULT now(),
                 success boolean NOT NULL,
                 checksum bytea NOT NULL,
                 execution_time bigint NOT NULL
             )",
        )
        .execute(&admin)
        .await
        .expect("core ledger");
        sqlx::query(
            "INSERT INTO public._sqlx_migrations
                 (version, description, success, checksum, execution_time)
             VALUES ($1, $2, true, $3, 0)",
        )
        .bind(baseline.version)
        .bind(baseline.description.as_ref())
        .bind(baseline.checksum.as_ref())
        .execute(&admin)
        .await
        .expect("baseline row");
        admin.close().await;
        Self::open(Guard::Created(guard)).await
    }

    async fn run(&self, sources: Vec<NamedMigrator>) {
        run_core_and_flavor_migrations(&self.pg, sources)
            .await
            .expect("the migration run");
    }

    async fn plan(
        &self,
        sources: &[NamedMigrator],
    ) -> Result<Vec<PendingMigration>, MigrationError> {
        pending_migrations(&self.platform, sources).await
    }

    async fn ledger_exists(&self, ledger: &str) -> bool {
        sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
            .bind(ledger)
            .fetch_one(&self.admin)
            .await
            .expect("ledger lookup")
    }

    /// The rows of one ledger, none if it does not exist.
    async fn ledger_rows(&self, ledger: &'static str) -> Vec<Row> {
        if !self.ledger_exists(ledger).await {
            return Vec::new();
        }
        let found: Vec<(i64, Vec<u8>, bool, String, i64, String)> = if ledger == CORE {
            sqlx::query_as(
                "SELECT version, checksum, success, installed_on::text, xmin::text::bigint,
                        ctid::text
                   FROM public._sqlx_migrations ORDER BY version",
            )
            .fetch_all(&self.admin)
            .await
        } else {
            sqlx::query_as(
                "SELECT version, checksum, success, installed_on::text, xmin::text::bigint,
                        ctid::text
                   FROM public._sqlx_migrations_forge ORDER BY version",
            )
            .fetch_all(&self.admin)
            .await
        }
        .expect("ledger rows");
        found
            .into_iter()
            .map(
                |(version, checksum, success, installed_on, xid, ctid)| Row {
                    ledger,
                    version,
                    checksum,
                    success,
                    installed_on,
                    xid,
                    slot: heap_slot(&ctid),
                },
            )
            .collect()
    }

    /// Every ledger row, with the transaction id that wrote it, and every
    /// table in the database: what a write by the plan would change.
    async fn world(&self) -> (Vec<String>, Vec<Row>) {
        let tables = sqlx::query_scalar(
            "SELECT schemaname || '.' || tablename FROM pg_tables
              WHERE schemaname NOT IN ('pg_catalog', 'information_schema')
              ORDER BY 1",
        )
        .fetch_all(&self.admin)
        .await
        .expect("tables");
        let mut rows = self.ledger_rows(CORE).await;
        rows.extend(self.ledger_rows(FORGE).await);
        (tables, rows)
    }

    /// Plan, run, and compare: the plan equals what the run applied, in the
    /// order it applied it, and nothing is left afterwards. Returns the plan.
    async fn plan_then_run(
        &self,
        sources: impl Fn() -> Vec<NamedMigrator>,
    ) -> Vec<PendingMigration> {
        let (tables, before) = self.world().await;
        let plan = self.plan(&sources()).await.expect("the plan");
        assert_eq!(
            self.world().await,
            (tables, before.clone()),
            "the plan wrote nothing and created nothing"
        );
        self.run(sources()).await;
        let (_, after) = self.world().await;
        assert_eq!(
            planned(&plan),
            applied_between(&before, &after),
            "the plan is what the run applied, in the order it applied it"
        );
        assert_eq!(
            self.plan(&sources()).await.expect("the plan after the run"),
            Vec::new(),
            "nothing is left after the run"
        );
        plan
    }

    /// A refusal the plan and the run share: the same typed error.
    async fn same_refusal(&self, sources: impl Fn() -> Vec<NamedMigrator>) -> MigrationError {
        let (tables, before) = self.world().await;
        let refused = self.plan(&sources()).await.expect_err("the plan refuses");
        assert_eq!(
            self.world().await,
            (tables, before),
            "a refusing plan wrote nothing"
        );
        let ran = run_core_and_flavor_migrations(&self.pg, sources())
            .await
            .expect_err("the run refuses");
        assert_eq!(
            format!("{refused:?}"),
            format!("{ran:?}"),
            "the plan returns what the run returns"
        );
        refused
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    ledger: &'static str,
    version: i64,
    checksum: Vec<u8>,
    success: bool,
    installed_on: String,
    /// The transaction that wrote the row.
    xid: i64,
    /// Where in the table's heap, `(page, line)`: rows one transaction wrote
    /// sit in the order it wrote them.
    slot: (i64, i64),
}

/// `(page,line)` as `ctid::text` spells it.
fn heap_slot(ctid: &str) -> (i64, i64) {
    let (page, line) = ctid
        .trim_matches(|c| c == '(' || c == ')')
        .split_once(',')
        .expect("a ctid is (page,line)");
    (page.parse().expect("page"), line.parse().expect("line"))
}

type Triple = (String, i64, Vec<u8>);

fn planned(plan: &[PendingMigration]) -> Vec<Triple> {
    plan.iter()
        .map(|pending| {
            (
                pending.ledger.clone(),
                pending.version,
                pending.checksum.clone(),
            )
        })
        .collect()
}

fn coordinates(plan: &[PendingMigration]) -> Vec<(&str, i64)> {
    plan.iter()
        .map(|pending| (pending.ledger.as_str(), pending.version))
        .collect()
}

/// The migrations a run wrote between two readings of the ledgers, in write
/// order: by transaction, then by place in the heap. A row on a flavor's own ledger whose version core's ledger also
/// holds is the cutover's copy, not an applied migration.
fn applied_between(before: &[Row], after: &[Row]) -> Vec<Triple> {
    let mut fresh: Vec<&Row> = after
        .iter()
        .filter(|row| {
            !before
                .iter()
                .any(|old| old.ledger == row.ledger && old.version == row.version)
        })
        .filter(|row| {
            row.ledger == CORE
                || !after
                    .iter()
                    .any(|other| other.ledger == CORE && other.version == row.version)
        })
        .collect();
    fresh.sort_by_key(|row| (row.xid, row.slot));
    fresh
        .into_iter()
        .map(|row| (row.ledger.to_owned(), row.version, row.checksum.clone()))
        .collect()
}

/// Core's up migrations as the plan lists them for a database without any.
fn core_triples(after_version: i64) -> Vec<(String, i64, String, Vec<u8>)> {
    core_migrator()
        .iter()
        .filter(|migration| {
            !migration.migration_type.is_down_migration() && migration.version > after_version
        })
        .map(|migration| {
            (
                CORE.to_owned(),
                migration.version,
                migration.description.to_string(),
                migration.checksum.to_vec(),
            )
        })
        .collect()
}

fn full(plan: &[PendingMigration]) -> Vec<(String, i64, String, Vec<u8>)> {
    plan.iter()
        .map(|pending| {
            (
                pending.ledger.clone(),
                pending.version,
                pending.description.clone(),
                pending.checksum.clone(),
            )
        })
        .collect()
}

#[tokio::test]
async fn a_fresh_database_lists_everything_in_runner_order() {
    let db = Db::empty().await;
    assert!(
        db.world().await.1.is_empty(),
        "role provisioning leaves empty ledgers, and no migration recorded"
    );

    let reversed = db
        .plan(&[shared(&[S1, S2]), forge(&[F1, F2, F3])])
        .await
        .expect("the plan");
    let mut expected: Vec<(&str, i64)> = core_triples(0)
        .iter()
        .map(|(_, version, _, _)| (CORE, *version))
        .collect();
    expected.extend([
        (CORE, S1),
        (CORE, S2),
        (FORGE, F1),
        (FORGE, F2),
        (FORGE, F3),
    ]);
    assert_eq!(
        coordinates(&reversed),
        expected,
        "core first, then each source in the order given"
    );

    let plan = db
        .plan_then_run(|| vec![forge(&[F1, F2, F3]), shared(&[S1, S2])])
        .await;
    let core_len = core_triples(0).len();
    assert_eq!(
        full(&plan[..core_len]),
        core_triples(0),
        "core's own migrations: version, description and checksum"
    );
    assert_eq!(
        coordinates(&plan[core_len..]),
        [
            (FORGE, F1),
            (FORGE, F2),
            (FORGE, F3),
            (CORE, S1),
            (CORE, S2)
        ]
    );
    assert_eq!(plan[core_len].description, format!("plan {F1}"));
}

/// An absent ledger is an empty one: the plan neither needs the table nor
/// creates it.
#[tokio::test]
async fn a_database_without_any_ledger_lists_everything() {
    let db = Db::empty().await;
    sqlx::query("DROP TABLE public._sqlx_migrations, public._sqlx_migrations_proxima_code")
        .execute(&db.admin)
        .await
        .expect("drop the ledgers role provisioning created");
    assert!(
        !db.ledger_exists(CORE).await && !db.ledger_exists(FORGE).await,
        "no ledger"
    );

    let plan = db
        .plan_then_run(|| vec![forge(&[F1, F2]), shared(&[S1])])
        .await;
    let core_len = core_triples(0).len();
    assert_eq!(full(&plan[..core_len]), core_triples(0));
    assert_eq!(
        coordinates(&plan[core_len..]),
        [(FORGE, F1), (FORGE, F2), (CORE, S1)]
    );
}

#[tokio::test]
async fn core_behind_lists_the_core_migrations_after_the_ones_recorded() {
    let db = Db::core_at_the_v008_baseline().await;
    let plan = db.plan_then_run(|| vec![forge(&[F1]), shared(&[S1])]).await;

    let core_len = core_triples(1).len();
    assert!(core_len > 0, "the embedded lane is ahead of the baseline");
    assert_eq!(full(&plan[..core_len]), core_triples(1));
    assert_eq!(coordinates(&plan[core_len..]), [(FORGE, F1), (CORE, S1)]);
}

#[tokio::test]
async fn a_flavor_behind_lists_the_rest_and_a_current_database_lists_nothing() {
    let db = Db::core_current().await;
    db.run(vec![forge(&[F1]), shared(&[S1])]).await;

    let plan = db
        .plan_then_run(|| vec![forge(&[F1, F2, F3]), shared(&[S1, S2])])
        .await;
    assert_eq!(
        coordinates(&plan),
        [(FORGE, F2), (FORGE, F3), (CORE, S2)],
        "the own ledger and the shared ledger each list their rest"
    );

    let plan = db
        .plan_then_run(|| vec![forge(&[F1, F2, F3]), shared(&[S1, S2])])
        .await;
    assert_eq!(coordinates(&plan), [], "a current database lists nothing");
}

#[tokio::test]
async fn a_flavor_whose_rows_sit_on_the_shared_ledger_has_nothing_pending() {
    let db = Db::core_current().await;
    // `NamedMigrator::new` records on core's ledger: the shape of a database
    // from before the flavor had a ledger of its own.
    db.run(vec![NamedMigrator::new("forge", migrator(&[F1, F2]))])
        .await;
    assert!(
        !db.ledger_exists(FORGE).await,
        "the flavor never had its own ledger"
    );
    let before = db.world().await;

    let plan = db.plan(&[forge(&[F1, F2])]).await.expect("the plan");
    assert_eq!(
        coordinates(&plan),
        [],
        "the cutover would copy both rows: nothing is pending"
    );
    assert_eq!(db.world().await, before, "the plan changed nothing");
    assert!(
        !db.ledger_exists(FORGE).await,
        "the plan did not create the own ledger"
    );

    let plan = db.plan_then_run(|| vec![forge(&[F1, F2, F3])]).await;
    assert_eq!(
        coordinates(&plan),
        [(FORGE, F3)],
        "the copied rows count; the run's own ledger gets F3"
    );
    assert!(db.ledger_exists(FORGE).await, "the run created it");
}

#[tokio::test]
async fn a_lane_that_does_not_continue_its_ledger_is_refused_as_the_run_refuses_it() {
    let db = Db::core_current().await;
    db.run(vec![forge(&[F1, F2, F4])]).await;

    let replaced = db.same_refusal(|| vec![forge(&[F5])]).await;
    assert!(
        matches!(
            replaced,
            MigrationError::Ledger {
                source: "forge",
                conflict: LedgerConflict::Replaced { root: F5, .. },
                ..
            }
        ),
        "{replaced:?}"
    );

    let diverged = db.same_refusal(|| vec![forge(&[F1, F2, F3])]).await;
    assert!(
        matches!(
            diverged,
            MigrationError::Ledger {
                source: "forge",
                ref ledger,
                conflict: LedgerConflict::Diverged { .. },
            } if ledger == FORGE
        ),
        "{diverged:?}"
    );
}

#[tokio::test]
async fn a_dirty_ledger_row_is_refused_as_the_run_refuses_it() {
    let db = Db::core_current().await;
    db.run(vec![forge(&[F1, F2]), shared(&[S1])]).await;

    sqlx::query(
        "INSERT INTO public._sqlx_migrations_forge
             (version, description, success, checksum, execution_time)
         VALUES ($1, 'half applied', false, '\\x00', 0)",
    )
    .bind(F3)
    .execute(&db.admin)
    .await
    .expect("dirty row");
    let own = db.same_refusal(|| vec![forge(&[F1, F2, F3, F4])]).await;
    assert!(
        matches!(
            own,
            MigrationError::Flavor {
                source: "forge",
                err: MigrateError::Dirty(F3)
            }
        ),
        "{own:?}"
    );

    sqlx::query(
        "INSERT INTO public._sqlx_migrations
             (version, description, success, checksum, execution_time)
         VALUES ($1, 'half applied', false, '\\x00', 0)",
    )
    .bind(S2)
    .execute(&db.admin)
    .await
    .expect("dirty row");
    let core = db.same_refusal(|| vec![forge(&[F1, F2, F3, F4])]).await;
    assert!(
        matches!(core, MigrationError::Core(MigrateError::Dirty(S2))),
        "core's run reads the whole shared ledger first: {core:?}"
    );
}

#[tokio::test]
async fn a_changed_checksum_is_refused_as_the_run_refuses_it() {
    let db = Db::core_current().await;
    db.run(vec![forge(&[F1, F2]), shared(&[S1, S2])]).await;

    sqlx::query("UPDATE public._sqlx_migrations_forge SET checksum = '\\x00' WHERE version = $1")
        .bind(F1)
        .execute(&db.admin)
        .await
        .expect("amend");
    let own = db.same_refusal(|| vec![forge(&[F1, F2, F3])]).await;
    assert!(
        matches!(
            own,
            MigrationError::Flavor {
                source: "forge",
                err: MigrateError::VersionMismatch(F1)
            }
        ),
        "{own:?}"
    );

    sqlx::query("UPDATE public._sqlx_migrations SET checksum = '\\x00' WHERE version = $1")
        .bind(S1)
        .execute(&db.admin)
        .await
        .expect("amend");
    let on_shared = db.same_refusal(|| vec![shared(&[S1, S2])]).await;
    assert!(
        matches!(
            on_shared,
            MigrationError::Flavor {
                source: "shared",
                err: MigrateError::VersionMismatch(S1)
            }
        ),
        "{on_shared:?}"
    );
}

#[tokio::test]
async fn a_core_ledger_that_does_not_reconcile_is_refused_as_the_run_refuses_it() {
    let db = Db::core_current().await;

    sqlx::query(
        "INSERT INTO public._sqlx_migrations
             (version, description, success, checksum, execution_time)
         VALUES (9000, 'retired draft', true, '\\x01', 0)",
    )
    .execute(&db.admin)
    .await
    .expect("unknown core version");
    let unknown = db.same_refusal(Vec::new).await;
    assert!(
        matches!(unknown, MigrationError::CorePreflight(_)),
        "{unknown:?}"
    );
    sqlx::query("DELETE FROM public._sqlx_migrations WHERE version = 9000")
        .execute(&db.admin)
        .await
        .expect("restore");

    let original: Vec<u8> =
        sqlx::query_scalar("SELECT checksum FROM public._sqlx_migrations WHERE version = 5")
            .fetch_one(&db.admin)
            .await
            .expect("checksum");
    sqlx::query("UPDATE public._sqlx_migrations SET checksum = '\\x00' WHERE version = 5")
        .execute(&db.admin)
        .await
        .expect("amend");
    let amended = db.same_refusal(Vec::new).await;
    assert!(
        matches!(amended, MigrationError::CorePreflight(_)),
        "{amended:?}"
    );
    sqlx::query("UPDATE public._sqlx_migrations SET checksum = $1 WHERE version = 5")
        .bind(original)
        .execute(&db.admin)
        .await
        .expect("restore");
    assert_eq!(
        db.plan(&[]).await.expect("the restored ledger plans"),
        Vec::new()
    );
}

#[tokio::test]
async fn a_source_the_run_refuses_is_refused_by_the_plan() {
    let db = Db::core_current().await;
    let refused = db.same_refusal(|| vec![forge(&[F1, NO_TX])]).await;
    assert!(
        matches!(
            refused,
            MigrationError::Flavor {
                source: "forge",
                err: MigrateError::Execute(sqlx::Error::Protocol(_))
            }
        ),
        "{refused:?}"
    );
}

/// `SQLx` walks a migrator in the order it holds, not by version, and the
/// plan lists and refuses in that order: the first changed checksum is the
/// one the run would name.
#[tokio::test]
async fn a_source_out_of_order_is_planned_and_refused_in_the_order_the_run_walks_it() {
    let db = Db::core_current().await;
    let out_of_order = || vec![forge(&[F3, F1, F2])];

    let plan = db.plan_then_run(out_of_order).await;
    assert_eq!(
        coordinates(&plan),
        [(FORGE, F3), (FORGE, F1), (FORGE, F2)],
        "the run applied them in the order the migrator holds, and the plan said so"
    );

    sqlx::query(
        "UPDATE public._sqlx_migrations_forge SET checksum = '\\x00' WHERE version = ANY($1)",
    )
    .bind(vec![F1, F3])
    .execute(&db.admin)
    .await
    .expect("amend");
    let refused = db.same_refusal(out_of_order).await;
    assert!(
        matches!(
            refused,
            MigrationError::Flavor {
                source: "forge",
                err: MigrateError::VersionMismatch(F3)
            }
        ),
        "F3 comes first in the order the run walks, F1 first by version: {refused:?}"
    );
}

/// Sessions of this database waiting for advisory lock `key`: the tests run
/// side by side on one cluster, and advisory locks are per database.
async fn locked_waiters(conn: &mut PgConnection, key: i64) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM pg_locks
          WHERE locktype = 'advisory' AND NOT granted
            AND database = (SELECT oid FROM pg_database WHERE datname = current_database())
            AND ((classid::bigint << 32) | objid::bigint) = $1",
    )
    .bind(key)
    .fetch_one(conn)
    .await
    .expect("lock waiters")
}

async fn wait_for_waiters(conn: &mut PgConnection, key: i64) {
    for _ in 0..400 {
        if locked_waiters(conn, key).await > 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("nothing waits for advisory lock {key}");
}

/// A run in the middle of a source has taken the migration lock and not yet
/// committed. A plan that read before it got the lock would list as pending
/// what the run commits a moment later; one that waits reads after.
#[tokio::test]
async fn a_plan_waits_for_a_run_mid_source_and_reports_the_state_after_it() {
    let db = Db::core_current().await;
    db.run(vec![forge(&[F1])]).await;
    let sources = || vec![forge(&[F1, BLOCKED])];

    // The run's second migration blocks on this session's lock, with the
    // migration lock held and its transaction open.
    let mut blocker = PgConnection::connect(&db_url(db.guard.name()))
        .await
        .expect("blocker");
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(BLOCK_KEY)
        .execute(&mut blocker)
        .await
        .expect("block");
    let platform_url = db.platform_url.clone();
    let run = tokio::spawn(async move {
        let pg = PgStorage::connect_for_migrations_with_config(
            &platform_url,
            PgPoolConfig::default(),
            PgTuning::default(),
        )
        .await
        .expect("storage");
        run_core_and_flavor_migrations(&pg, sources()).await
    });
    wait_for_waiters(&mut blocker, BLOCK_KEY).await;

    let plan_pool = db.platform.clone();
    let mut plan = tokio::spawn(async move { pending_migrations(&plan_pool, &sources()).await });
    wait_for_waiters(&mut blocker, MIGRATION_KEY).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(300), &mut plan)
            .await
            .is_err(),
        "the plan waits for the lock the run holds"
    );

    sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(BLOCK_KEY)
        .execute(&mut blocker)
        .await
        .expect("unblock");
    run.await.expect("run task").expect("the run commits");
    let plan = plan.await.expect("plan task").expect("the plan");
    assert_eq!(
        coordinates(&plan),
        [],
        "the plan reads after the commit: {BLOCKED} is applied"
    );
}

/// The wait is the run's: `lock_timeout` of 5 s, then the plan fails as the
/// run's lock statement does, and the connection that waited, which carries
/// the session setting, does not go back to the pool.
#[tokio::test]
async fn a_plan_gives_up_on_the_lock_when_a_run_would() {
    let db = Db::core_current().await;
    let mut holder = PgConnection::connect(&db_url(db.guard.name()))
        .await
        .expect("holder");
    let default_timeout: String = sqlx::query_scalar("SHOW lock_timeout")
        .fetch_one(&mut holder)
        .await
        .expect("default lock_timeout");
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(MIGRATION_KEY)
        .execute(&mut holder)
        .await
        .expect("hold the migration lock");

    let started = std::time::Instant::now();
    let refused = db
        .plan(&[forge(&[F1])])
        .await
        .expect_err("the lock is not granted");
    assert!(
        started.elapsed() >= Duration::from_secs(4),
        "the plan waited for the lock: {:?}",
        started.elapsed()
    );
    let MigrationError::Core(MigrateError::Execute(sqlx::Error::Database(refusal))) = &refused
    else {
        panic!("expected the run's lock failure, got {refused:?}");
    };
    assert_eq!(
        refusal.code().as_deref(),
        Some("55P03"),
        "lock_not_available"
    );

    sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(MIGRATION_KEY)
        .execute(&mut holder)
        .await
        .expect("release");
    assert_eq!(
        coordinates(
            &db.plan(&[forge(&[F1])])
                .await
                .expect("plan once the lock is free")
        ),
        [(FORGE, F1)]
    );
    // A dropped connection is returned or closed by a task of its own: let
    // the pool settle, so a returned connection would be the one asked.
    for _ in 0..80 {
        if db.platform.size() as usize == db.platform.num_idle() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let after: String = sqlx::query_scalar("SHOW lock_timeout")
        .fetch_one(&db.platform)
        .await
        .expect("pool lock_timeout");
    assert_eq!(
        after, default_timeout,
        "no connection the plan configured is back in the pool"
    );
}

async fn granted_holders(conn: &mut PgConnection, key: i64) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM pg_locks
          WHERE locktype = 'advisory' AND granted
            AND database = (SELECT oid FROM pg_database WHERE datname = current_database())
            AND ((classid::bigint << 32) | objid::bigint) = $1",
    )
    .bind(key)
    .fetch_one(conn)
    .await
    .expect("lock holders")
}

/// A caller that drops the plan while it waits for the lock must not leave
/// the lock on a pooled connection: the session that was waiting is closed
/// with the future, and a plan or run afterwards gets the lock.
#[tokio::test]
async fn a_plan_dropped_while_it_waits_for_the_lock_does_not_strand_the_lock() {
    let db = Db::core_current().await;
    let mut holder = PgConnection::connect(&db_url(db.guard.name()))
        .await
        .expect("holder");
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(MIGRATION_KEY)
        .execute(&mut holder)
        .await
        .expect("hold the migration lock");

    let pool = db.platform.clone();
    let plan = tokio::spawn(async move { pending_migrations(&pool, &[forge(&[F1])]).await });
    wait_for_waiters(&mut holder, MIGRATION_KEY).await;
    plan.abort();
    assert!(
        plan.await
            .expect_err("the plan was cancelled")
            .is_cancelled()
    );

    sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(MIGRATION_KEY)
        .execute(&mut holder)
        .await
        .expect("release");

    // A pool of its own, so the plan cannot be handed the connection that was
    // cancelled: a pooled one would hold the lock (a session can take it
    // twice, a second session waits out the 5 s `lock_timeout`).
    let other = PgPool::connect(&db.platform_url).await.expect("other pool");
    let started = std::time::Instant::now();
    assert_eq!(
        coordinates(
            &pending_migrations(&other, &[forge(&[F1])])
                .await
                .expect("a plan after the dropped one gets the lock")
        ),
        [(FORGE, F1)]
    );
    db.run(vec![forge(&[F1])]).await;
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "neither waited out the lock_timeout: {:?}",
        started.elapsed()
    );
    for _ in 0..80 {
        if granted_holders(&mut holder, MIGRATION_KEY).await == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("a session still holds the migration lock");
}

/// The runner disables `statement_timeout` and pins `search_path` on the
/// connection it migrates on. A caller that drops the run mid-source must
/// not find that connection back in the pool.
#[tokio::test]
async fn a_run_dropped_mid_source_does_not_return_its_connection_to_the_pool() {
    let db = Db::core_current().await;
    db.run(vec![forge(&[F1])]).await;

    let mut blocker = PgConnection::connect(&db_url(db.guard.name()))
        .await
        .expect("blocker");
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(BLOCK_KEY)
        .execute(&mut blocker)
        .await
        .expect("block");
    let pg = PgStorage::connect_for_migrations_with_config(
        &db.platform_url,
        PgPoolConfig::default(),
        PgTuning::default(),
    )
    .await
    .expect("storage");
    let pool = pg.pool_for_tests().clone();
    let default_timeout: String = sqlx::query_scalar("SHOW statement_timeout")
        .fetch_one(&pool)
        .await
        .expect("pool statement_timeout");

    // The second migration blocks on this session's lock, mid-source.
    let dropped = tokio::time::timeout(
        Duration::from_secs(2),
        run_core_and_flavor_migrations(&pg, vec![forge(&[F1, BLOCKED])]),
    )
    .await;
    assert!(
        dropped.is_err(),
        "the run is still blocked when it is dropped"
    );

    sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(BLOCK_KEY)
        .execute(&mut blocker)
        .await
        .expect("unblock");
    // A dropped connection is returned or closed by a task of its own: let
    // the pool settle, so a returned connection would be among those asked.
    for _ in 0..80 {
        if pool.size() as usize == pool.num_idle() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let mut held = Vec::new();
    for _ in 0..pool.size().max(1) {
        let mut conn = pool.acquire().await.expect("pool connection");
        let timeout: String = sqlx::query_scalar("SHOW statement_timeout")
            .fetch_one(&mut *conn)
            .await
            .expect("statement_timeout");
        assert_eq!(
            timeout, default_timeout,
            "no connection the run configured is back in the pool"
        );
        held.push(conn);
    }
}

#[tokio::test]
async fn a_plan_under_the_runtime_role_succeeds_and_writes_nothing() {
    let db = Db::core_current().await;
    db.run(vec![forge(&[F1, F2]), shared(&[S1])]).await;
    let before = db.world().await;
    let sources = || {
        vec![
            forge(&[F1, F2, F3, F4]),
            shared(&[S1, S2]),
            NamedMigrator::flavor("newcomer", migrator(&[N1])),
        ]
    };

    let plan = pending_migrations(&db.runtime, &sources())
        .await
        .expect("the runtime role can plan");
    assert_eq!(
        coordinates(&plan),
        [
            (FORGE, F3),
            (FORGE, F4),
            (CORE, S2),
            ("public._sqlx_migrations_newcomer", N1),
        ],
        "a flavor whose ledger does not exist lists all of its lane"
    );
    assert_eq!(db.world().await, before, "the plan wrote nothing");
    assert!(
        !db.ledger_exists("public._sqlx_migrations_newcomer").await,
        "and created no ledger"
    );
    assert_eq!(
        plan,
        db.plan(&sources()).await.expect("the platform role plans"),
        "the roles see the same plan"
    );
}

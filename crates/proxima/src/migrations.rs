//! Shared migration facade for embedded Proxima hosts.
//!
//! Core records into `SQLx`'s default `public._sqlx_migrations`; each
//! flavor records into its own tracking table
//! (`public._sqlx_migrations_<flavor>` — in `public`, because destructive
//! flavor baselines drop the flavor schema and the ledger must survive
//! them). Before each flavor migration run, matching rows from a pre-split
//! database's shared ledger are copied into its flavor ledger and retained
//! in the shared table. The facade pins the migration `search_path` to
//! `public`: core runs first, flavors run in composition order, and
//! duplicate versions fail before the database is touched.
//!
//! Every migrator keeps `SQLx`'s `ignore_missing`: a ledger row the binary
//! does not ship is normal when two releases' fleets share a database or a
//! lane shed a file. What is not normal is a binary whose lane does not
//! continue the one its ledger records; [`LedgerConflict`] names those
//! shapes, and each flavor ledger is checked against them before it runs.
//!
//! [`pending_migrations`] answers what a run would apply without applying it.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use proxima_core::StorageError;
use proxima_storage_pg::{
    PgStorage, core_migrator, ensure_core_ledger_compatible,
    ensure_core_ledger_compatible_on_connection, ensure_core_schema_current,
};
use sqlx::migrate::{MigrateError, Migration, Migrator};
use sqlx::pool::PoolConnection;
use sqlx::{AssertSqlSafe, Connection, PgConnection, PgPool, Postgres, Transaction};

const CORE_SOURCE: &str = "proxima-core";

/// Core's tracking table, which a flavor still recording on it shares.
const CORE_LEDGER: &str = "public._sqlx_migrations";

/// One named `SQLx` migration source in a composite Proxima binary.
#[derive(Debug)]
pub struct NamedMigrator {
    source: &'static str,
    migrator: Migrator,
}

impl NamedMigrator {
    /// Build a named migrator. Use the flavor id or host app id as
    /// `source`, e.g. `proxima-code`.
    ///
    /// The migrator keeps whatever tracking table it declares; a flavor
    /// that declares none records into core's `public._sqlx_migrations`.
    /// Prefer [`Self::flavor`].
    #[must_use]
    pub fn new(source: &'static str, migrator: Migrator) -> Self {
        Self { source, migrator }
    }

    /// A flavor's migrator on its own ledger: [`flavor_ledger_table`]`(id)`,
    /// unless the migrator already declares a table of its own, which it
    /// keeps (a ledger is never renamed under a live database).
    ///
    /// A flavor ledger lives in `public` because a destructive flavor
    /// baseline drops the flavor schema and the ledger must survive it. A
    /// database whose rows for this flavor sit in core's
    /// `public._sqlx_migrations` gets them copied over on the next migration
    /// run, before the flavor's migrator first reads its own table, so
    /// switching a shared-ledger flavor to this constructor re-runs nothing.
    ///
    /// # Panics
    ///
    /// When `id` is not a flavor id (see [`flavor_ledger_table`]); ids are
    /// compile-time constants, so this is a programming error.
    #[must_use]
    pub fn flavor(id: &'static str, mut migrator: Migrator) -> Self {
        let derived = flavor_ledger_table(id);
        if is_core_ledger(&migrator.table_name) {
            migrator.dangerous_set_table_name(derived);
        }
        migrator.set_ignore_missing(true);
        Self {
            source: id,
            migrator,
        }
    }

    /// Source id used in reports and errors.
    #[must_use]
    pub fn source(&self) -> &'static str {
        self.source
    }

    /// Borrow the underlying `SQLx` migrator.
    #[must_use]
    pub fn migrator(&self) -> &Migrator {
        &self.migrator
    }
}

/// The tracking table [`NamedMigrator::flavor`] gives flavor `id`:
/// `public._sqlx_migrations_<id>`, `-` spelled `_`
/// (`proxima-code` → `public._sqlx_migrations_proxima_code`).
///
/// # Panics
///
/// When `id` is empty, longer than 40 bytes, or not lowercase ASCII
/// letters, digits, `-` and `_` starting with a letter. The derived name is
/// interpolated into DDL by `SQLx` and by the ledger cutover, so only a name
/// that needs no quoting is accepted, and the bound keeps it within
/// the 63-byte `PostgreSQL` identifier limit.
#[must_use]
pub fn flavor_ledger_table(id: &str) -> String {
    assert!(
        is_flavor_ledger_id(id),
        "flavor id {id:?} must be 1-40 bytes of [a-z0-9_-] starting with a letter"
    );
    format!("public._sqlx_migrations_{}", id.replace('-', "_"))
}

/// Whether [`flavor_ledger_table`] accepts `id`. `const`, so
/// `flavor_bundle!` checks its `name` at compile time.
#[must_use]
pub const fn is_flavor_ledger_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    if bytes.is_empty() || bytes.len() > 40 || !bytes[0].is_ascii_lowercase() {
        return false;
    }
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if !(byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_') {
            return false;
        }
        index += 1;
    }
    true
}

fn is_core_ledger(table_name: &str) -> bool {
    table_name == "_sqlx_migrations" || table_name == CORE_LEDGER
}

/// Successful migration run metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationRunReport {
    pub sources: Vec<&'static str>,
}

/// Errors raised by the framework migration facade.
#[derive(Debug, thiserror::Error)]
pub enum MigrationError {
    #[error(
        "duplicate migration version {version}: {first_source} ({first_description}) and {second_source} ({second_description})"
    )]
    DuplicateVersion {
        version: i64,
        first_source: &'static str,
        first_description: String,
        second_source: &'static str,
        second_description: String,
    },
    #[error("failed to acquire migration connection: {0}")]
    Connection(#[source] sqlx::Error),
    #[error("failed to pin migration search_path to public: {0}")]
    PinSearchPath(#[source] sqlx::Error),
    #[error("failed to reset migration search_path after migrations: {0}")]
    ResetSearchPath(#[source] sqlx::Error),
    #[error("core migration preflight failed: {0}")]
    CorePreflight(#[source] StorageError),
    #[error("core migrations failed: {0}")]
    Core(#[source] MigrateError),
    #[error("flavor migrations failed for {source}: {err}")]
    Flavor {
        source: &'static str,
        #[source]
        err: MigrateError,
    },
    #[error(
        "two flavors record on one ledger {table}: {first_source} and {second_source}; give each its own (flavor ids that differ only in `-`/`_` derive the same name)"
    )]
    DuplicateLedger {
        table: String,
        first_source: &'static str,
        second_source: &'static str,
    },
    #[error("migration ledger preparation failed for {source}: {err}")]
    FlavorLedgerCutover {
        source: &'static str,
        #[source]
        err: sqlx::Error,
    },
    #[error("failed to read migration ledger {ledger} for {source}: {err}")]
    LedgerRead {
        source: &'static str,
        ledger: String,
        #[source]
        err: MigrateError,
    },
    #[error("{source} refuses migration ledger {ledger}: {conflict}")]
    Ledger {
        source: &'static str,
        ledger: String,
        #[source]
        conflict: LedgerConflict,
    },
}

/// Why a flavor's lane cannot run, or serve, against its ledger.
///
/// Checked for every source with a ledger of its own; core's shared
/// `public._sqlx_migrations` mixes lanes, so its rows cannot be attributed.
/// A lane squashed in part into a newer version still passes: its kept
/// first migration looks like an upgrade. Only a publish-time check that
/// each release's lane extends the previous one's catches that.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LedgerConflict {
    /// The lane's first migration was never applied, yet the ledger records
    /// versions this binary does not ship: the database ran a different
    /// lane (a replaced baseline, in either direction), and applying this
    /// one would re-create what that one built.
    #[error(
        "its first migration {root} was never applied here, but the ledger records {unknown:?}, which this binary does not ship: the database ran a different lane (a replaced baseline). Retire the release that does not match, or reset the lane's schemas and ledger before this one first boots (docs/how-to/migrations.md, Ledger lineage)"
    )]
    Replaced { root: i64, unknown: Vec<i64> },
    /// A pending migration is older than a recorded one this binary does
    /// not ship: a later release squashed or renumbered the lane this
    /// binary still carries.
    #[error(
        "it would apply {pending:?} after {unknown:?}, which a release this binary does not ship already applied: a later release squashed or renumbered this lane. Retire this release rather than boot it against this database (docs/how-to/migrations.md, Ledger lineage)"
    )]
    Diverged {
        pending: Vec<i64>,
        unknown: Vec<i64>,
    },
    /// `skip_migrations` boot found migrations that were never applied; it
    /// issues no DDL, so the lane would serve against an older schema.
    #[error(
        "migrations {pending:?} are not applied, and a boot that skips migrations issues no DDL: run the migration step first (docs/15-deployment.md)"
    )]
    Unapplied { pending: Vec<i64> },
}

/// Run core migrations followed by the provided flavor/host migrators.
///
/// # Errors
///
/// Returns `MigrationError::DuplicateVersion` before any database write
/// if two sources claim the same migration version. Returns `Connection`
/// or `PinSearchPath` if the pinned migration connection cannot be prepared.
/// Returns `Ledger`, before that source applies anything, if a flavor's
/// ledger records a lane its migrator does not continue ([`LedgerConflict`]).
/// Returns `Core` or `Flavor` if `SQLx` fails while applying that source.
pub async fn run_core_and_flavor_migrations(
    pg: &PgStorage,
    flavors: impl IntoIterator<Item = NamedMigrator>,
) -> Result<MigrationRunReport, MigrationError> {
    run_lineup(pg, &lineup(flavors)).await
}

/// [`run_core_and_flavor_migrations`] over a [`lineup`] the caller keeps: the
/// same refusals before any query, the same run. A test template builds from
/// the migrators it also fingerprints, without consuming them.
pub(crate) async fn run_lineup(
    pg: &PgStorage,
    sources: &[NamedMigrator],
) -> Result<MigrationRunReport, MigrationError> {
    check_lineup(sources)?;
    let report = MigrationRunReport::from_sources(sources);
    let pool = pg.clone_pool_for_backend();
    ensure_core_ledger_compatible(&pool)
        .await
        .map_err(MigrationError::CorePreflight)?;
    let mut conn = acquire_session_connection(&pool).await?;

    pin_migration_search_path(&mut conn)
        .await
        .map_err(MigrationError::PinSearchPath)?;

    let migration_result = run_sources_on_connection(&mut conn, sources).await;
    let reset_result = reset_migration_search_path(&mut conn).await;

    match (migration_result, reset_result) {
        (Ok(()), Ok(())) => Ok(report),
        (Err(err), Ok(())) => Err(err),
        (Ok(()), Err(err)) => Err(MigrationError::ResetSearchPath(err)),
        (Err(err), Err(reset_err)) => {
            tracing::warn!(
                error = %reset_err,
                "failed to reset migration search_path after migration error"
            );
            Err(err)
        }
    }
}

/// A connection that is about to carry session state: the runner's disabled
/// `statement_timeout`, a plan's `lock_timeout` and session lock.
///
/// It is marked close-on-drop here, before the caller can await on it, so no
/// way out returns it to the pool: not an error, and not the caller dropping
/// the future while it waits for the migration lock. A pooled connection
/// would be granted that lock later and hold it, or carry the overrides into
/// request serving. Closed, its session ends and takes both with it.
async fn acquire_session_connection(
    pool: &PgPool,
) -> Result<PoolConnection<Postgres>, MigrationError> {
    let mut conn = pool.acquire().await.map_err(MigrationError::Connection)?;
    conn.close_on_drop();
    Ok(conn)
}

/// Run the pre-boot compatibility preflight **without applying any
/// migration DDL**.
///
/// For `GitOps` / split-role deploys (see docs/15): an init container or
/// `tools/dev-migrate` applies migrations under a DDL-capable role, and the
/// long-running app then boots under a DML-only role that cannot issue DDL.
/// This rejects a stale pre-v0.0.4 database and rejects duplicate
/// migration versions across composed sources, but never runs
/// `run_direct` / touches schema — so it succeeds against an already-migrated
/// database held by a narrow role.
///
/// # Errors
///
/// Returns `MigrationError::DuplicateVersion` if two sources claim the same
/// version, `MigrationError::CorePreflight` if the database still carries
/// pre-v0.0.4 artifacts, `MigrationError::Ledger` if a flavor's ledger
/// conflicts with its lane or lacks one of its migrations, or
/// `MigrationError::Connection` if the preflight pool cannot be reached.
pub async fn preflight_without_migrations(
    pg: &PgStorage,
    flavors: impl IntoIterator<Item = NamedMigrator>,
) -> Result<MigrationRunReport, MigrationError> {
    let sources = prepare_sources(flavors)?;
    let report = MigrationRunReport::from_sources(&sources);
    let pool = pg.clone_pool_for_backend();
    ensure_core_ledger_compatible(&pool)
        .await
        .map_err(MigrationError::CorePreflight)?;
    ensure_core_schema_current(&pool)
        .await
        .map_err(MigrationError::CorePreflight)?;
    for source in sources
        .iter()
        .filter(|source| !is_core_ledger(&source.migrator.table_name))
    {
        let mut conn = pool.acquire().await.map_err(MigrationError::Connection)?;
        let recorded =
            ledger_versions(&read_ledger(&mut conn, source, &source.ledger(), true).await?);
        let lane = source.lane_versions();
        if let Some(conflict) =
            ledger_conflict(&lane, &recorded).or_else(|| unapplied(&lane, &recorded))
        {
            return Err(source.ledger_error(conflict));
        }
    }
    Ok(report)
}

/// A migration a run on the same database would apply.
///
/// Only [`pending_migrations`] makes one: a value a caller builds would claim
/// a plan nobody read from a ledger.
///
/// ```compile_fail,E0639
/// let _planned = proxima::host::PendingMigration {
///     ledger: "public._sqlx_migrations".to_owned(),
///     version: 1,
///     description: String::new(),
///     checksum: Vec::new(),
/// };
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct PendingMigration {
    /// The schema-qualified table the migration records on, spelled as
    /// [`flavor_ledger_table`] spells a flavor's (`public._sqlx_migrations`
    /// for core).
    pub ledger: String,
    pub version: i64,
    pub description: String,
    /// `SQLx`'s checksum of the migration file; the ledger row a run writes
    /// records it.
    pub checksum: Vec<u8>,
}

/// What a migration run on this database would apply right now, applying
/// nothing.
///
/// `sources` is what `flavors` is to [`run_core_and_flavor_migrations`]; core
/// is always first. The list holds core's pending migrations, then each
/// source's in the given order, inside a ledger in the order the run walks
/// the migrator (ascending by version for `sqlx::migrate!`). It follows the
/// runner's rules, from the runner's helpers:
///
/// - A flavor on its own ledger counts that ledger's rows, an absent ledger
///   as empty, and the rows of core's ledger that the run's ledger
///   preparation would copy for its versions. Nothing is copied or created.
/// - A flavor still recording on core's ledger is read there.
/// - Down migrations are not listed.
///
/// The plan runs under the migration lock a run takes per source (the
/// `proxmigr` advisory key) and reads every ledger in one
/// `REPEATABLE READ, READ ONLY` transaction: a run in the middle of a source
/// has committed or not committed all of it when the plan reads. It needs
/// `SELECT` on the ledgers, which a run leaves to the runtime role, issues no
/// DDL and writes nothing. It is a snapshot, not a reservation: a run checks
/// everything again under the lock. The connection that waits for the lock is
/// closed, not pooled, if the caller drops the future.
///
/// The plan says what would apply, not whether the previous release's binary
/// tolerates it. A coordinated cutover (docs/how-to/migrations.md, v0.0.15)
/// overrides any such reading of a migration file.
///
/// # Errors
///
/// Returns `MigrationError::DuplicateVersion` or `DuplicateLedger` before any
/// query; a table is matched by the name Postgres resolves, not by its
/// spelling (`_sqlx_migrations_host` is `public."_sqlx_migrations_host"`), and
/// a spelling outside Postgres' identifier rules is compared as written. Returns `Connection` if no connection can be acquired.
/// Returns `CorePreflight` if core's ledger does not reconcile with this
/// binary. Returns `Ledger` if a flavor's ledger records a lane its migrator
/// does not continue ([`LedgerConflict`]). Returns `Core` or `Flavor` with
/// `MigrateError::Dirty` for a `success = false` row and
/// `MigrateError::VersionMismatch` for a changed checksum, as a run would, and
/// with `MigrateError::Execute` if the lock is not granted within the
/// runner's `lock_timeout` or a source holds a `no_tx` migration. Returns
/// `LedgerRead` if a ledger cannot be read. The answer is the whole list or
/// an error, never a partial list.
pub async fn pending_migrations(
    pool: &PgPool,
    sources: &[NamedMigrator],
) -> Result<Vec<PendingMigration>, MigrationError> {
    let core = core_source();
    let lineup: Vec<&NamedMigrator> = std::iter::once(&core).chain(sources).collect();
    validate_sources(&lineup)?;
    let mut conn = acquire_session_connection(pool).await?;
    plan_under_lock(&mut conn, &lineup).await
}

/// The lock wait and the snapshot's statements are a run's own, so their
/// failures are a run's: core's, the first source a run locks for.
fn execute_error(err: sqlx::Error) -> MigrationError {
    MigrationError::Core(MigrateError::Execute(err))
}

async fn plan_under_lock(
    conn: &mut PgConnection,
    lineup: &[&NamedMigrator],
) -> Result<Vec<PendingMigration>, MigrationError> {
    sqlx::query(MIGRATION_LOCK_TIMEOUT_SQL)
        .execute(&mut *conn)
        .await
        .map_err(execute_error)?;
    let mut lock = SessionLock::acquire(conn).await.map_err(execute_error)?;
    let mut snapshot = lock.begin_snapshot().await.map_err(execute_error)?;
    let plan = plan_snapshot(&mut snapshot, lineup).await;
    // Nothing to commit: the snapshot only read.
    let ended = snapshot.rollback().await;
    let released = lock.release().await;
    let plan = plan?;
    ended.map_err(execute_error)?;
    released.map_err(execute_error)?;
    Ok(plan)
}

/// [`MIGRATION_LOCK_KEY`] held on one session. The runner takes the same key
/// per transaction (`pg_advisory_xact_lock`); the two exclude each other.
///
/// Held until [`Self::release`] or until the connection closes. The
/// snapshot can only begin on a held lock: a lock taken inside a `REPEATABLE
/// READ` transaction is granted after its snapshot is fixed.
struct SessionLock<'c> {
    conn: &'c mut PgConnection,
}

impl<'c> SessionLock<'c> {
    async fn acquire(conn: &'c mut PgConnection) -> Result<Self, sqlx::Error> {
        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(MIGRATION_LOCK_KEY)
            .execute(&mut *conn)
            .await?;
        Ok(Self { conn })
    }

    async fn begin_snapshot(&mut self) -> Result<Transaction<'_, Postgres>, sqlx::Error> {
        let mut snapshot = self.conn.begin().await?;
        // Before any query: the snapshot is fixed by the first one.
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
            .execute(&mut *snapshot)
            .await?;
        Ok(snapshot)
    }

    async fn release(self) -> Result<(), sqlx::Error> {
        sqlx::query("SELECT pg_advisory_unlock($1)")
            .bind(MIGRATION_LOCK_KEY)
            .execute(&mut *self.conn)
            .await?;
        Ok(())
    }
}

/// Walk the sources in the order a run does, so the first refusal is the
/// run's: core's preflight, then per source its `no_tx` refusal, its lineage
/// check, and the dirty and checksum refusals of `SQLx`'s run.
async fn plan_snapshot(
    snapshot: &mut PgConnection,
    lineup: &[&NamedMigrator],
) -> Result<Vec<PendingMigration>, MigrationError> {
    ensure_core_ledger_compatible_on_connection(snapshot)
        .await
        .map_err(MigrationError::CorePreflight)?;
    let mut plan = Vec::new();
    for source in lineup {
        plan.extend(plan_source(snapshot, source).await?);
    }
    Ok(plan)
}

async fn plan_source(
    snapshot: &mut PgConnection,
    source: &NamedMigrator,
) -> Result<Vec<PendingMigration>, MigrationError> {
    source.require_transactional()?;
    let rows = ledger_after_prepare(snapshot, source).await?;
    if !is_core_ledger(&source.migrator.table_name)
        && let Some(conflict) = ledger_conflict(&source.lane_versions(), &ledger_versions(&rows))
    {
        return Err(source.ledger_error(conflict));
    }
    let ledger = source.ledger();
    let pending = unrecorded(&source.lane(), &rows).map_err(|err| source.run_error(err))?;
    Ok(pending
        .into_iter()
        .map(|migration| PendingMigration {
            ledger: ledger.clone(),
            version: migration.version,
            description: migration.description.to_string(),
            checksum: migration.checksum.to_vec(),
        })
        .collect())
}

/// The rows `source`'s ledger holds when a run reaches the source's
/// migrations: the rows [`prepare_ledger`] leaves, read without preparing.
async fn ledger_after_prepare(
    snapshot: &mut PgConnection,
    source: &NamedMigrator,
) -> Result<Vec<LedgerRow>, MigrationError> {
    let own = read_ledger(snapshot, source, &source.ledger(), true).await?;
    if is_core_ledger(&source.migrator.table_name) {
        return Ok(own);
    }
    let shared = read_ledger(snapshot, source, CORE_LEDGER, true).await?;
    Ok(with_cutover_rows(own, shared, &source.cutover_versions()))
}

/// `own` plus the rows of `shared` at `versions` that `own` lacks, as
/// [`PREPARE_LEDGER`]'s `INSERT ... ON CONFLICT (version) DO NOTHING` leaves
/// them: a row the flavor's ledger already holds wins.
fn with_cutover_rows(
    own: Vec<LedgerRow>,
    shared: Vec<LedgerRow>,
    versions: &[i64],
) -> Vec<LedgerRow> {
    let mut rows: BTreeMap<i64, LedgerRow> =
        own.into_iter().map(|row| (row.version, row)).collect();
    for row in shared
        .into_iter()
        .filter(|row| versions.contains(&row.version))
    {
        rows.entry(row.version).or_insert(row);
    }
    rows.into_values().collect()
}

/// The migrations of `lane` that `rows` do not record, or the refusal
/// `SQLx`'s `run_direct` raises first for these rows: a `success = false` row
/// ([`MigrateError::Dirty`], the lowest version), then the first lane
/// migration whose recorded checksum differs
/// ([`MigrateError::VersionMismatch`]).
///
/// A recorded version the lane does not ship is never an error here: every
/// source runs with `ignore_missing` ([`prepare_sources`]).
fn unrecorded<'a>(
    lane: &[&'a Migration],
    rows: &[LedgerRow],
) -> Result<Vec<&'a Migration>, MigrateError> {
    if let Some(version) = rows
        .iter()
        .filter(|row| !row.success)
        .map(|row| row.version)
        .min()
    {
        return Err(MigrateError::Dirty(version));
    }
    let recorded: BTreeMap<i64, &LedgerRow> = rows.iter().map(|row| (row.version, row)).collect();
    let mut pending = Vec::new();
    for migration in lane {
        match recorded.get(&migration.version) {
            Some(row) if row.checksum.as_slice() != migration.checksum.as_ref() => {
                return Err(MigrateError::VersionMismatch(migration.version));
            }
            Some(_) => {}
            None => pending.push(*migration),
        }
    }
    Ok(pending)
}

impl MigrationRunReport {
    fn from_sources(sources: &[NamedMigrator]) -> Self {
        let sources = sources.iter().map(|source| source.source).collect();
        Self { sources }
    }
}

async fn pin_migration_search_path(conn: &mut PgConnection) -> Result<(), sqlx::Error> {
    sqlx::query("SET search_path TO public")
        .execute(&mut *conn)
        .await?;
    // The pool's request-serving `statement_timeout`
    // must not abort a long schema migration (CREATE INDEX / backfill) mid-way.
    // Disable it for this boot connection; `acquire_session_connection` marks
    // it close-on-drop so the override never returns to the shared pool.
    sqlx::query("SET statement_timeout = 0")
        .execute(&mut *conn)
        .await?;
    // Waiting *for* a lock is not the same as holding one. A migration that
    // takes ACCESS EXCLUSIVE (0011 rewrites four tables) queues behind any
    // in-flight reader on the outgoing release — and in Postgres a queued
    // exclusive request blocks every reader that arrives after it. With no
    // lock_timeout that pile-up is unbounded, so a rolling upgrade stalls the
    // whole table instead of the migration failing and retrying on the next
    // pod. Fail fast; the work itself still runs untimed once the lock is in
    // hand.
    sqlx::query(MIGRATION_LOCK_TIMEOUT_SQL)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// How long a migration waits for a table lock before giving up.
///
/// Short on purpose: the cost of failing is one pod restart, the cost of
/// waiting is every reader queued behind the request.
const MIGRATION_LOCK_TIMEOUT_SQL: &str = "SET lock_timeout = '5s'";

async fn reset_migration_search_path(conn: &mut PgConnection) -> Result<(), sqlx::Error> {
    sqlx::query("RESET search_path").execute(&mut *conn).await?;
    Ok(())
}

async fn run_sources_on_connection(
    conn: &mut PgConnection,
    sources: &[NamedMigrator],
) -> Result<(), MigrationError> {
    for source in sources {
        if source.source == CORE_SOURCE {
            run_source_with_contention_retry(conn, source).await?;
            prepare_ledger(conn, source).await?;
        } else {
            prepare_ledger(conn, source).await?;
            run_source_with_contention_retry(conn, source).await?;
        }
    }

    Ok(())
}

/// Total attempts for one source when shared-catalog contention is the only
/// failure. Contention needs another migrator racing on the same cluster, and
/// every round settles one winner whose role DDL is then committed — N racing
/// boots need at most N rounds, and a genuine catalog problem still surfaces
/// instead of looping.
const CATALOG_CONTENTION_ATTEMPTS: u32 = 5;

/// Base delay between contention retries; multiplied by the attempt number so
/// concurrent losers decorrelate without a randomness source.
const CATALOG_CONTENTION_BACKOFF: Duration = Duration::from_millis(100);

/// Run one source, retrying when it loses a shared-catalog race.
///
/// Role DDL in a migration (`ALTER ROLE`, `GRANT <role> TO ...`) updates
/// cluster-shared catalogs (`pg_authid`, `pg_auth_members`). No lock taken on
/// this connection can exclude a concurrent migrator on a *sibling database*
/// of the same cluster: Postgres advisory locks — `SQLx`'s per-run migrator
/// lock included — are scoped to the database in their lock tag, so two boots
/// migrating two different databases hold their locks independently and still
/// collide on the one shared tuple. The loser gets `tuple concurrently
/// updated` (raised by Postgres' non-MVCC catalog update path).
///
/// Retrying the whole source run is safe: each migration applies inside its
/// own transaction together with its ledger row, so a failed run recorded
/// nothing for the failing version, and the retry skips already-recorded
/// versions and re-executes only the loser. A failed attempt also leaves this
/// session's migrator advisory lock stacked (`run_direct` skips its unlock on
/// error, and re-locking on retry stacks); that is contained because the
/// facade never returns the migration connection to the pool.
///
/// A flavor's ledger is checked against its lane ([`LedgerConflict`]) under
/// the same lock, so no replica booting alongside changes it in between.
async fn run_source_with_contention_retry(
    conn: &mut PgConnection,
    source: &NamedMigrator,
) -> Result<(), MigrationError> {
    let mut attempt = 1;
    loop {
        source.require_transactional()?;
        let mut transaction = proxima_storage_pg::begin_migration_transaction(conn)
            .await
            .map_err(|error| {
                source.run_error(MigrateError::Execute(sqlx::Error::Protocol(
                    error.to_string(),
                )))
            })?;
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(MIGRATION_LOCK_KEY)
            .execute(&mut *transaction)
            .await
            .map_err(|error| source.run_error(MigrateError::Execute(error)))?;
        if !is_core_ledger(&source.migrator.table_name) {
            let recorded = ledger_versions(
                &read_ledger(&mut transaction, source, &source.ledger(), false).await?,
            );
            if let Some(conflict) = ledger_conflict(&source.lane_versions(), &recorded) {
                transaction
                    .rollback()
                    .await
                    .map_err(|error| source.run_error(MigrateError::Execute(error)))?;
                return Err(source.ledger_error(conflict));
            }
        }
        let result = source
            .migrator
            .run_direct(None, &mut *transaction, false)
            .await;
        let result = match result {
            Ok(()) => transaction.commit().await.map_err(MigrateError::Execute),
            Err(error) => {
                transaction
                    .rollback()
                    .await
                    .map_err(|error| source.run_error(MigrateError::Execute(error)))?;
                Err(error)
            }
        };
        match result {
            Err(err)
                if attempt < CATALOG_CONTENTION_ATTEMPTS && is_shared_catalog_contention(&err) =>
            {
                tracing::warn!(
                    source = source.source,
                    attempt,
                    error = %err,
                    "shared-catalog contention during migration; retrying"
                );
                tokio::time::sleep(CATALOG_CONTENTION_BACKOFF * attempt).await;
                attempt += 1;
            }
            result => return result.map_err(|err| source.run_error(err)),
        }
    }
}

/// Whether a migration failure is a lost race on a cluster-shared catalog.
///
/// The race has two server-side shapes. Updating an *existing* shared tuple
/// concurrently raises `tuple concurrently updated` — matched on the message
/// because Postgres files it under the catch-all `XX000` internal-error
/// SQLSTATE, which would sweep in genuine corruption (under a non-English
/// `lc_messages` the match misses and the boot fails exactly as it did
/// without the retry — degraded to the status quo, never looser). A
/// concurrent *first* write of a shared tuple instead surfaces as a
/// unique-key violation (`23505`) on the catalog's index — both sessions saw
/// no row and both insert — so `23505` counts only when the named table is
/// one of the role-DDL shared catalogs, where re-running converges (the
/// retry's write finds the winner's row and updates it, or reports the true
/// conflict, e.g. a duplicate `CREATE ROLE`).
fn is_shared_catalog_contention(err: &MigrateError) -> bool {
    let (MigrateError::Execute(sqlx::Error::Database(db))
    | MigrateError::ExecuteMigration(sqlx::Error::Database(db), _)) = err
    else {
        return false;
    };
    if matches!(
        db.message(),
        "tuple concurrently updated" | "tuple concurrently deleted"
    ) {
        return true;
    }
    db.code().as_deref() == Some("23505")
        && matches!(
            db.table(),
            Some("pg_authid" | "pg_auth_members" | "pg_db_role_setting")
        )
}

/// Prepare a flavor's own ledger before its migrator first reads it: create
/// it, copy in the rows core's shared ledger holds for its versions, and
/// leave it read-only for every role but its owner. Runs on every migration
/// run; every step is idempotent. Core's ledger, which `SQLx` creates on
/// core's first run, gets the read-only step alone, after core migrates.
///
/// - Serialized. Pods booting together race on `CREATE SCHEMA`, `CREATE
///   TABLE` (`pg_type` unique index) and the ledger's ACL ("tuple
///   concurrently updated"), all before `SQLx`'s own migrator lock; the
///   transaction takes [`MIGRATION_LOCK_KEY`] first.
/// - Copy, not move. A database migrated before the ledger split, or a
///   flavor switching from `NamedMigrator::new` to [`NamedMigrator::flavor`],
///   carries the flavor's rows in `public._sqlx_migrations`; without them
///   `SQLx` would re-run the flavor's DDL against the empty new table. The
///   rows also stay where they were, so an older binary still on the shared
///   ledger (a rollback, a pod restarted mid-deploy) re-runs nothing either;
///   core ignores versions above its ceiling. Rows are matched by version:
///   a checksum that differs then fails the flavor's run as `SQLx`'s
///   version mismatch instead of re-running it.
/// - Read-only. The platform role's default privileges hand every new
///   table's DML to the runtime role, and a runtime role that can delete a
///   ledger row makes the next boot re-run that migration — a destructive
///   baseline included. Every non-owner INSERT/UPDATE/DELETE/TRUNCATE grant
///   on the ledger is revoked (`CASCADE`, so grants passed on go too); SELECT
///   stays. A role that owns the ledger then refuses the boot if any such
///   grant is left; one that does not can revoke only what it granted.
async fn prepare_ledger(
    conn: &mut PgConnection,
    source: &NamedMigrator,
) -> Result<(), MigrationError> {
    let table_name = source.migrator().table_name.clone();
    let map_err = |err: sqlx::Error| MigrationError::FlavorLedgerCutover {
        source: source.source,
        err,
    };
    let versions = source.cutover_versions();

    let mut tx = conn.begin().await.map_err(map_err)?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(MIGRATION_LOCK_KEY)
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
    let schemas = if is_core_ledger(&table_name) {
        &[][..]
    } else {
        &source.migrator().create_schemas[..]
    };
    for schema in schemas {
        // SQL-POLICY: fixed-fragment — `schema` is a compiled-in
        // `create_schemas` entry from the flavor crate's migrator; no value
        // reaches it from a caller. Interpolated as-is, like `SQLx` itself
        // interpolates it in `create_schema_if_not_exists`.
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "CREATE SCHEMA IF NOT EXISTS {schema}"
        )))
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
    }
    sqlx::query(
        "SELECT set_config('proxima.flavor_ledger', $1, true),
                set_config('proxima.flavor_ledger_versions', $2::bigint[]::text, true)",
    )
    .bind(&table_name)
    .bind(&versions)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;
    sqlx::query(PREPARE_LEDGER)
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
    tx.commit().await.map_err(map_err)
}

/// `pg_advisory_xact_lock` key taken first in every transaction this module
/// opens — each source's run and each ledger's preparation: ASCII
/// `proxmigr`, beside `runtime_grants`' `proxgrnt`. `SQLx`'s own migrator lock
/// is session-level and released inside `run_direct`, before the enclosing
/// transaction commits; alone, it let a replica booting alongside read the
/// ledger before the first one's rows were visible and re-apply them
/// (`schema "proxima_core" already exists`). The wait is bounded by
/// [`MIGRATION_LOCK_TIMEOUT_SQL`], as `SQLx`'s is.
const MIGRATION_LOCK_KEY: i64 = i64::from_be_bytes(*b"proxmigr");

/// [`prepare_ledger`]'s statement. The ledger name arrives through a
/// transaction-local setting, so the statement text is fixed; it is the
/// flavor migrator's compiled-in table name, interpolated as `SQLx` itself
/// interpolates it in `ensure_migrations_table`.
const PREPARE_LEDGER: &str = r"DO $prepare_ledger$
DECLARE
    ledger text := current_setting('proxima.flavor_ledger');
    versions bigint[] := current_setting('proxima.flavor_ledger_versions')::bigint[];
    ledger_oid regclass;
    grantee oid;
BEGIN
    IF to_regclass('public._sqlx_migrations') IS NULL THEN
        RAISE EXCEPTION 'core ledger public._sqlx_migrations is missing; core migrates first';
    END IF;
    IF to_regclass(ledger) IS DISTINCT FROM 'public._sqlx_migrations'::regclass THEN
        EXECUTE format('CREATE TABLE IF NOT EXISTS %s (LIKE public._sqlx_migrations INCLUDING ALL)', ledger);
        EXECUTE format('INSERT INTO %s SELECT * FROM public._sqlx_migrations WHERE version = ANY($1) ON CONFLICT (version) DO NOTHING', ledger::regclass)
            USING versions;
    END IF;
    ledger_oid := ledger::regclass;
    FOR grantee IN
        SELECT DISTINCT acl.grantee
          FROM pg_class AS c, aclexplode(c.relacl) AS acl
         WHERE c.oid = ledger_oid AND acl.grantee <> c.relowner
           AND acl.privilege_type IN ('INSERT', 'UPDATE', 'DELETE', 'TRUNCATE')
    LOOP
        EXECUTE format('REVOKE INSERT, UPDATE, DELETE, TRUNCATE ON %s FROM %s CASCADE', ledger_oid,
            CASE WHEN grantee = 0 THEN 'PUBLIC' ELSE quote_ident(pg_get_userbyid(grantee)) END);
    END LOOP;
    IF EXISTS (SELECT 1
                 FROM pg_class AS c, aclexplode(c.relacl) AS acl
                WHERE c.oid = ledger_oid AND pg_has_role(c.relowner, 'USAGE')
                  AND acl.grantee <> c.relowner
                  AND acl.privilege_type IN ('INSERT', 'UPDATE', 'DELETE', 'TRUNCATE')) THEN
        RAISE EXCEPTION 'migration ledger % is still writable by a role other than its owner', ledger_oid;
    END IF;
END
$prepare_ledger$";

impl NamedMigrator {
    fn run_error(&self, err: MigrateError) -> MigrationError {
        if self.source == CORE_SOURCE {
            MigrationError::Core(err)
        } else {
            MigrationError::Flavor {
                source: self.source,
                err,
            }
        }
    }

    fn ledger_error(&self, conflict: LedgerConflict) -> MigrationError {
        MigrationError::Ledger {
            source: self.source,
            ledger: self.migrator.table_name.to_string(),
            conflict,
        }
    }

    /// A run applies each source inside the transaction that also carries
    /// its scope and ledger rows, so it refuses a `no_tx` migration before
    /// it touches the ledger.
    fn require_transactional(&self) -> Result<(), MigrationError> {
        if self.migrator.iter().any(|migration| migration.no_tx) {
            return Err(self.run_error(MigrateError::Execute(sqlx::Error::Protocol(
                "platform-scoped migrations require transactional migration files".into(),
            ))));
        }
        Ok(())
    }

    /// The schema-qualified table this source records on, spelled as
    /// [`flavor_ledger_table`] spells a flavor's. A table declared without a
    /// schema lives in `public`, where a run pins `search_path`.
    pub(crate) fn ledger(&self) -> String {
        let table = self.migrator.table_name.as_ref();
        if table.contains('.') {
            table.to_owned()
        } else {
            format!("public.{table}")
        }
    }

    /// What two sources must not share: the table Postgres resolves
    /// [`Self::ledger`] to, whatever its spelling. Matching follows Postgres'
    /// identifier rules ([`parse_ledger_name`]); a spelling outside them is
    /// compared as written.
    fn ledger_identity(&self) -> LedgerIdentity {
        let name = self.migrator.table_name.as_ref();
        match parse_ledger_name(name) {
            Some((schema, table)) => LedgerIdentity::Resolved { schema, table },
            None => LedgerIdentity::Spelled(name.to_owned()),
        }
    }

    /// What a run applies for this source, in the order it applies it: every
    /// migration it ships but its down migrations, in the migrator's own
    /// order, which is what `SQLx`'s `run_direct` walks. `sqlx::migrate!`
    /// yields ascending versions; a migrator built by hand keeps the order it
    /// was given, and the plan, like the run, applies and refuses in that
    /// order. The one enumeration the ledger checks, [`pending_migrations`]
    /// and a test-template fingerprint share; [`Self::ledger`] names the table
    /// each entry records on.
    ///
    /// A version a source repeats is listed once, at its first place; a
    /// lineup with one is refused before a run or a plan reads the database
    /// ([`validate_sources`]).
    pub(crate) fn lane(&self) -> Vec<&Migration> {
        let mut seen = BTreeSet::new();
        self.migrator
            .iter()
            .filter(|migration| !migration.migration_type.is_down_migration())
            .filter(|migration| seen.insert(migration.version))
            .collect()
    }

    /// The versions of [`Self::lane`], ascending: the lineage view of a
    /// ledger, where the order a run walks does not matter.
    fn lane_versions(&self) -> Vec<i64> {
        let versions: BTreeSet<i64> = self
            .lane()
            .into_iter()
            .map(|migration| migration.version)
            .collect();
        versions.into_iter().collect()
    }

    /// The versions whose rows [`prepare_ledger`] copies from core's ledger
    /// into this source's own: everything the migrator ships, down
    /// migrations included.
    fn cutover_versions(&self) -> Vec<i64> {
        self.migrator
            .iter()
            .map(|migration| migration.version)
            .collect()
    }
}

/// A ledger as two sources compare it. Private: only the shared-ledger check
/// reads it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum LedgerIdentity {
    /// The table the name resolves to.
    Resolved { schema: String, table: String },
    /// A spelling [`parse_ledger_name`] does not read (a comment inside the
    /// name, say): all that is known is how it is written.
    Spelled(String),
}

impl std::fmt::Display for LedgerIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Resolved { schema, table } => write!(f, "{schema}.{table}"),
            Self::Spelled(name) => f.write_str(name),
        }
    }
}

/// Postgres' whitespace between the tokens of a name.
fn is_name_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0b' | '\x0c')
}

/// The `(schema, table)` a ledger name resolves to, by Postgres' rules for an
/// identifier: parts split on `.` outside double quotes, with whitespace
/// allowed around parts and dots; a quoted part keeps its case, with `""` for
/// `"`; an unquoted part folds to ASCII lower case; each is cut to 63 bytes
/// (`NAMEDATALEN` - 1) on a character boundary; a name without a schema is in
/// `public`, where a run pins `search_path`.
///
/// `None` for a name that is not one or two such parts. That says nothing
/// against the name (SQL accepts more spellings than this reads, comments
/// among them): it is not matched by what it resolves to.
fn parse_ledger_name(name: &str) -> Option<(String, String)> {
    const MAX_IDENTIFIER_BYTES: usize = 63;
    let mut parts: Vec<String> = Vec::new();
    let mut chars = name.chars().peekable();
    loop {
        while chars.next_if(|c| is_name_space(*c)).is_some() {}
        let mut part = String::new();
        if chars.next_if_eq(&'"').is_some() {
            loop {
                match chars.next()? {
                    '"' if chars.next_if_eq(&'"').is_some() => part.push('"'),
                    '"' => break,
                    c => part.push(c),
                }
            }
            if part.is_empty() {
                return None;
            }
        } else {
            while let Some(c) = chars.next_if(|c| *c != '.' && !is_name_space(*c)) {
                part.push(c);
            }
            let mut letters = part.chars();
            let starts = letters
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || !c.is_ascii());
            if !starts
                || !letters
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$' || !c.is_ascii())
            {
                return None;
            }
            part.make_ascii_lowercase();
        }
        let mut end = part.len().min(MAX_IDENTIFIER_BYTES);
        while !part.is_char_boundary(end) {
            end -= 1;
        }
        part.truncate(end);
        parts.push(part);
        while chars.next_if(|c| is_name_space(*c)).is_some() {}
        match chars.next() {
            None => break,
            Some('.') => {}
            Some(_) => return None,
        }
    }
    match <[String; 1]>::try_from(parts) {
        Ok([table]) => Some(("public".to_owned(), table)),
        Err(parts) => <[String; 2]>::try_from(parts)
            .ok()
            .map(|[schema, table]| (schema, table)),
    }
}

/// One row of a migration ledger, as `SQLx` writes it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LedgerRow {
    version: i64,
    checksum: Vec<u8>,
    success: bool,
}

fn ledger_versions(rows: &[LedgerRow]) -> Vec<i64> {
    rows.iter().map(|row| row.version).collect()
}

/// The rows of `ledger`, ascending by version, on behalf of `source`. A
/// ledger that does not exist holds nothing when `may_be_absent`; the
/// migration run creates every flavor ledger before reading it.
///
/// The one ledger read of the runner's lineage check, the preflight and the
/// plan. It reads what `SQLx`'s run reads (`list_applied_migrations`, every
/// row whatever its `success`) and adds the `success` that its
/// `dirty_version` reads separately.
async fn read_ledger(
    conn: &mut PgConnection,
    source: &NamedMigrator,
    ledger: &str,
    may_be_absent: bool,
) -> Result<Vec<LedgerRow>, MigrationError> {
    let read_error = |err| MigrationError::LedgerRead {
        source: source.source,
        ledger: ledger.to_owned(),
        err: MigrateError::Execute(err),
    };
    if may_be_absent {
        let exists: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
            .bind(ledger)
            .fetch_one(&mut *conn)
            .await
            .map_err(read_error)?;
        if !exists {
            return Ok(Vec::new());
        }
    }
    // SQL-POLICY: fixed-fragment — `ledger` is a compiled-in migrator table
    // name (`NamedMigrator::ledger`); no value reaches it from a caller.
    // Interpolated as-is, like `SQLx` itself interpolates it in
    // `list_applied_migrations`.
    let rows: Vec<(i64, Vec<u8>, bool)> = sqlx::query_as(AssertSqlSafe(format!(
        "SELECT version, checksum, success FROM {ledger} ORDER BY version"
    )))
    .fetch_all(&mut *conn)
    .await
    .map_err(read_error)?;
    Ok(rows
        .into_iter()
        .map(|(version, checksum, success)| LedgerRow {
            version,
            checksum,
            success,
        })
        .collect())
}

/// Whether a lane (`lane`, the versions a binary ships) continues the one
/// its ledger records (`recorded`). Both ascending.
///
/// Recorded versions the binary does not ship are expected: a newer
/// release's fleet on the same database, or a file the lane shed. They
/// conflict only beside pending work that would repeat or reorder theirs:
/// the lane's first migration still pending ([`LedgerConflict::Replaced`]),
/// or a pending migration older than one of them
/// ([`LedgerConflict::Diverged`]).
fn ledger_conflict(lane: &[i64], recorded: &[i64]) -> Option<LedgerConflict> {
    let shipped: BTreeSet<i64> = lane.iter().copied().collect();
    let applied: BTreeSet<i64> = recorded.iter().copied().collect();
    let unknown: Vec<i64> = applied.difference(&shipped).copied().collect();
    let pending: Vec<i64> = shipped.difference(&applied).copied().collect();
    let (&root, &newest_unknown, &oldest_pending) =
        (shipped.first()?, unknown.last()?, pending.first()?);
    if oldest_pending == root {
        return Some(LedgerConflict::Replaced { root, unknown });
    }
    if oldest_pending < newest_unknown {
        return Some(LedgerConflict::Diverged {
            pending: pending
                .into_iter()
                .filter(|version| *version < newest_unknown)
                .collect(),
            unknown: unknown
                .into_iter()
                .filter(|version| *version > oldest_pending)
                .collect(),
        });
    }
    None
}

/// The lane's migrations its ledger does not record, for a boot that
/// applies none.
fn unapplied(lane: &[i64], recorded: &[i64]) -> Option<LedgerConflict> {
    let pending: Vec<i64> = lane
        .iter()
        .copied()
        .filter(|version| recorded.binary_search(version).is_err())
        .collect();
    (!pending.is_empty()).then_some(LedgerConflict::Unapplied { pending })
}

fn core_source() -> NamedMigrator {
    NamedMigrator::new(CORE_SOURCE, core_migrator())
}

fn prepare_sources(
    flavors: impl IntoIterator<Item = NamedMigrator>,
) -> Result<Vec<NamedMigrator>, MigrationError> {
    let sources = lineup(flavors);
    check_lineup(&sources)?;
    Ok(sources)
}

/// Core followed by `flavors` in the given order, each prepared the way a
/// run needs it (`ignore_missing`). Nothing is refused here: a run
/// validates the lineup ([`check_lineup`]) before it connects.
pub(crate) fn lineup(flavors: impl IntoIterator<Item = NamedMigrator>) -> Vec<NamedMigrator> {
    let mut sources = vec![core_source()];

    for mut source in flavors {
        source.migrator.set_ignore_missing(true);
        if is_core_ledger(&source.migrator.table_name) {
            tracing::warn!(
                source = source.source,
                "flavor records its migrations in core's public._sqlx_migrations; \
                 build it with NamedMigrator::flavor to give it its own ledger \
                 (docs/09 §Migrations)"
            );
        }
        sources.push(source);
    }
    sources
}

/// [`validate_sources`] over a [`lineup`]: the one check a run, a plan and a
/// test template make of the sources they are about to use.
pub(crate) fn check_lineup(sources: &[NamedMigrator]) -> Result<(), MigrationError> {
    validate_sources(&sources.iter().collect::<Vec<_>>())
}

/// The refusals that need no database, for core followed by the host's
/// sources: a run raises them before it connects, a plan likewise.
fn validate_sources(sources: &[&NamedMigrator]) -> Result<(), MigrationError> {
    reject_duplicate_versions(sources)?;
    reject_shared_flavor_ledgers(sources)
}

fn reject_shared_flavor_ledgers(sources: &[&NamedMigrator]) -> Result<(), MigrationError> {
    let mut seen: BTreeMap<LedgerIdentity, &'static str> = BTreeMap::new();
    for source in sources.iter().skip(1) {
        if is_core_ledger(&source.migrator.table_name) {
            continue;
        }
        // The table Postgres resolves the name to, not its spelling:
        // `_sqlx_migrations_host`, `public."_sqlx_migrations_host"` and
        // `PUBLIC._SQLX_Migrations_Host` are one table.
        let identity = source.ledger_identity();
        if let Some(first_source) = seen.insert(identity.clone(), source.source) {
            return Err(MigrationError::DuplicateLedger {
                table: identity.to_string(),
                first_source,
                second_source: source.source,
            });
        }
    }
    Ok(())
}

fn reject_duplicate_versions(sources: &[&NamedMigrator]) -> Result<(), MigrationError> {
    let mut seen: BTreeMap<i64, (&'static str, String)> = BTreeMap::new();

    for source in sources {
        for migration in source.migrator.iter() {
            let description = migration.description.to_string();
            if let Some((first_source, first_description)) =
                seen.insert(migration.version, (source.source, description.clone()))
            {
                return Err(MigrationError::DuplicateVersion {
                    version: migration.version,
                    first_source,
                    first_description,
                    second_source: source.source,
                    second_description: description,
                });
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use sqlx::SqlSafeStr;
    use sqlx::migrate::{MigrateError, Migration, MigrationType, Migrator};

    use super::{
        LedgerConflict, LedgerRow, MigrationError, NamedMigrator, core_source, flavor_ledger_table,
        ledger_conflict, pending_migrations, prepare_sources, unapplied, unrecorded,
        with_cutover_rows,
    };

    const TEST_FLAVOR_VERSION: i64 = 20_260_612_000_010;

    fn migrator(versions: &[i64]) -> Migrator {
        let migrations = versions
            .iter()
            .map(|version| {
                Migration::new(
                    *version,
                    Cow::Owned(format!("test {version}")),
                    MigrationType::Simple,
                    sqlx::AssertSqlSafe(format!("SELECT {version};")).into_sql_str(),
                    false,
                )
            })
            .collect();
        Migrator {
            migrations: Cow::Owned(migrations),
            ..Migrator::DEFAULT
        }
    }

    #[test]
    fn duplicate_versions_fail_before_run() {
        let err = prepare_sources([
            NamedMigrator::new("alpha", migrator(&[TEST_FLAVOR_VERSION])),
            NamedMigrator::new("beta", migrator(&[TEST_FLAVOR_VERSION])),
        ])
        .expect_err("duplicate migration version should fail");

        assert!(matches!(
            err,
            MigrationError::DuplicateVersion {
                version: TEST_FLAVOR_VERSION,
                first_source: "alpha",
                second_source: "beta",
                ..
            }
        ));
    }

    #[test]
    fn a_declared_ledger_is_kept_and_two_flavors_never_share_one() {
        let mut declared = migrator(&[TEST_FLAVOR_VERSION]);
        declared.dangerous_set_table_name("acme._sqlx_migrations");
        assert_eq!(
            NamedMigrator::flavor("acme", declared)
                .migrator()
                .table_name,
            "acme._sqlx_migrations",
            "a ledger already in use is never renamed under a live database"
        );

        let err = prepare_sources([
            NamedMigrator::flavor("a-b", migrator(&[TEST_FLAVOR_VERSION])),
            NamedMigrator::flavor("a_b", migrator(&[TEST_FLAVOR_VERSION + 1])),
        ])
        .expect_err("two flavors on one ledger");
        assert!(matches!(
            err,
            MigrationError::DuplicateLedger {
                first_source: "a-b",
                second_source: "a_b",
                ..
            }
        ));
        prepare_sources([
            NamedMigrator::new("alpha", migrator(&[TEST_FLAVOR_VERSION])),
            NamedMigrator::new("beta", migrator(&[TEST_FLAVOR_VERSION + 1])),
        ])
        .expect("flavors still on core's shared ledger are a warning, not a refusal");
    }

    #[test]
    fn a_flavor_id_that_would_need_quoting_is_refused() {
        for id in [
            "",
            "Acme",
            "9acme",
            "acme.forge",
            "acme forge",
            "acme\"",
            &"a".repeat(41),
        ] {
            let result = std::panic::catch_unwind(|| flavor_ledger_table(id));
            assert!(result.is_err(), "{id:?} must be refused");
            assert!(!super::is_flavor_ledger_id(id));
        }
    }

    /// Every shape a flavor ledger meets in practice, against the lane a
    /// binary ships. Versions stand for dated files: 1 is the oldest.
    #[test]
    fn a_lane_that_does_not_continue_its_ledger_is_refused() {
        let continues: [(&str, &[i64], &[i64]); 8] = [
            ("a fresh ledger", &[1, 2], &[]),
            ("an upgrade", &[1, 2, 3], &[1, 2]),
            ("the same release", &[1, 2], &[1, 2]),
            (
                "an older release's fleet beside a newer one",
                &[1, 2],
                &[1, 2, 3],
            ),
            (
                "a shed first file, a new one pending",
                &[2, 3, 4],
                &[1, 2, 3],
            ),
            (
                "a shed middle file, a new one pending",
                &[1, 3, 4],
                &[1, 2, 3],
            ),
            ("a branch merged under a newer version", &[1, 2, 3], &[1, 3]),
            // Not caught here: the kept first migration looks like an
            // upgrade, so only a publish-time check sees the squash.
            ("a lane squashed in part", &[1, 9], &[1, 2, 3]),
        ];
        for (shape, lane, recorded) in continues {
            assert_eq!(ledger_conflict(lane, recorded), None, "{shape}");
        }

        assert_eq!(
            ledger_conflict(
                &[20_260_924_000_060],
                &[20_260_822_000_060, 20_260_904_000_060]
            ),
            Some(LedgerConflict::Replaced {
                root: 20_260_924_000_060,
                unknown: vec![20_260_822_000_060, 20_260_904_000_060],
            }),
            "a new baseline over the lane it replaced (forgejo 0.0.7 over 0.0.5)"
        );
        assert_eq!(
            ledger_conflict(
                &[20_260_822_000_060, 20_260_904_000_060],
                &[20_260_924_000_060]
            ),
            Some(LedgerConflict::Replaced {
                root: 20_260_822_000_060,
                unknown: vec![20_260_924_000_060],
            }),
            "the replaced lane's fleet meeting the database the new baseline built"
        );
        assert_eq!(
            ledger_conflict(&[1, 2, 3, 4], &[1, 9]),
            Some(LedgerConflict::Diverged {
                pending: vec![2, 3, 4],
                unknown: vec![9],
            }),
            "an older release after a later one squashed its files 2-4 into 9"
        );
        assert_eq!(
            ledger_conflict(&[1, 2, 5, 12], &[1, 2, 7, 9, 11]),
            Some(LedgerConflict::Diverged {
                pending: vec![5],
                unknown: vec![7, 9, 11],
            }),
            "only the pending versions below an unknown one, and the unknown ones above them"
        );
    }

    #[test]
    fn a_boot_that_skips_migrations_names_what_is_not_applied() {
        assert_eq!(unapplied(&[1, 2], &[1, 2, 3]), None);
        assert_eq!(
            unapplied(&[1, 2, 3], &[1]),
            Some(LedgerConflict::Unapplied {
                pending: vec![2, 3]
            })
        );
    }

    fn recorded(migration: &Migration) -> LedgerRow {
        LedgerRow {
            version: migration.version,
            checksum: migration.checksum.to_vec(),
            success: true,
        }
    }

    fn pending_versions(
        source: &NamedMigrator,
        rows: &[LedgerRow],
    ) -> Result<Vec<i64>, MigrateError> {
        unrecorded(&source.lane(), rows).map(|pending| {
            pending
                .into_iter()
                .map(|migration| migration.version)
                .collect()
        })
    }

    #[test]
    fn a_source_enumerates_what_a_run_applies() {
        let mut migrations = migrator(&[3, 1, 2]).migrations.into_owned();
        migrations.push(Migration::new(
            2,
            Cow::Borrowed("test 2 down"),
            MigrationType::ReversibleDown,
            sqlx::AssertSqlSafe("SELECT 2;".to_owned()).into_sql_str(),
            false,
        ));
        let source = NamedMigrator::new(
            "alpha",
            Migrator {
                migrations: Cow::Owned(migrations),
                ..Migrator::DEFAULT
            },
        );

        assert_eq!(
            source.lane_versions(),
            vec![1, 2, 3],
            "the lineage view is ascending, and a down migration is not applied"
        );
        assert_eq!(
            source
                .lane()
                .iter()
                .map(|migration| migration.description.as_ref())
                .collect::<Vec<_>>(),
            ["test 3", "test 1", "test 2"],
            "the order the migrator holds, which `run_direct` walks; the up migration of version 2, not its down"
        );
        assert_eq!(
            source.cutover_versions().len(),
            4,
            "the ledger preparation copies a version's row whatever its type"
        );
    }

    #[test]
    fn a_source_out_of_order_is_planned_and_refused_in_the_order_a_run_walks_it() {
        let source = NamedMigrator::new("alpha", migrator(&[3, 1, 2]));
        assert_eq!(
            pending_versions(&source, &[]).expect("empty"),
            [3, 1, 2],
            "pending in the order `run_direct` applies"
        );

        let lane = source.lane();
        let mut rows: Vec<LedgerRow> = lane.iter().map(|m| recorded(m)).collect();
        rows.sort_by_key(|row| row.version);
        for row in &mut rows {
            if row.version != 2 {
                row.checksum = vec![0];
            }
        }
        assert!(
            matches!(
                pending_versions(&source, &rows),
                Err(MigrateError::VersionMismatch(3))
            ),
            "the first changed checksum in the order the run walks, not the lowest version"
        );
    }

    /// Two sources on `tables`, the second a version above the first.
    fn on_tables(first: &str, second: &str) -> Vec<NamedMigrator> {
        [("first", first, 0), ("second", second, 1)]
            .into_iter()
            .map(|(source, table, step)| {
                let mut declared = migrator(&[TEST_FLAVOR_VERSION + step]);
                declared.dangerous_set_table_name(table.to_owned());
                NamedMigrator::new(source, declared)
            })
            .collect()
    }

    /// The runner's first step, then the plan's: both before any query, the
    /// plan on a pool nothing listens behind.
    async fn lineup_refusals(
        sources: impl Fn() -> Vec<NamedMigrator>,
    ) -> (Result<(), MigrationError>, Result<(), MigrationError>) {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(std::time::Duration::from_millis(300))
            .connect_lazy("postgres://nobody@127.0.0.1:1/none")
            .expect("a lazy pool parses its URL and connects later");
        (
            prepare_sources(sources()).map(drop),
            pending_migrations(&pool, &sources()).await.map(drop),
        )
    }

    #[tokio::test]
    async fn a_ledger_is_matched_by_the_table_postgres_resolves_not_by_its_spelling() {
        let one_table = |err: Result<(), MigrationError>| match err {
            Err(MigrationError::DuplicateLedger {
                table,
                first_source: "first",
                second_source: "second",
            }) => table,
            other => panic!("expected DuplicateLedger, got {other:?}"),
        };
        let long = "y".repeat(70);
        for (first, second, table) in [
            (
                "_sqlx_migrations_host",
                "public._sqlx_migrations_host",
                "public._sqlx_migrations_host",
            ),
            (
                "public._sqlx_migrations_host",
                "public.\"_sqlx_migrations_host\"",
                "public._sqlx_migrations_host",
            ),
            (
                "_SQLX_Migrations_Host",
                "_sqlx_migrations_host",
                "public._sqlx_migrations_host",
            ),
            (
                "\"_sqlx_migrations_host\"",
                "PUBLIC._SQLX_MIGRATIONS_HOST",
                "public._sqlx_migrations_host",
            ),
            (
                "_sqlx_migrations_host",
                "public . _sqlx_migrations_host",
                "public._sqlx_migrations_host",
            ),
            ("\"Host\"", "public.\"Host\"", "public.Host"),
            ("\"a\"\"b\"", "public.\"a\"\"b\"", "public.a\"b"),
            (
                &format!("x{long}a"),
                &format!("x{long}b"),
                &format!("public.x{}", &long[..62]),
            ),
        ] {
            let (run, plan) = lineup_refusals(|| on_tables(first, second)).await;
            assert_eq!(one_table(run), table, "{first} and {second}");
            assert_eq!(one_table(plan), table, "{first} and {second}");
        }
    }

    #[tokio::test]
    async fn ledgers_that_resolve_to_different_tables_are_not_one_ledger() {
        for (first, second) in [
            ("\"Host\"", "\"host\""),
            ("\"Host\"", "host"),
            ("a.t", "b.t"),
            ("\"a.t\"", "a.t"),
            // Schema `a.b`, table `c` and schema `a`, table `b.c` print alike.
            ("\"a.b\".c", "a.\"b.c\""),
        ] {
            let (run, plan) = lineup_refusals(|| on_tables(first, second)).await;
            run.unwrap_or_else(|err| panic!("{first} and {second}: {err:?}"));
            assert!(
                matches!(plan, Err(MigrationError::Connection(_))),
                "{first} and {second} reach for the database: {plan:?}"
            );
        }
    }

    /// A lineup `main` completes is never refused for how a ledger is
    /// spelled: a name the parser does not read is compared as written.
    #[tokio::test]
    async fn a_spelling_outside_postgres_identifier_rules_is_compared_as_written() {
        for name in [
            "\"unclosed",
            "public.",
            ".host",
            "a..b",
            "a.b.c",
            "\"\"",
            "a-b",
            "a/* comment */.b",
            "\"a\"b",
            "",
        ] {
            assert_eq!(super::parse_ledger_name(name), None, "{name:?}");
            let (run, plan) =
                lineup_refusals(|| on_tables(name, "public._sqlx_migrations_ok")).await;
            run.unwrap_or_else(|err| panic!("{name:?}: {err:?}"));
            assert!(
                matches!(plan, Err(MigrationError::Connection(_))),
                "{name:?} reaches for the database: {plan:?}"
            );

            let (run, plan) = lineup_refusals(|| on_tables(name, name)).await;
            for same in [run, plan] {
                assert!(
                    matches!(
                        &same,
                        Err(MigrationError::DuplicateLedger { table, .. }) if table == name
                    ),
                    "the same spelling twice is one ledger: {same:?}"
                );
            }
        }
    }

    #[tokio::test]
    async fn whitespace_around_the_parts_of_a_ledger_name_is_accepted() {
        let (run, plan) =
            lineup_refusals(|| on_tables("public . _sqlx_migrations_host", "other.t")).await;
        run.expect("the runner takes it");
        assert!(
            matches!(plan, Err(MigrationError::Connection(_))),
            "the plan reaches for the database: {plan:?}"
        );
    }

    #[test]
    fn a_ledger_name_is_read_as_postgres_reads_an_identifier() {
        let identity = |name: &str| super::parse_ledger_name(name).expect(name);
        let pair = |schema: &str, table: &str| (schema.to_owned(), table.to_owned());
        assert_eq!(identity("host"), pair("public", "host"));
        assert_eq!(
            identity(" public\t.\n\"Host\" "),
            pair("public", "Host"),
            "whitespace around parts and dots"
        );
        assert_eq!(identity("Acme.Host"), pair("acme", "host"));
        assert_eq!(identity("\"Acme\".\"Host\""), pair("Acme", "Host"));
        assert_eq!(
            identity("\"a.b\""),
            pair("public", "a.b"),
            "a dot inside quotes"
        );
        assert_eq!(identity("\"a\"\"b\""), pair("public", "a\"b"));
        assert_eq!(identity("a$b"), pair("public", "a$b"));
        assert_eq!(
            identity("\u{c9}t\u{e9}"),
            pair("public", "\u{c9}t\u{e9}"),
            "ASCII only"
        );
        let clipped = format!("{}\u{e9}", "a".repeat(62));
        assert_eq!(
            identity(&clipped),
            pair("public", &"a".repeat(62)),
            "cut to 63 bytes on a character boundary"
        );
    }

    #[test]
    fn a_ledger_is_named_as_flavor_ledger_table_names_it() {
        assert_eq!(core_source().ledger(), "public._sqlx_migrations");
        assert_eq!(
            NamedMigrator::flavor("proxima-code", migrator(&[1])).ledger(),
            flavor_ledger_table("proxima-code")
        );
        let mut declared = migrator(&[1]);
        declared.dangerous_set_table_name("acme._sqlx_migrations");
        assert_eq!(
            NamedMigrator::flavor("acme", declared).ledger(),
            "acme._sqlx_migrations"
        );
        let mut unqualified = migrator(&[1]);
        unqualified.dangerous_set_table_name("_sqlx_migrations_host");
        assert_eq!(
            NamedMigrator::new("host", unqualified).ledger(),
            "public._sqlx_migrations_host",
            "a run pins `search_path` to public"
        );
    }

    #[test]
    fn the_rows_of_a_ledger_leave_what_a_run_would_apply() {
        let source = NamedMigrator::new("alpha", migrator(&[1, 2, 3, 4]));
        let lane = source.lane();
        let first_two: Vec<LedgerRow> = lane[..2].iter().map(|m| recorded(m)).collect();

        assert_eq!(pending_versions(&source, &[]).expect("empty"), [1, 2, 3, 4]);
        assert_eq!(
            pending_versions(&source, &first_two).expect("behind"),
            [3, 4]
        );
        let mut with_unknown = first_two.clone();
        with_unknown.push(LedgerRow {
            version: 9,
            checksum: vec![9],
            success: true,
        });
        assert_eq!(
            pending_versions(&source, &with_unknown).expect("a recorded version it does not ship"),
            [3, 4],
            "every source runs with `ignore_missing`"
        );

        let mut changed_second = first_two.clone();
        changed_second[1].checksum = vec![0];
        let mut changed_both = changed_second.clone();
        changed_both[0].checksum = vec![0];
        assert!(matches!(
            pending_versions(&source, &changed_second),
            Err(MigrateError::VersionMismatch(2))
        ));
        assert!(
            matches!(
                pending_versions(&source, &changed_both),
                Err(MigrateError::VersionMismatch(1))
            ),
            "the first in the order the run walks"
        );

        let mut dirty = changed_both.clone();
        dirty.push(LedgerRow {
            version: 7,
            checksum: vec![7],
            success: false,
        });
        dirty.push(LedgerRow {
            version: 5,
            checksum: vec![5],
            success: false,
        });
        assert!(
            matches!(
                pending_versions(&source, &dirty),
                Err(MigrateError::Dirty(5))
            ),
            "a dirty row is refused before any checksum, lowest version first"
        );
    }

    #[test]
    fn the_cutover_copies_only_the_flavors_versions_and_never_over_its_own_rows() {
        let row = |version, checksum: u8| LedgerRow {
            version,
            checksum: vec![checksum],
            success: true,
        };
        let rows = with_cutover_rows(
            vec![row(2, 20), row(4, 40)],
            vec![row(1, 11), row(2, 12), row(3, 13), row(9, 19)],
            &[1, 2, 4],
        );
        assert_eq!(
            rows,
            vec![row(1, 11), row(2, 20), row(4, 40)],
            "the own row of version 2 wins; 3 and 9 are not the flavor's"
        );
    }

    #[tokio::test]
    async fn a_plan_refuses_a_lineup_before_it_issues_a_query() {
        // Nothing listens here: a query would fail as `Connection` or
        // `LedgerRead`, not as the refusals below.
        let pool = sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(std::time::Duration::from_secs(2))
            .connect_lazy("postgres://nobody@127.0.0.1:1/none")
            .expect("a lazy pool parses its URL and connects later");

        let duplicate = pending_migrations(
            &pool,
            &[
                NamedMigrator::new("alpha", migrator(&[TEST_FLAVOR_VERSION])),
                NamedMigrator::new("beta", migrator(&[TEST_FLAVOR_VERSION])),
            ],
        )
        .await
        .expect_err("duplicate migration version");
        assert!(matches!(
            duplicate,
            MigrationError::DuplicateVersion {
                version: TEST_FLAVOR_VERSION,
                first_source: "alpha",
                second_source: "beta",
                ..
            }
        ));

        let shared = pending_migrations(
            &pool,
            &[
                NamedMigrator::flavor("a-b", migrator(&[TEST_FLAVOR_VERSION])),
                NamedMigrator::flavor("a_b", migrator(&[TEST_FLAVOR_VERSION + 1])),
            ],
        )
        .await
        .expect_err("two flavors on one ledger");
        assert!(matches!(shared, MigrationError::DuplicateLedger { .. }));

        let reachable = pending_migrations(&pool, &[]).await;
        assert!(
            matches!(reachable, Err(MigrationError::Connection(_))),
            "a valid lineup does reach for the database: {reachable:?}"
        );
    }
}

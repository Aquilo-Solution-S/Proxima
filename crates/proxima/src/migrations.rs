//! Shared migration facade for embedded Proxima hosts.
//!
//! Core records into `SQLx`'s default `public._sqlx_migrations`; each
//! flavor records into its own tracking table
//! (`public._sqlx_migrations_<flavor>` — in `public`, because destructive
//! flavor baselines drop the flavor schema and the ledger must survive
//! them), with a one-time cutover moving a pre-split database's flavor rows
//! out of the shared table. The facade pins the migration `search_path` to
//! `public`: core runs first, flavors run in composition order, and
//! duplicate versions fail before the database is touched.
//!
//! Every migrator keeps `SQLx`'s `ignore_missing`: a ledger row the binary
//! does not ship is normal when two releases' fleets share a database or a
//! lane shed a file. What is not normal is a binary whose lane does not
//! continue the one its ledger records; [`LedgerConflict`] names those
//! shapes, and each flavor ledger is checked against them before it runs.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use proxima_core::StorageError;
use proxima_storage_pg::{
    PgStorage, core_migrator, ensure_core_ledger_compatible, ensure_core_schema_current,
};
use sqlx::Connection;
use sqlx::PgConnection;
use sqlx::migrate::{Migrate, MigrateError, Migrator};

const CORE_SOURCE: &str = "proxima-core";

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
    table_name == "_sqlx_migrations" || table_name == "public._sqlx_migrations"
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
    let sources = prepare_sources(flavors)?;
    let report = MigrationRunReport::from_sources(&sources);
    let pool = pg.clone_pool_for_backend();
    ensure_core_ledger_compatible(&pool)
        .await
        .map_err(MigrationError::CorePreflight)?;
    let mut conn = pool.acquire().await.map_err(MigrationError::Connection)?;

    pin_migration_search_path(&mut conn)
        .await
        .map_err(MigrationError::PinSearchPath)?;

    let migration_result = run_sources_on_connection(&mut conn, sources).await;
    let reset_result = reset_migration_search_path(&mut conn).await;

    match (migration_result, reset_result) {
        (Ok(()), Ok(())) => {
            // The migration connection carried a disabled statement_timeout
            // Never return it to the pool with that override.
            conn.close_on_drop();
            Ok(report)
        }
        (Err(err), Ok(())) => {
            conn.close_on_drop();
            Err(err)
        }
        (Ok(()), Err(err)) => {
            conn.close_on_drop();
            Err(MigrationError::ResetSearchPath(err))
        }
        (Err(err), Err(reset_err)) => {
            conn.close_on_drop();
            tracing::warn!(
                error = %reset_err,
                "failed to reset migration search_path after migration error"
            );
            Err(err)
        }
    }
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
        let recorded = recorded_versions(&mut conn, source, true).await?;
        let lane = lane_versions(&source.migrator);
        if let Some(conflict) =
            ledger_conflict(&lane, &recorded).or_else(|| unapplied(&lane, &recorded))
        {
            return Err(source.ledger_error(conflict));
        }
    }
    Ok(report)
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
    // Disable it for this boot connection; the caller marks the connection
    // close-on-drop so the override never returns to the shared pool.
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
    sources: Vec<NamedMigrator>,
) -> Result<(), MigrationError> {
    for source in sources {
        if source.source == CORE_SOURCE {
            run_source_with_contention_retry(conn, &source).await?;
            prepare_ledger(conn, &source).await?;
        } else {
            prepare_ledger(conn, &source).await?;
            run_source_with_contention_retry(conn, &source).await?;
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
        if source.migrator.iter().any(|migration| migration.no_tx) {
            return Err(
                source.run_error(MigrateError::Execute(sqlx::Error::Protocol(
                    "platform-scoped migrations require transactional migration files".into(),
                ))),
            );
        }
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
            let recorded = recorded_versions(&mut transaction, source, false).await?;
            if let Some(conflict) = ledger_conflict(&lane_versions(&source.migrator), &recorded) {
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
    let versions: Vec<i64> = source
        .migrator()
        .iter()
        .map(|migration| migration.version)
        .collect();

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
}

/// The versions a migrator applies, ascending; down migrations excluded.
fn lane_versions(migrator: &Migrator) -> Vec<i64> {
    let versions: BTreeSet<i64> = migrator
        .iter()
        .filter(|migration| !migration.migration_type.is_down_migration())
        .map(|migration| migration.version)
        .collect();
    versions.into_iter().collect()
}

/// The versions `source`'s ledger records as applied, ascending. A ledger
/// that does not exist records nothing when `may_be_absent`; the migration
/// run creates every flavor ledger before reading it.
async fn recorded_versions(
    conn: &mut PgConnection,
    source: &NamedMigrator,
    may_be_absent: bool,
) -> Result<Vec<i64>, MigrationError> {
    let table = source.migrator.table_name.as_ref();
    // The migration run pins `search_path` to `public`, so an unqualified
    // ledger lives there; say so for the preflight, which pins nothing.
    let ledger = if table.contains('.') {
        table.to_owned()
    } else {
        format!("public.{table}")
    };
    let read_error = |err| MigrationError::LedgerRead {
        source: source.source,
        ledger: ledger.clone(),
        err,
    };
    if may_be_absent {
        let exists: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
            .bind(&ledger)
            .fetch_one(&mut *conn)
            .await
            .map_err(|err| read_error(MigrateError::Execute(err)))?;
        if !exists {
            return Ok(Vec::new());
        }
    }
    // SQLx's own ledger read, so the rows mean what its run means by them.
    let applied = conn
        .list_applied_migrations(&ledger)
        .await
        .map_err(read_error)?;
    Ok(applied.into_iter().map(|row| row.version).collect())
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

fn prepare_sources(
    flavors: impl IntoIterator<Item = NamedMigrator>,
) -> Result<Vec<NamedMigrator>, MigrationError> {
    let mut sources = Vec::new();
    sources.push(NamedMigrator::new(CORE_SOURCE, core_migrator()));

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

    reject_duplicate_versions(&sources)?;
    reject_shared_flavor_ledgers(&sources)?;
    Ok(sources)
}

fn reject_shared_flavor_ledgers(sources: &[NamedMigrator]) -> Result<(), MigrationError> {
    let mut seen: BTreeMap<String, &'static str> = BTreeMap::new();
    for source in sources.iter().skip(1) {
        let table = source.migrator.table_name.to_string();
        if is_core_ledger(&table) {
            continue;
        }
        if let Some(first_source) = seen.insert(table.clone(), source.source) {
            return Err(MigrationError::DuplicateLedger {
                table,
                first_source,
                second_source: source.source,
            });
        }
    }
    Ok(())
}

fn reject_duplicate_versions(sources: &[NamedMigrator]) -> Result<(), MigrationError> {
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
    use sqlx::migrate::{Migration, MigrationType, Migrator};

    use super::{
        LedgerConflict, MigrationError, NamedMigrator, flavor_ledger_table, ledger_conflict,
        prepare_sources, unapplied,
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
}

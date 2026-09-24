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

use std::collections::BTreeMap;
use std::time::Duration;

use proxima_core::StorageError;
use proxima_storage_pg::{
    PgStorage, core_migrator, ensure_core_ledger_compatible, ensure_core_schema_current,
};
use sqlx::Connection;
use sqlx::PgConnection;
use sqlx::migrate::{MigrateError, Migrator};

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
    #[error("flavor ledger cutover failed for {source}: {err}")]
    FlavorLedgerCutover {
        source: &'static str,
        #[source]
        err: sqlx::Error,
    },
}

/// Run core migrations followed by the provided flavor/host migrators.
///
/// # Errors
///
/// Returns `MigrationError::DuplicateVersion` before any database write
/// if two sources claim the same migration version. Returns `Connection`
/// or `PinSearchPath` if the pinned migration connection cannot be prepared.
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
/// pre-v0.0.4 artifacts, or `MigrationError::Connection` if the preflight
/// pool cannot be reached.
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
            run_source_with_contention_retry(conn, &source)
                .await
                .map_err(MigrationError::Core)?;
        } else {
            prepare_flavor_ledger(conn, &source).await?;
            run_source_with_contention_retry(conn, &source)
                .await
                .map_err(|err| MigrationError::Flavor {
                    source: source.source,
                    err,
                })?;
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
async fn run_source_with_contention_retry(
    conn: &mut PgConnection,
    source: &NamedMigrator,
) -> Result<(), MigrateError> {
    let mut attempt = 1;
    loop {
        if source.migrator.iter().any(|migration| migration.no_tx) {
            return Err(MigrateError::Execute(sqlx::Error::Protocol(
                "platform-scoped migrations require transactional migration files".into(),
            )));
        }
        let mut transaction = proxima_storage_pg::begin_migration_transaction(conn)
            .await
            .map_err(|error| MigrateError::Execute(sqlx::Error::Protocol(error.to_string())))?;
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
                    .map_err(MigrateError::Execute)?;
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
            result => return result,
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
/// run; every step is idempotent.
///
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
///   on the ledger is revoked; SELECT stays.
async fn prepare_flavor_ledger(
    conn: &mut PgConnection,
    source: &NamedMigrator,
) -> Result<(), MigrationError> {
    let table_name = source.migrator().table_name.clone();
    if is_core_ledger(&table_name) {
        return Ok(());
    }
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
    for schema in source.migrator().create_schemas.iter() {
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
    sqlx::query(PREPARE_FLAVOR_LEDGER)
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
    tx.commit().await.map_err(map_err)
}

/// [`prepare_flavor_ledger`]'s statement. The ledger name arrives through a
/// transaction-local setting, so the statement text is fixed; it is the
/// flavor migrator's compiled-in table name, interpolated as `SQLx` itself
/// interpolates it in `ensure_migrations_table`.
const PREPARE_FLAVOR_LEDGER: &str = r"DO $prepare_flavor_ledger$
DECLARE
    ledger text := current_setting('proxima.flavor_ledger');
    versions bigint[] := current_setting('proxima.flavor_ledger_versions')::bigint[];
    ledger_oid regclass;
    grantee oid;
BEGIN
    IF to_regclass('public._sqlx_migrations') IS NULL THEN
        RAISE EXCEPTION 'core ledger public._sqlx_migrations is missing; core migrates first';
    END IF;
    EXECUTE format('CREATE TABLE IF NOT EXISTS %s (LIKE public._sqlx_migrations INCLUDING ALL)', ledger);
    ledger_oid := ledger::regclass;
    EXECUTE format('INSERT INTO %s SELECT * FROM public._sqlx_migrations WHERE version = ANY($1) ON CONFLICT (version) DO NOTHING', ledger_oid)
        USING versions;
    FOR grantee IN
        SELECT DISTINCT acl.grantee
          FROM pg_class AS c, aclexplode(c.relacl) AS acl
         WHERE c.oid = ledger_oid AND acl.grantee <> c.relowner
           AND acl.privilege_type IN ('INSERT', 'UPDATE', 'DELETE', 'TRUNCATE')
    LOOP
        EXECUTE format('REVOKE INSERT, UPDATE, DELETE, TRUNCATE ON %s FROM %s', ledger_oid,
            CASE WHEN grantee = 0 THEN 'PUBLIC' ELSE quote_ident(pg_get_userbyid(grantee)) END);
    END LOOP;
END
$prepare_flavor_ledger$";

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

    use super::{MigrationError, NamedMigrator, flavor_ledger_table, prepare_sources};

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
    fn a_flavor_migrator_records_on_its_own_ledger() {
        assert_eq!(
            flavor_ledger_table("proxima-code"),
            "public._sqlx_migrations_proxima_code",
            "the derivation reproduces the ledger the code flavor already uses"
        );
        let mut shared = migrator(&[TEST_FLAVOR_VERSION]);
        shared.set_ignore_missing(false);
        let source = NamedMigrator::flavor("acme-forge_2", shared);
        assert_eq!(source.source(), "acme-forge_2");
        assert_eq!(
            source.migrator().table_name,
            "public._sqlx_migrations_acme_forge_2"
        );
        assert!(source.migrator().ignore_missing);
        assert_eq!(
            NamedMigrator::new("beta", migrator(&[TEST_FLAVOR_VERSION]))
                .migrator()
                .table_name,
            "_sqlx_migrations",
            "`new` keeps the migrator's own table, core's by default"
        );
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

    #[test]
    fn flavor_sources_are_forced_to_ignore_missing() {
        let sources = prepare_sources([NamedMigrator::new(
            "alpha",
            migrator(&[TEST_FLAVOR_VERSION]),
        )])
        .expect("valid sources");
        let alpha = sources
            .iter()
            .find(|source| source.source() == "alpha")
            .expect("alpha source");

        assert!(alpha.migrator().ignore_missing);
    }
}

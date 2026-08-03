//! Shared migration facade for embedded Proxima hosts.
//!
//! Core and flavor migrators share `SQLx`'s default `_sqlx_migrations`
//! table in v0.0.1. The facade pins that global namespace to `public`:
//! core runs first, flavors run in composition order, every migrator has
//! `ignore_missing(true)`, and duplicate versions fail before the
//! database is touched.

use std::collections::BTreeMap;

use proxima_core::StorageError;
use proxima_storage_pg::{
    PgStorage, core_migrator, ensure_core_schema_current, ensure_v004_baseline_compatible,
    ensure_v007_lane_squash_compatible,
};
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
    #[must_use]
    pub fn new(source: &'static str, migrator: Migrator) -> Self {
        Self { source, migrator }
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
    ensure_v004_baseline_compatible(&pool)
        .await
        .map_err(MigrationError::CorePreflight)?;
    ensure_v007_lane_squash_compatible(&pool)
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
/// This still rejects a stale pre-v0.0.4 database and still rejects duplicate
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
    ensure_v004_baseline_compatible(&pool)
        .await
        .map_err(MigrationError::CorePreflight)?;
    ensure_v007_lane_squash_compatible(&pool)
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
            source
                .migrator
                .run_direct(None, &mut *conn, false)
                .await
                .map_err(MigrationError::Core)?;
        } else {
            source
                .migrator
                .run_direct(None, &mut *conn, false)
                .await
                .map_err(|err| MigrationError::Flavor {
                    source: source.source,
                    err,
                })?;
        }
    }

    Ok(())
}

fn prepare_sources(
    flavors: impl IntoIterator<Item = NamedMigrator>,
) -> Result<Vec<NamedMigrator>, MigrationError> {
    let mut sources = Vec::new();
    sources.push(NamedMigrator::new(CORE_SOURCE, core_migrator()));

    for mut source in flavors {
        source.migrator.set_ignore_missing(true);
        sources.push(source);
    }

    reject_duplicate_versions(&sources)?;
    Ok(sources)
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

    use super::{MigrationError, NamedMigrator, prepare_sources};

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

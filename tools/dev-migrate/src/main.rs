//! Bootstrap a blank Postgres with the substrate schema and every in-repo
//! flavor sidecar, in the same order a composite host boots them.
//!
//! `sqlx migrate run` cannot do this: the substrate and flavor migrators
//! share one `_sqlx_migrations` table, so the CLI fails with
//! `VersionMissing` on the second source. This bin delegates to the same
//! framework facade used by embedded hosts: core first, then flavors in
//! composition order, with duplicate migration versions rejected up front.
//!
//! Usage:
//!
//! ```text
//! cargo run -p proxima-dev-migrate -- --database-url postgres://proxima:proxima@localhost/<db>
//! # or: DATABASE_URL=postgres://proxima:proxima@localhost/<db> cargo run -p proxima-dev-migrate
//! ```
//!
//! The target database URL always comes from `--database-url <URL>` when
//! given, falling back to `DATABASE_URL`; the resolved host/database is
//! printed before anything runs. Two repair modes exist beyond the plain
//! migration run:
//!
//! - `--reset`: destructive drop-and-recreate of the
//!   `proxima_core`/`proxima_code` schemas (see [`reset_local_dev_database`])
//!   — requires `PROXIMA_RESET_CONFIRM` and refuses non-local hosts and
//!   protected database names as a second, independent guard against pointing
//!   this at anything but a scratch dev database.
//! - `--stamp`: non-destructive ledger repair for a database that applied a
//!   draft lane later squashed under a fresh version number (see
//!   [`stamp_squashed_lane`] and docs/how-to/migrations.md). Refuses when the
//!   schema does not already match the current lane.

use proxima::flavor::FlavorBundle;
use proxima::run_core_and_flavor_migrations;
use proxima_storage_pg::{
    CORE_MIGRATION_VERSION_CEILING, PgStorage, core_migrator, ensure_core_schema_markers,
};

const DATABASE_URL_FLAG: &str = "--database-url";
const RESET_FLAG: &str = "--reset";
const STAMP_FLAG: &str = "--stamp";
// Keep the destructive-reset confirmation versioned so stale operator scripts
// do not silently opt in to a future baseline reset.
const RESET_CONFIRM_ENV: &str = "PROXIMA_RESET_CONFIRM";
const RESET_CONFIRM_ENV_LEGACY: &str = "PROXIMA_V004_RESET_CONFIRM";
const RESET_CONFIRM_VALUE: &str = "reset-my-dev-db";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let url = resolve_database_url(&args)?;
    print_target(&url)?;

    let pg = PgStorage::connect(&url).await?;
    if args.iter().any(|arg| arg == RESET_FLAG) {
        reset_local_dev_database(&pg, &url).await?;
    }
    if args.iter().any(|arg| arg == STAMP_FLAG) {
        stamp_squashed_lane(&pg).await?;
    }
    let report = run_core_and_flavor_migrations(&pg, proxima_code::CodeFlavor::migrators()).await?;
    for source in report.sources {
        println!("{source} migrations applied");
    }
    Ok(())
}

/// Resolve the target database URL: `--database-url <URL>` (or
/// `--database-url=<URL>`) first, then the `DATABASE_URL` env var.
fn resolve_database_url(args: &[String]) -> Result<String, Box<dyn std::error::Error>> {
    if let Some(pos) = args.iter().position(|arg| arg == DATABASE_URL_FLAG) {
        let value = args.get(pos + 1).ok_or(concat!(
            "--database-url requires a value, e.g. ",
            "--database-url postgres://proxima:proxima@localhost/proxima"
        ))?;
        return Ok(value.clone());
    }
    for arg in args {
        if let Some(value) = arg.strip_prefix("--database-url=") {
            return Ok(value.to_string());
        }
    }
    std::env::var("DATABASE_URL").map_err(|_| {
        "database URL required: pass --database-url <URL> or set DATABASE_URL, \
         e.g. postgres://proxima:proxima@localhost/proxima"
            .into()
    })
}

/// Print the resolved target host/database before any migration or
/// destructive operation runs, so a wrong `--database-url`/`DATABASE_URL`
/// is visible before it takes effect.
fn print_target(url: &str) -> Result<(), Box<dyn std::error::Error>> {
    let options: sqlx::postgres::PgConnectOptions = url.parse()?;
    eprintln!(
        "dev-migrate target: host={} database={}",
        options.get_host(),
        options.get_database().unwrap_or("<default>"),
    );
    Ok(())
}

async fn reset_local_dev_database(
    pg: &PgStorage,
    url: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let confirmed = [RESET_CONFIRM_ENV, RESET_CONFIRM_ENV_LEGACY]
        .iter()
        .any(|name| std::env::var(name).as_deref() == Ok(RESET_CONFIRM_VALUE));
    if !confirmed {
        return Err(format!(
            "set {RESET_CONFIRM_ENV}={RESET_CONFIRM_VALUE} to reset a local development database"
        )
        .into());
    }
    reset_local_dev_database_confirmed(pg, url).await
}

/// `sqlx` may expose an empty host when the connection options rely on a
/// default local Postgres host; this dev-only reset treats that as local.
const LOCAL_POSTGRES_EMPTY_HOST: &str = "";

fn is_local_postgres_host(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | LOCAL_POSTGRES_EMPTY_HOST) || host.starts_with('/')
}

async fn reset_local_dev_database_confirmed(
    pg: &PgStorage,
    url: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let options: sqlx::postgres::PgConnectOptions = url.parse()?;
    let host = options.get_host();
    let database = options.get_database().unwrap_or_default();
    if !is_local_postgres_host(host) {
        return Err("dev reset refuses non-local DATABASE_URL host".into());
    }
    if matches!(database, "postgres" | "template0" | "template1") {
        return Err("dev reset refuses protected database names".into());
    }
    let pool = pg.clone_pool_for_backend();
    let unexpected_schemas: Vec<String> = sqlx::query_scalar(
        "SELECT schema_name::text
           FROM information_schema.schemata
          WHERE schema_name LIKE 'proxima\\_%' ESCAPE '\\'
            AND schema_name NOT IN ('proxima_core', 'proxima_code')
          ORDER BY schema_name",
    )
    .fetch_all(&pool)
    .await?;
    if !unexpected_schemas.is_empty() {
        return Err(format!(
            "refusing reset because this binary does not own schemas: {}",
            unexpected_schemas.join(", ")
        )
        .into());
    }

    // Everything attributable to Proxima goes: the whole core version
    // namespace (which covers retired and draft rows without enumerating
    // them — see docs/how-to/migrations.md), plus every flavor version the
    // compiled flavors embed (covering rows a database from earlier in the v0.0.7 cycle still
    // tracks in the shared table), plus each compiled flavor's own tracking
    // table. A date-shaped row in the shared table that no compiled migrator
    // recognizes cannot be attributed, so it is left behind and reported —
    // it is inert under `ignore_missing`.
    let mut flavor_versions = Vec::new();
    let mut flavor_ledger_tables = Vec::new();
    for migrator in proxima_code::CodeFlavor::migrators() {
        flavor_versions.extend(
            migrator
                .migrator()
                .iter()
                .map(|migration| migration.version),
        );
        let table = migrator.migrator().table_name.clone();
        if table != "_sqlx_migrations" && table != "public._sqlx_migrations" {
            flavor_ledger_tables.push(table);
        }
    }

    eprintln!("resetting schemas: proxima_code, proxima_core");
    sqlx::query("DROP SCHEMA IF EXISTS proxima_code CASCADE")
        .execute(&pool)
        .await?;
    sqlx::query("DROP SCHEMA IF EXISTS proxima_core CASCADE")
        .execute(&pool)
        .await?;
    for table in flavor_ledger_tables {
        eprintln!("dropping flavor ledger table: {table}");
        sqlx::query(sqlx::AssertSqlSafe(format!("DROP TABLE IF EXISTS {table}")))
            .execute(&pool)
            .await?;
    }
    let migration_table_exists: bool =
        sqlx::query_scalar("SELECT to_regclass('public._sqlx_migrations') IS NOT NULL")
            .fetch_one(&pool)
            .await?;
    if migration_table_exists {
        let deleted: Vec<i64> = sqlx::query_scalar(
            "DELETE FROM public._sqlx_migrations
              WHERE version <= $1
                 OR version = ANY($2::bigint[])
              RETURNING version",
        )
        .bind(CORE_MIGRATION_VERSION_CEILING)
        .bind(&flavor_versions)
        .fetch_all(&pool)
        .await?;
        eprintln!("deleted migration versions: {deleted:?}");
        let leftover: Vec<i64> =
            sqlx::query_scalar("SELECT version FROM public._sqlx_migrations ORDER BY version")
                .fetch_all(&pool)
                .await?;
        if !leftover.is_empty() {
            eprintln!(
                "leaving {} ledger row(s) no compiled migrator accounts for (inert): {leftover:?}",
                leftover.len()
            );
        }
    }
    Ok(())
}

/// Repair a dev/staging database whose ledger recorded a draft lane that was
/// later squashed under a fresh version number (docs/how-to/migrations.md).
///
/// Stamping records migrations as applied **without executing them**, so it
/// is only honest when the schema already matches the current release lane;
/// this refuses unless the structural schema markers check out, and the
/// remedy for a partial draft lane is `--reset`. On success: deletes the
/// core-namespace ledger rows the embedded migrator cannot account for (the
/// orphaned drafts, or rows whose file was amended after application), then
/// records every pending migration as applied via `SQLx`'s skip machinery —
/// core against the shared table, each flavor against its own table when the
/// flavor's schema already exists.
async fn stamp_squashed_lane(pg: &PgStorage) -> Result<(), Box<dyn std::error::Error>> {
    let pool = pg.clone_pool_for_backend();
    if let Err(err) = ensure_core_schema_markers(&pool).await {
        return Err(format!(
            "refusing --stamp: {err}. Stamping records migrations as applied without running \
             them, so the schema must already match the current lane; for a partial draft \
             lane use --reset instead"
        )
        .into());
    }

    let migration_table_exists: bool =
        sqlx::query_scalar("SELECT to_regclass('public._sqlx_migrations') IS NOT NULL")
            .fetch_one(&pool)
            .await?;
    if migration_table_exists {
        let recorded: Vec<(i64, Vec<u8>)> = sqlx::query_as(
            "SELECT version, checksum
               FROM public._sqlx_migrations
              WHERE version <= $1
              ORDER BY version",
        )
        .bind(CORE_MIGRATION_VERSION_CEILING)
        .fetch_all(&pool)
        .await?;
        let orphaned: Vec<i64> = recorded
            .iter()
            .filter(|(version, checksum)| {
                !core_migrator().iter().any(|migration| {
                    migration.version == *version && migration.checksum.as_ref() == checksum
                })
            })
            .map(|(version, _)| *version)
            .collect();
        if !orphaned.is_empty() {
            eprintln!("deleting unaccounted core ledger rows: {orphaned:?}");
            sqlx::query("DELETE FROM public._sqlx_migrations WHERE version = ANY($1::bigint[])")
                .bind(&orphaned)
                .execute(&pool)
                .await?;
        }
    }

    core_migrator().skip(&pool, None).await?;
    println!("proxima-core ledger stamped to the embedded migration set");

    for migrator in proxima_code::CodeFlavor::migrators() {
        let ledger_exists: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
            .bind(migrator.migrator().table_name.as_ref())
            .fetch_one(&pool)
            .await?;
        // A flavor with no ledger table of its own either never ran here or
        // last ran pre-split; stamping it would tell SQLx its DDL already
        // ran and permanently skip it. Leave it pending — the normal
        // migration run afterwards cuts over and applies what is missing.
        if !ledger_exists {
            continue;
        }
        migrator.migrator().skip(&pool, None).await?;
        println!(
            "{} ledger stamped to the embedded migration set",
            migrator.source()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proxima_pg_testkit::{create_db, db_url, drop_db, unique_db_name};

    // A date-shaped ledger row no compiled migrator recognizes — e.g. a
    // flavor lane retired before the version-namespace rules existed. The
    // reset cannot attribute it, so it must survive as an inert leftover
    // rather than be deleted by guesswork.
    const UNATTRIBUTABLE_TIMESTAMP_MIGRATION_VERSION: i64 = 20_260_622_000_000;

    #[tokio::test]
    async fn reset_deletes_core_namespace_rows_and_reports_leftovers() {
        let db_name = unique_db_name("proxima_dev_migrate_reset");
        create_db(&db_name).await.expect("PG required for tests");
        let url = db_url(&db_name);

        let result: Result<(), Box<dyn std::error::Error>> = async {
            let pg = PgStorage::connect(&url).await?;
            sqlx::query("CREATE SCHEMA proxima_core")
                .execute(pg.pool_for_tests())
                .await?;
            sqlx::query("CREATE SCHEMA proxima_code")
                .execute(pg.pool_for_tests())
                .await?;
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
            .execute(pg.pool_for_tests())
            .await?;
            for version in [
                1_i64,
                2,
                3,
                4,
                5,
                6,
                7,
                // Orphaned draft rows from a squashed dev-cycle lane fall in
                // the core namespace and must go without being enumerated.
                12,
                13,
                14,
                15,
                UNATTRIBUTABLE_TIMESTAMP_MIGRATION_VERSION,
            ] {
                sqlx::query(
                    "INSERT INTO public._sqlx_migrations
                        (version, description, success, checksum, execution_time)
                     VALUES ($1, 'old', true, decode('00', 'hex'), 0)",
                )
                .bind(version)
                .execute(pg.pool_for_tests())
                .await?;
            }

            reset_local_dev_database_confirmed(&pg, &url).await?;

            let remaining: Vec<i64> =
                sqlx::query_scalar("SELECT version FROM public._sqlx_migrations ORDER BY version")
                    .fetch_all(pg.pool_for_tests())
                    .await?;
            assert_eq!(
                remaining,
                vec![UNATTRIBUTABLE_TIMESTAMP_MIGRATION_VERSION],
                "reset must delete every core-namespace row and keep only the \
                 unattributable leftover"
            );

            run_core_and_flavor_migrations(&pg, proxima_code::CodeFlavor::migrators()).await?;
            Ok(())
        }
        .await;

        let _ = drop_db(&db_name).await;
        result.expect("dev reset retired-row regression failed");
    }
}

//! Bootstrap a blank Postgres with the substrate schema and every in-repo
//! flavor sidecar, in the same order a composite host boots them.
//!
//! `sqlx migrate run` cannot do this: the substrate and flavor migrators
//! share one `_sqlx_migrations` table, so the CLI fails with
//! `VersionMissing` on the second source. This bin delegates to the same
//! framework facade used by embedded hosts: core first, then flavors in
//! composition order, with duplicate migration versions rejected up front.
//!
//! Usage (two steps — exporting `DATABASE_URL` at compile time would point
//! the workspace's `sqlx::query!` validation at the still-blank target DB):
//!
//! ```text
//! SQLX_OFFLINE=true cargo build -p proxima-dev-migrate
//! ./target/debug/dev-migrate --database-url postgres://proxima:proxima@localhost/<db>
//! # or: DATABASE_URL=postgres://proxima:proxima@localhost/<db> ./target/debug/dev-migrate
//! ```
//!
//! Afterwards `cargo sqlx prepare --workspace` has every schema it needs.
//!
//! The target database URL always comes from `--database-url <URL>` when
//! given, falling back to `DATABASE_URL`; the resolved host/database is
//! printed before anything runs. `--reset` additionally performs a
//! destructive drop-and-recreate of the `proxima_core`/`proxima_code`
//! schemas (see [`reset_local_dev_database`]) — it still requires
//! `PROXIMA_V004_RESET_CONFIRM` and refuses non-local hosts and protected
//! database names as a second, independent guard against pointing this at
//! anything but a scratch dev database.

use proxima::flavor::FlavorBundle;
use proxima::run_core_and_flavor_migrations;
use proxima_storage_pg::{
    PgStorage, RETIRED_PRE_V004_MIGRATION_VERSIONS as RETIRED_BASELINE_MIGRATION_VERSIONS,
    core_migrator,
};

const DATABASE_URL_FLAG: &str = "--database-url";
const RESET_FLAG: &str = "--reset";
// Keep the destructive-reset confirmation versioned so stale operator scripts
// do not silently opt in to a future baseline reset.
const RESET_CONFIRM_ENV: &str = "PROXIMA_V004_RESET_CONFIRM";
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
    if std::env::var(RESET_CONFIRM_ENV).as_deref() != Ok(RESET_CONFIRM_VALUE) {
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

    let mut versions = core_migrator()
        .iter()
        .map(|migration| migration.version)
        .collect::<Vec<_>>();
    for migrator in proxima_code::CodeFlavor::migrators() {
        versions.extend(
            migrator
                .migrator()
                .iter()
                .map(|migration| migration.version),
        );
    }
    versions.extend_from_slice(RETIRED_BASELINE_MIGRATION_VERSIONS);
    versions.sort_unstable();
    versions.dedup();

    eprintln!("resetting schemas: proxima_code, proxima_core");
    eprintln!("deleting migration versions: {versions:?}");
    sqlx::query("DROP SCHEMA IF EXISTS proxima_code CASCADE")
        .execute(&pool)
        .await?;
    sqlx::query("DROP SCHEMA IF EXISTS proxima_core CASCADE")
        .execute(&pool)
        .await?;
    let migration_table_exists: bool =
        sqlx::query_scalar("SELECT to_regclass('public._sqlx_migrations') IS NOT NULL")
            .fetch_one(&pool)
            .await?;
    if migration_table_exists {
        sqlx::query(
            "DELETE FROM public._sqlx_migrations
              WHERE version = ANY($1::bigint[])",
        )
        .bind(&versions)
        .execute(&pool)
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proxima_pg_testkit::{create_db, db_url, drop_db, unique_db_name};

    // Representative retired timestamp-style SQLx migration version folded
    // into the destructive baseline.
    const LEGACY_TIMESTAMP_STYLE_MIGRATION_VERSION: i64 = 20_260_622_000_000;

    #[tokio::test]
    async fn reset_deletes_retired_baseline_migration_rows() {
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
                LEGACY_TIMESTAMP_STYLE_MIGRATION_VERSION,
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
            assert!(
                remaining.is_empty(),
                "retired rows survived reset: {remaining:?}"
            );

            run_core_and_flavor_migrations(&pg, proxima_code::CodeFlavor::migrators()).await?;
            Ok(())
        }
        .await;

        let _ = drop_db(&db_name).await;
        result.expect("dev reset retired-row regression failed");
    }
}

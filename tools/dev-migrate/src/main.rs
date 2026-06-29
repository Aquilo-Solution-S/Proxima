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
//! DATABASE_URL=postgres://proxima:proxima@localhost/<db> ./target/debug/dev-migrate
//! ```
//!
//! Afterwards `cargo sqlx prepare --workspace` has every schema it needs.

use proxima::{FlavorBundle, run_core_and_flavor_migrations};
use proxima_storage_pg::{PgStorage, RETIRED_PRE_V004_MIGRATION_VERSIONS, core_migrator};

const RESET_FLAG: &str = "--v004-reset-dev-db";
const RESET_CONFIRM_ENV: &str = "PROXIMA_V004_RESET_CONFIRM";
const RESET_CONFIRM_VALUE: &str = "reset-my-dev-db";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::var("DATABASE_URL").map_err(
        |_| "DATABASE_URL must be set, e.g. postgres://proxima:proxima@localhost/proxima",
    )?;
    let pg = PgStorage::connect(&url).await?;
    if std::env::args().skip(1).any(|arg| arg == RESET_FLAG) {
        reset_local_dev_database(&pg, &url).await?;
    }
    let report = run_core_and_flavor_migrations(&pg, proxima_code::CodeFlavor::migrators()).await?;
    for source in report.sources {
        println!("{source} migrations applied");
    }
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

async fn reset_local_dev_database_confirmed(
    pg: &PgStorage,
    url: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let options: sqlx::postgres::PgConnectOptions = url.parse()?;
    let host = options.get_host();
    let database = options.get_database().unwrap_or_default();
    if !matches!(host, "localhost" | "127.0.0.1" | "") && !host.starts_with('/') {
        return Err("v0.0.4 dev reset refuses non-local DATABASE_URL host".into());
    }
    if matches!(database, "postgres" | "template0" | "template1") {
        return Err("v0.0.4 dev reset refuses protected database names".into());
    }
    let unexpected_schemas: Vec<String> = sqlx::query_scalar(
        "SELECT schema_name::text
           FROM information_schema.schemata
          WHERE schema_name LIKE 'proxima\\_%' ESCAPE '\\'
            AND schema_name NOT IN ('proxima_core', 'proxima_code')
          ORDER BY schema_name",
    )
    .fetch_all(pg.pool())
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
    versions.extend_from_slice(RETIRED_PRE_V004_MIGRATION_VERSIONS);
    versions.sort_unstable();
    versions.dedup();

    eprintln!("resetting schemas: proxima_code, proxima_core");
    eprintln!("deleting migration versions: {versions:?}");
    sqlx::query("DROP SCHEMA IF EXISTS proxima_code CASCADE")
        .execute(pg.pool())
        .await?;
    sqlx::query("DROP SCHEMA IF EXISTS proxima_core CASCADE")
        .execute(pg.pool())
        .await?;
    let migration_table_exists: bool =
        sqlx::query_scalar("SELECT to_regclass('public._sqlx_migrations') IS NOT NULL")
            .fetch_one(pg.pool())
            .await?;
    if migration_table_exists {
        sqlx::query(
            "DELETE FROM public._sqlx_migrations
              WHERE version = ANY($1::bigint[])",
        )
        .bind(&versions)
        .execute(pg.pool())
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proxima_pg_testkit::{create_db, db_url, drop_db, unique_db_name};

    #[tokio::test]
    async fn reset_deletes_retired_pre_v004_migration_rows() {
        let db_name = unique_db_name("proxima_dev_migrate_reset");
        create_db(&db_name).await.expect("PG required for tests");
        let url = db_url(&db_name);

        let result: Result<(), Box<dyn std::error::Error>> = async {
            let pg = PgStorage::connect(&url).await?;
            sqlx::query("CREATE SCHEMA proxima_core")
                .execute(pg.pool())
                .await?;
            sqlx::query("CREATE SCHEMA proxima_code")
                .execute(pg.pool())
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
            .execute(pg.pool())
            .await?;
            for version in [1_i64, 2, 3, 4, 5, 6, 7, 20_260_622_000_000] {
                sqlx::query(
                    "INSERT INTO public._sqlx_migrations
                        (version, description, success, checksum, execution_time)
                     VALUES ($1, 'old', true, decode('00', 'hex'), 0)",
                )
                .bind(version)
                .execute(pg.pool())
                .await?;
            }

            reset_local_dev_database_confirmed(&pg, &url).await?;

            let remaining: Vec<i64> =
                sqlx::query_scalar("SELECT version FROM public._sqlx_migrations ORDER BY version")
                    .fetch_all(pg.pool())
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

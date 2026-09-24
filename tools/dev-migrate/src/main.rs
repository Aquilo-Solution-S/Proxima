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
//! - `--reset`: destructive drop-and-recreate of every `proxima_*` schema
//!   (see [`reset_local_dev_database`]) — the same namespace
//!   `ensure_core_ledger_compatible` calls Proxima's, so a schema left by a
//!   flavor no longer compiled in cannot make boot demand a reset the reset
//!   cannot deliver. Requires `PROXIMA_RESET_CONFIRM` and refuses non-local
//!   hosts and protected database names as a second, independent guard
//!   against pointing this at anything but a scratch dev database.
//! - `--stamp`: non-destructive ledger repair for a database that applied a
//!   draft lane later squashed under a fresh version number (see
//!   [`stamp_squashed_lane`] and docs/how-to/migrations.md). Refuses unless
//!   the live catalog equals what the embedded migrations create — proven by
//!   replaying them in a rolled-back transaction ([`catalog_proof`]).

mod catalog_proof;

use proxima::flavor::FlavorBundle;
use proxima::{NamedMigrator, run_core_and_flavor_migrations};
use proxima_storage_pg::{
    CORE_MIGRATION_VERSION_CEILING, PgPlatformScope, PgPoolConfig, PgStorage, PgTuning,
    core_migrator, ensure_core_schema_markers,
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

    let pg = PgStorage::connect_for_migrations_with_config(
        &url,
        PgPoolConfig::from_env()?,
        PgTuning::from_env()?,
    )
    .await?;
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
    // A `proxima_*` schema this binary's compiled flavors did not create
    // used to REFUSE the reset. That was the right guard while the reset
    // dropped a hardcoded two names — it said "I will not destroy what I
    // cannot recreate" — but it is now a deadlock, because
    // `ensure_core_ledger_compatible` treats EVERY `proxima\_%` schema as
    // Proxima's when the baseline marker is missing. Boot demanded a reset
    // that the reset then refused, with nothing in between to break the
    // tie. The two predicates have to agree on what the namespace is, so
    // the guard becomes a report: the schema goes with the rest, and the
    // operator sees it named before it does.
    let unowned_schemas: Vec<String> = sqlx::query_scalar(
        "SELECT schema_name::text
           FROM information_schema.schemata
          WHERE schema_name LIKE 'proxima\\_%' ESCAPE '\\'
            AND schema_name NOT IN ('proxima_core', 'proxima_code')
          ORDER BY schema_name",
    )
    .fetch_all(&pool)
    .await?;
    if !unowned_schemas.is_empty() {
        eprintln!(
            "dropping schemas no compiled flavor of this binary owns: {}",
            unowned_schemas.join(", ")
        );
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

    // Every `proxima_*` schema, not the two this binary happens to know
    // about. `ensure_core_ledger_compatible` decides a database needs a
    // reset by probing `table_schema LIKE 'proxima\_%'`, so a schema left
    // by a flavor that is no longer compiled in makes boot demand a reset
    // that a two-name DROP list can never satisfy — the reset "succeeds",
    // the schema stays, and boot demands the reset again, forever. The two
    // predicates have to be the same predicate.
    let schemas: Vec<String> = sqlx::query_scalar(
        "SELECT nspname::text FROM pg_namespace
          WHERE nspname LIKE 'proxima\\_%'
          ORDER BY nspname",
    )
    .fetch_all(&pool)
    .await?;
    eprintln!("resetting schemas: {}", schemas.join(", "));
    // The loop is inside the database rather than in Rust so the schema
    // name never becomes a Rust format argument: `format('%I')` is
    // PostgreSQL's own identifier quoting, and this stays one fixed string
    // literal instead of a dynamic SQL site needing a proof.
    sqlx::raw_sql(
        "DO $$
         DECLARE schema_name text;
         BEGIN
             FOR schema_name IN
                 SELECT nspname FROM pg_namespace WHERE nspname LIKE 'proxima\\_%'
             LOOP
                 EXECUTE format('DROP SCHEMA IF EXISTS %I CASCADE', schema_name);
             END LOOP;
         END $$;",
    )
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
/// is only honest when the schema already matches the current release lane.
/// This refuses unless the structural schema markers and the owner-RLS census
/// check out **and** the live catalog equals what the embedded migrations
/// create ([`catalog_proof`]); the remedy for a partial draft lane is
/// `--reset`. A marker check alone would stamp a database whose migration
/// was amended after it applied it — the amended checksum recorded over the
/// old routine bodies. On success: deletes the core-namespace ledger rows the
/// embedded migrator cannot account for (the orphaned drafts, or rows whose
/// file was amended after application), then records every pending migration
/// as applied via `SQLx`'s skip machinery — core against the shared table,
/// each flavor against its own table when that table already exists.
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

    // Stamping must never turn a pre-RLS schema into a falsely complete lane:
    // the subsequent migration runner would see the ledger row and skip the
    // owner-RLS activation.  Require the actual epoch marker and validate the
    // migration-purpose role against every composed schema that already exists
    // before touching either ledger.
    let epoch_exists: bool =
        sqlx::query_scalar("SELECT to_regclass('proxima_core.owner_rls_epoch') IS NOT NULL")
            .fetch_one(&pool)
            .await?;
    if !epoch_exists {
        return Err(
            "refusing --stamp: owner-RLS activation epoch is absent; run the pending activation migration"
                .into(),
        );
    }
    let existing_schemas: Vec<String> = sqlx::query_scalar(
        "SELECT schema_name::text
           FROM information_schema.schemata
          WHERE schema_name IN ('proxima_core', 'proxima_code')
          ORDER BY schema_name",
    )
    .fetch_all(&pool)
    .await?;
    let schema_refs: Vec<&str> = existing_schemas.iter().map(String::as_str).collect();
    PgPlatformScope::new(pool.clone(), &schema_refs)
        .await
        .map_err(|error| {
            format!(
                "refusing --stamp: migration-purpose platform role or owner-RLS policy census is invalid: {error}"
            )
        })?;

    // A flavor with no ledger table of its own either never ran here or last
    // ran pre-split; stamping it would tell SQLx its DDL already ran and
    // permanently skip it. Leave it pending — the normal migration run
    // afterwards cuts over and applies what is missing. The catalog proof
    // replays exactly the lanes this stamp records.
    let mut stamped_flavors: Vec<NamedMigrator> = Vec::new();
    for migrator in proxima_code::CodeFlavor::migrators() {
        let ledger_exists: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
            .bind(migrator.migrator().table_name.as_ref())
            .fetch_one(&pool)
            .await?;
        if ledger_exists {
            stamped_flavors.push(migrator);
        }
    }
    let core = core_migrator();
    let lanes: Vec<&sqlx::migrate::Migrator> = std::iter::once(&core)
        .chain(stamped_flavors.iter().map(NamedMigrator::migrator))
        .collect();
    catalog_proof::refuse_unless_live_catalog_matches(&pool, &lanes).await?;

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
                !core.iter().any(|migration| {
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

    core.skip(&pool, None).await?;
    println!("proxima-core ledger stamped to the embedded migration set");

    for migrator in stamped_flavors {
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
    use proxima_pg_testkit::{create_db, db_url, drop_db, split_role_urls, unique_db_name};

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
        let (_, platform_url) = split_role_urls(&db_name).await.expect("split role URLs");

        let result: Result<(), Box<dyn std::error::Error>> = async {
            let pg = PgStorage::connect_for_migrations_with_config(
                &platform_url,
                PgPoolConfig::from_env()?,
                PgTuning::from_env()?,
            )
            .await?;
            sqlx::query("CREATE SCHEMA proxima_core")
                .execute(pg.pool_for_tests())
                .await?;
            sqlx::query("CREATE SCHEMA proxima_code")
                .execute(pg.pool_for_tests())
                .await?;
            sqlx::query(
                "CREATE TABLE IF NOT EXISTS public._sqlx_migrations (
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

    #[tokio::test]
    async fn stamp_requires_owner_rls_epoch_and_preserves_ledger_on_refusal() {
        let db_name = unique_db_name("proxima_dev_migrate_stamp");
        create_db(&db_name).await.expect("PG required for tests");
        let (_, platform_url) = split_role_urls(&db_name).await.expect("split role URLs");
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let pg = PgStorage::connect_for_migrations_with_config(
                &platform_url,
                PgPoolConfig::from_env()?,
                PgTuning::from_env()?,
            )
            .await?;
            proxima_storage_pg::test_fixtures::core_migrator_before_owner_rls()
                .run(pg.pool_for_tests())
                .await?;

            let ledger = || async {
                sqlx::query_as::<_, (i64, Vec<u8>)>(
                    "SELECT version, checksum FROM public._sqlx_migrations ORDER BY version",
                )
                .fetch_all(pg.pool_for_tests())
                .await
            };
            let before = ledger().await?;
            let refusal = stamp_squashed_lane(&pg)
                .await
                .expect_err("pre-owner-RLS schema must not be stampable")
                .to_string();
            assert!(
                refusal.contains("owner-RLS activation epoch is absent"),
                "{refusal}"
            );
            assert_eq!(
                ledger().await?,
                before,
                "refusal must leave the ledger unchanged"
            );

            run_core_and_flavor_migrations(&pg, proxima_code::CodeFlavor::migrators()).await?;
            stamp_squashed_lane(&pg).await?;

            let admin = sqlx::PgPool::connect(&db_url(&db_name)).await?;
            sqlx::query("ALTER TABLE proxima_core.memory NO FORCE ROW LEVEL SECURITY")
                .execute(&admin)
                .await?;
            let before_policy_refusal = ledger().await?;
            let refusal = stamp_squashed_lane(&pg)
                .await
                .expect_err("missing FORCE RLS must refuse stamping")
                .to_string();
            assert!(
                refusal.contains("platform role or owner-RLS policy census"),
                "{refusal}"
            );
            assert_eq!(
                ledger().await?,
                before_policy_refusal,
                "policy refusal must leave the ledger unchanged"
            );
            Ok(())
        }
        .await;
        let _ = drop_db(&db_name).await;
        result.expect("dev stamp owner-RLS regression failed");
    }

    /// The checksum a database records when it applied bytes the binary no
    /// longer embeds: here, version 14 as if the v0.0.15 file were applied.
    const AMENDED_CHECKSUM_HEX: &str = "ab";
    const AMENDED_VERSION: i64 = 14;

    type Ledger = Vec<(i64, Vec<u8>)>;

    async fn read_ledger(pool: &sqlx::PgPool) -> Result<Ledger, sqlx::Error> {
        sqlx::query_as(
            "SELECT version, checksum FROM public._sqlx_migrations
              UNION ALL
             SELECT version, checksum FROM public._sqlx_migrations_proxima_code
              ORDER BY 1",
        )
        .fetch_all(pool)
        .await
    }

    /// A fully migrated split-role database whose ledger records version 14
    /// with a checksum the embedded file does not have — the state d12da4f2
    /// left every database that applied the v0.0.15 bytes of 0014 in.
    async fn amended_database(
        db_name: &str,
    ) -> Result<(PgStorage, sqlx::PgPool), Box<dyn std::error::Error>> {
        create_db(db_name).await?;
        let (_, platform_url) = split_role_urls(db_name).await?;
        let pg = PgStorage::connect_for_migrations_with_config(
            &platform_url,
            PgPoolConfig::from_env()?,
            PgTuning::from_env()?,
        )
        .await?;
        run_core_and_flavor_migrations(&pg, proxima_code::CodeFlavor::migrators()).await?;
        let admin = sqlx::PgPool::connect(&db_url(db_name)).await?;
        sqlx::query(
            "UPDATE public._sqlx_migrations
                SET checksum = decode(repeat($1, 48), 'hex')
              WHERE version = $2",
        )
        .bind(AMENDED_CHECKSUM_HEX)
        .bind(AMENDED_VERSION)
        .execute(&admin)
        .await?;
        Ok((pg, admin))
    }

    /// Stamp must refuse, name the drifted object, and leave both the ledger
    /// and the catalog exactly as it found them.
    async fn assert_stamp_refuses_drift(
        pg: &PgStorage,
        admin: &sqlx::PgPool,
        drifted: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let schemas = || async {
            sqlx::query_scalar::<_, String>(
                "SELECT nspname::text FROM pg_namespace
                  WHERE nspname LIKE 'proxima\\_%' OR nspname LIKE 'stamp\\_live\\_%'
                  ORDER BY 1",
            )
            .fetch_all(admin)
            .await
        };
        let ledger_before = read_ledger(admin).await?;
        let schemas_before = schemas().await?;
        let refusal = stamp_squashed_lane(pg)
            .await
            .expect_err("a catalog that differs from the embedded lane must not be stampable")
            .to_string();
        assert!(
            refusal.contains("the live catalog differs from what the embedded migrations create")
                && refusal.contains(drifted),
            "{refusal}"
        );
        assert_eq!(
            read_ledger(admin).await?,
            ledger_before,
            "refusal must leave the ledger unchanged"
        );
        assert_eq!(
            schemas().await?,
            schemas_before,
            "the proof's renames and replay must roll back"
        );
        Ok(())
    }

    #[tokio::test]
    async fn stamp_refuses_an_amended_migration_whose_bodies_differ_from_the_live_catalog() {
        let db_name = unique_db_name("proxima_dev_migrate_stamp_drift");
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let (pg, admin) = amended_database(&db_name).await?;

            // A routine 0014 defines keeps the body an earlier file gave it.
            let definition: String = sqlx::query_scalar(
                "SELECT pg_get_functiondef(
                    'proxima_core.record_erased_pin_target(uuid, proxima_core.pin_target_kind)'
                        ::regprocedure)",
            )
            .fetch_one(&admin)
            .await?;
            let drifted = definition.replacen(
                "AS $function$",
                "AS $function$\n-- the body the v0.0.15 file shipped\n",
                1,
            );
            assert_ne!(drifted, definition, "the drift must change the body");
            sqlx::raw_sql(sqlx::AssertSqlSafe(drifted))
                .execute(&admin)
                .await?;
            assert_stamp_refuses_drift(&pg, &admin, "record_erased_pin_target").await?;
            sqlx::raw_sql(sqlx::AssertSqlSafe(definition))
                .execute(&admin)
                .await?;

            // A policy the migration defines, with a different predicate.
            let (using, check): (String, String) = sqlx::query_as(
                "SELECT pg_get_expr(polqual, polrelid), pg_get_expr(polwithcheck, polrelid)
                   FROM pg_policy
                  WHERE polrelid = 'proxima_core.memory'::regclass
                    AND polname = 'proxima_owner_write'",
            )
            .fetch_one(&admin)
            .await?;
            sqlx::query(
                "ALTER POLICY proxima_owner_write ON proxima_core.memory
                  USING (false) WITH CHECK (false)",
            )
            .execute(&admin)
            .await?;
            assert_stamp_refuses_drift(&pg, &admin, "memory.proxima_owner_write").await?;
            sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
                "ALTER POLICY proxima_owner_write ON proxima_core.memory
                  USING ({using}) WITH CHECK ({check})"
            )))
            .execute(&admin)
            .await?;

            // A trigger the migration defines, disabled.
            let trigger: String = sqlx::query_scalar(
                "SELECT tgname::text FROM pg_trigger
                  WHERE tgrelid = 'proxima_core.memory'::regclass AND NOT tgisinternal
                  ORDER BY tgname LIMIT 1",
            )
            .fetch_one(&admin)
            .await?;
            sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
                "ALTER TABLE proxima_core.memory DISABLE TRIGGER {trigger}"
            )))
            .execute(&admin)
            .await?;
            assert_stamp_refuses_drift(&pg, &admin, &format!("memory.{trigger}")).await?;
            Ok(())
        }
        .await;
        let _ = drop_db(&db_name).await;
        result.expect("stamp over drifted bodies must refuse");
    }

    #[tokio::test]
    async fn stamp_records_an_amended_migration_whose_bodies_match_the_live_catalog() {
        let db_name = unique_db_name("proxima_dev_migrate_stamp_same");
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let (pg, admin) = amended_database(&db_name).await?;
            // The amendment changed bytes, not objects (a comment, say): boot
            // refuses the checksum, and the stamp may record the new one.
            let refusal =
                run_core_and_flavor_migrations(&pg, proxima_code::CodeFlavor::migrators())
                    .await
                    .expect_err("an amended checksum must stop boot")
                    .to_string();
            assert!(refusal.contains("were amended"), "{refusal}");

            stamp_squashed_lane(&pg).await?;

            let embedded = core_migrator()
                .iter()
                .find(|migration| migration.version == AMENDED_VERSION)
                .map(|migration| migration.checksum.to_vec())
                .expect("version 14 is embedded");
            let recorded: Vec<u8> = sqlx::query_scalar(
                "SELECT checksum FROM public._sqlx_migrations WHERE version = $1",
            )
            .bind(AMENDED_VERSION)
            .fetch_one(&admin)
            .await?;
            assert_eq!(recorded, embedded, "stamp records the embedded checksum");
            run_core_and_flavor_migrations(&pg, proxima_code::CodeFlavor::migrators()).await?;
            Ok(())
        }
        .await;
        let _ = drop_db(&db_name).await;
        result.expect("stamp over identical bodies must succeed");
    }
}

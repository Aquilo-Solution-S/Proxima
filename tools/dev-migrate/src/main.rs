//! Bootstrap a blank Postgres with the substrate schema and every in-repo
//! flavor sidecar, in the same order a composite host boots them.
//!
//! `sqlx migrate run` cannot do this: it records every source into one
//! `_sqlx_migrations` table, while core and each flavor keep their own ledger
//! (`public._sqlx_migrations_<flavor>` for a flavor). This bin delegates to the same
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
//!   against pointing this at anything but a scratch dev database. Refuses,
//!   listing them, when objects outside those schemas depend on them (a
//!   consumer's FK, view, trigger or policy the `CASCADE` would drop while
//!   the consumer's ledger rows still claim it); `--reset-dependent-lanes`
//!   accepts the loss and also deletes the shared-ledger rows no Proxima lane
//!   owns, so the consumer's lane re-runs instead of trusting stale rows.

use proxima::flavor::FlavorBundle;
use proxima::run_core_and_flavor_migrations;
use proxima_storage_pg::{
    CORE_MIGRATION_VERSION_CEILING, PgMigrationStorage, PgPoolConfig, PgTuning,
};

const DATABASE_URL_FLAG: &str = "--database-url";
const RESET_FLAG: &str = "--reset";
const RESET_DEPENDENT_LANES_FLAG: &str = "--reset-dependent-lanes";
// Keep the destructive-reset confirmation versioned so stale operator scripts
// do not silently opt in to a future baseline reset.
const RESET_CONFIRM_ENV: &str = "PROXIMA_RESET_CONFIRM";
const RESET_CONFIRM_VALUE: &str = "reset-my-dev-db";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let url = resolve_database_url(&args)?;
    print_target(&url)?;

    let pg = proxima_storage_pg::PgMigrationStorage::connect_with_config(
        &url,
        PgPoolConfig::from_env()?,
        PgTuning::from_env()?,
    )
    .await?;
    let dependent_lanes = if args.iter().any(|arg| arg == RESET_DEPENDENT_LANES_FLAG) {
        DependentLanes::Reset
    } else {
        DependentLanes::Refuse
    };
    if args.iter().any(|arg| arg == RESET_FLAG) {
        reset_local_dev_database(&pg, &url, dependent_lanes).await?;
    } else if dependent_lanes == DependentLanes::Reset {
        return Err(format!("{RESET_DEPENDENT_LANES_FLAG} only qualifies {RESET_FLAG}").into());
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

/// What `--reset` does with objects outside `proxima_*` that depend on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DependentLanes {
    /// Refuse and list them: nothing is dropped, no ledger row is touched.
    Refuse,
    /// Drop them with Proxima's schemas and delete the shared-ledger rows no
    /// Proxima lane owns (`--reset-dependent-lanes`).
    Reset,
}

/// Objects of other lanes that `DROP SCHEMA .. CASCADE` would take with the
/// `proxima_*` schemas, as `(object, what it depends on)`. Direct edges only:
/// whatever depends on a listed object goes with it too.
///
/// Two directions, both from `pg_depend`:
///
/// - an object outside depends on one inside — a consumer's FK to
///   `proxima_core.memory`, a view over it, a trigger or policy calling a
///   Proxima routine, a column of a Proxima type;
/// - an object attached to a Proxima table depends on a user object outside
///   — a consumer's trigger or policy on `proxima_core.memory` whose routine
///   lives in the consumer's schema. Extension members and system objects
///   are what Proxima's own lane references, so they do not count.
///
/// Internal (`i`) and extension (`e`, `x`) edges are skipped: a toast table
/// in `pg_toast` is part of its table, not a dependent lane. Not detectable:
/// a consumer object on a Proxima table that references only Proxima or
/// built-in objects — the catalog cannot tell it from Proxima's own.
const FOREIGN_DEPENDENTS: &str = r"
WITH proxima AS (
    SELECT oid FROM pg_catalog.pg_namespace WHERE nspname LIKE 'proxima\_%' ESCAPE '\'
), holder (classid, objid, nsp) AS (
    SELECT 'pg_catalog.pg_class'::regclass::oid, c.oid, c.relnamespace FROM pg_catalog.pg_class c
    UNION ALL SELECT 'pg_catalog.pg_proc'::regclass::oid, p.oid, p.pronamespace FROM pg_catalog.pg_proc p
    UNION ALL SELECT 'pg_catalog.pg_type'::regclass::oid, t.oid, t.typnamespace FROM pg_catalog.pg_type t
    UNION ALL SELECT 'pg_catalog.pg_constraint'::regclass::oid, k.oid, k.connamespace
                FROM pg_catalog.pg_constraint k
    UNION ALL SELECT 'pg_catalog.pg_operator'::regclass::oid, o.oid, o.oprnamespace
                FROM pg_catalog.pg_operator o
    UNION ALL SELECT 'pg_catalog.pg_collation'::regclass::oid, l.oid, l.collnamespace
                FROM pg_catalog.pg_collation l
    UNION ALL SELECT 'pg_catalog.pg_statistic_ext'::regclass::oid, x.oid, x.stxnamespace
                FROM pg_catalog.pg_statistic_ext x
    UNION ALL SELECT 'pg_catalog.pg_namespace'::regclass::oid, n.oid, n.oid FROM pg_catalog.pg_namespace n
    UNION ALL SELECT 'pg_catalog.pg_trigger'::regclass::oid, g.oid, c.relnamespace
                FROM pg_catalog.pg_trigger g JOIN pg_catalog.pg_class c ON c.oid = g.tgrelid
    UNION ALL SELECT 'pg_catalog.pg_policy'::regclass::oid, y.oid, c.relnamespace
                FROM pg_catalog.pg_policy y JOIN pg_catalog.pg_class c ON c.oid = y.polrelid
    UNION ALL SELECT 'pg_catalog.pg_rewrite'::regclass::oid, r.oid, c.relnamespace
                FROM pg_catalog.pg_rewrite r JOIN pg_catalog.pg_class c ON c.oid = r.ev_class
    UNION ALL SELECT 'pg_catalog.pg_attrdef'::regclass::oid, a.oid, c.relnamespace
                FROM pg_catalog.pg_attrdef a JOIN pg_catalog.pg_class c ON c.oid = a.adrelid
    UNION ALL SELECT 'pg_catalog.pg_default_acl'::regclass::oid, f.oid, f.defaclnamespace
                FROM pg_catalog.pg_default_acl f
), edge AS (
    SELECT d.classid, d.objid, d.objsubid, d.refclassid, d.refobjid, d.refobjsubid, d.deptype,
           COALESCE(dh.nsp IN (SELECT oid FROM proxima), false) AS dependent_inside,
           COALESCE(rh.nsp IN (SELECT oid FROM proxima), false) AS referenced_inside
      FROM pg_catalog.pg_depend d
      LEFT JOIN holder dh ON dh.classid = d.classid AND dh.objid = d.objid
      LEFT JOIN holder rh ON rh.classid = d.refclassid AND rh.objid = d.refobjid
     WHERE d.deptype NOT IN ('i', 'e', 'x')
)
SELECT DISTINCT
       pg_catalog.pg_describe_object(e.classid, e.objid, e.objsubid),
       pg_catalog.pg_describe_object(e.refclassid, e.refobjid, e.refobjsubid)
  FROM edge e
 WHERE (e.referenced_inside AND NOT e.dependent_inside)
    OR (e.dependent_inside AND NOT e.referenced_inside
        AND e.deptype = 'n'
        AND e.refobjid >= 16384
        AND e.refclassid <> 'pg_catalog.pg_namespace'::regclass
        AND NOT EXISTS (
            SELECT 1 FROM pg_catalog.pg_depend m
             WHERE m.classid = e.refclassid AND m.objid = e.refobjid AND m.deptype = 'e'))
 ORDER BY 1, 2";

async fn reset_local_dev_database(
    pg: &PgMigrationStorage,
    url: &str,
    dependent_lanes: DependentLanes,
) -> Result<(), Box<dyn std::error::Error>> {
    let confirmed = std::env::var(RESET_CONFIRM_ENV).as_deref() == Ok(RESET_CONFIRM_VALUE);
    if !confirmed {
        return Err(format!(
            "set {RESET_CONFIRM_ENV}={RESET_CONFIRM_VALUE} to reset a local development database"
        )
        .into());
    }
    reset_local_dev_database_confirmed(pg, url, dependent_lanes).await
}

/// `sqlx` may expose an empty host when the connection options rely on a
/// default local Postgres host; this dev-only reset treats that as local.
const LOCAL_POSTGRES_EMPTY_HOST: &str = "";

fn is_local_postgres_host(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | LOCAL_POSTGRES_EMPTY_HOST) || host.starts_with('/')
}

/// `DROP SCHEMA .. CASCADE` also drops what OTHER lanes built on these
/// schemas, but those lanes' ledger rows stay, so their migrator never
/// re-creates it: the consumer silently loses an FK, a view, a trigger or a
/// policy. Refuse before anything is dropped, naming each one, unless the
/// operator opted in; returns the dependents the reset will drop.
async fn census_foreign_dependents(
    pool: &sqlx::PgPool,
    dependent_lanes: DependentLanes,
) -> Result<Vec<(String, String)>, Box<dyn std::error::Error>> {
    let dependents: Vec<(String, String)> =
        sqlx::query_as(FOREIGN_DEPENDENTS).fetch_all(pool).await?;
    if dependents.is_empty() {
        return Ok(dependents);
    }
    // The detail goes to stderr line by line; the error names the objects on
    // one line, since `main` prints it through `Debug`.
    eprintln!("objects of other lanes that DROP SCHEMA proxima_* .. CASCADE drops:");
    for (object, on) in &dependents {
        eprintln!("  - {object} (depends on {on})");
    }
    if dependent_lanes == DependentLanes::Refuse {
        let objects: std::collections::BTreeSet<&str> = dependents
            .iter()
            .map(|(object, _)| object.as_str())
            .collect();
        return Err(format!(
            "dev reset refuses: {} object(s) of other lanes depend on the proxima_* schemas or \
             sit on them ({}), and the CASCADE would drop them (and whatever depends on them) \
             while their lanes' ledger rows still claim them. Reset those lanes first (drop \
             their objects and delete their ledger rows), or pass {RESET_DEPENDENT_LANES_FLAG} \
             to drop them here and delete every ledger row no Proxima lane owns \
             (public._sqlx_migrations and the other public._sqlx_migrations_* tables), so \
             those lanes re-run from scratch",
            objects.len(),
            objects.into_iter().collect::<Vec<_>>().join("; ")
        )
        .into());
    }
    // The opt-in must be able to delete every row it promises to, or it would
    // stop halfway: dependents dropped, their rows still claiming them.
    let undeletable: Vec<String> = sqlx::query_scalar(
        r"SELECT 'public.' || c.relname
            FROM pg_catalog.pg_class c
           WHERE c.relnamespace = 'public'::regnamespace
             AND c.relkind IN ('r', 'p')
             AND (c.relname = '_sqlx_migrations'
                  OR c.relname LIKE '\_sqlx\_migrations\_%' ESCAPE '\')
             AND NOT has_table_privilege(c.oid, 'DELETE')
           ORDER BY 1",
    )
    .fetch_all(pool)
    .await?;
    if !undeletable.is_empty() {
        return Err(format!(
            "dev reset refuses {RESET_DEPENDENT_LANES_FLAG}: this role cannot delete from {}, \
             so the dependent lanes' rows would outlive their dropped objects; nothing was \
             dropped",
            undeletable.join(", ")
        )
        .into());
    }
    Ok(dependents)
}

async fn reset_local_dev_database_confirmed(
    pg: &PgMigrationStorage,
    url: &str,
    dependent_lanes: DependentLanes,
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
    let dependents = census_foreign_dependents(&pool, dependent_lanes).await?;

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
    // them — see docs/contributing.md), plus every flavor version the
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
    if !dependents.is_empty() {
        delete_dependent_lane_rows(&pool, &flavor_versions).await?;
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

/// With dependents dropped, a ledger row no Proxima lane owns may claim one of
/// them. The reset cannot say which lane built which object, so the opt-in
/// deletes every such row — in the shared `public._sqlx_migrations`, and in
/// every other `public._sqlx_migrations_*` table (the facade's per-lane
/// ledgers; the compiled flavors' were dropped above) — and the consumer's
/// migrators re-run their lanes. Objects of those lanes the reset did not drop
/// are the consumer's to reset (docs/contributing.md §Reset).
async fn delete_dependent_lane_rows(
    pool: &sqlx::PgPool,
    flavor_versions: &[i64],
) -> Result<(), sqlx::Error> {
    let shared_exists: bool =
        sqlx::query_scalar("SELECT to_regclass('public._sqlx_migrations') IS NOT NULL")
            .fetch_one(pool)
            .await?;
    if shared_exists {
        let deleted: Vec<i64> = sqlx::query_scalar(
            "DELETE FROM public._sqlx_migrations
              WHERE version > $1
                AND NOT (version = ANY($2::bigint[]))
              RETURNING version",
        )
        .bind(CORE_MIGRATION_VERSION_CEILING)
        .bind(flavor_versions)
        .fetch_all(pool)
        .await?;
        eprintln!(
            "deleted public._sqlx_migrations rows of the dependent lanes (they re-run from \
             scratch; reset their remaining objects first): {deleted:?}"
        );
    }
    // The server quotes each name (`format('%I')`); nothing from Rust is
    // spliced into the statement.
    let lane_ledgers: Vec<(String, String)> = sqlx::query_as(
        r"SELECT c.relname::text,
                 format('DELETE FROM public.%I RETURNING version', c.relname)
            FROM pg_catalog.pg_class c
           WHERE c.relnamespace = 'public'::regnamespace
             AND c.relkind IN ('r', 'p')
             AND c.relname LIKE '\_sqlx\_migrations\_%' ESCAPE '\'
           ORDER BY 1",
    )
    .fetch_all(pool)
    .await?;
    for (table, delete) in lane_ledgers {
        let deleted: Vec<i64> = sqlx::query_scalar(sqlx::AssertSqlSafe(delete))
            .fetch_all(pool)
            .await?;
        eprintln!("deleted public.{table} rows of a dependent lane: {deleted:?}");
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
            let pg = proxima_storage_pg::PgMigrationStorage::connect_with_config(
                &platform_url,
                PgPoolConfig::from_env()?,
                PgTuning::from_env()?,
            )
            .await?;
            sqlx::query("CREATE SCHEMA proxima_core")
                .execute(&pg.clone_pool_for_backend())
                .await?;
            sqlx::query("CREATE SCHEMA proxima_code")
                .execute(&pg.clone_pool_for_backend())
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
            .execute(&pg.clone_pool_for_backend())
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
                .execute(&pg.clone_pool_for_backend())
                .await?;
            }

            reset_local_dev_database_confirmed(&pg, &url, DependentLanes::Refuse).await?;

            let remaining: Vec<i64> =
                sqlx::query_scalar("SELECT version FROM public._sqlx_migrations ORDER BY version")
                    .fetch_all(&pg.clone_pool_for_backend())
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

    /// A consumer lane's row in the shared ledger: timestamp-shaped, in the
    /// downstream-host suffix lane (docs/09 §Version lanes).
    const CONSUMER_LANE_VERSION: i64 = 20_260_924_000_060;

    /// What a consumer lane embedding Proxima builds on `proxima_core`: an FK
    /// and a view that depend on it, and a trigger on its table whose routine
    /// lives in the consumer's schema.
    const CONSUMER_LANE: &str = "
        CREATE SCHEMA consumer;
        CREATE TABLE consumer.orders (
            id bigint PRIMARY KEY,
            memory_t uuid NOT NULL REFERENCES proxima_core.memory (t)
        );
        CREATE VIEW consumer.memory_ids AS SELECT t FROM proxima_core.memory;
        CREATE FUNCTION consumer.audit() RETURNS trigger
            LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$;
        CREATE TRIGGER consumer_audit AFTER INSERT ON proxima_core.memory
            FOR EACH ROW EXECUTE FUNCTION consumer.audit();";

    /// `CONSUMER_LANE` with its row in the shared ledger, plus a second lane
    /// composed through the facade that keeps its own ledger, written by the
    /// same migration role.
    async fn build_consumer_lanes(
        pg: &PgMigrationStorage,
        admin: &sqlx::PgPool,
    ) -> Result<(), sqlx::Error> {
        sqlx::raw_sql(CONSUMER_LANE).execute(admin).await?;
        sqlx::query(
            "INSERT INTO public._sqlx_migrations
                (version, description, success, checksum, execution_time)
             VALUES ($1, 'consumer orders', true, decode('00', 'hex'), 0)",
        )
        .bind(CONSUMER_LANE_VERSION)
        .execute(admin)
        .await?;
        sqlx::raw_sql(
            "CREATE TABLE public._sqlx_migrations_consumer
                 (LIKE public._sqlx_migrations INCLUDING ALL);
             INSERT INTO public._sqlx_migrations_consumer
                 (version, description, success, checksum, execution_time)
             VALUES (20260924000061, 'consumer audit', true, '\\x00', 0);",
        )
        .execute(&pg.clone_pool_for_backend())
        .await?;
        Ok(())
    }

    #[tokio::test]
    async fn reset_refuses_to_cascade_into_a_consumer_lane_unless_asked() {
        let db_name = unique_db_name("proxima_dev_migrate_reset_consumer");
        create_db(&db_name).await.expect("PG required for tests");
        let url = db_url(&db_name);
        let (_, platform_url) = split_role_urls(&db_name).await.expect("split role URLs");

        let result: Result<(), Box<dyn std::error::Error>> = async {
            let pg = proxima_storage_pg::PgMigrationStorage::connect_with_config(
                &platform_url,
                PgPoolConfig::from_env()?,
                PgTuning::from_env()?,
            )
            .await?;
            run_core_and_flavor_migrations(&pg, proxima_code::CodeFlavor::migrators()).await?;
            let admin = sqlx::PgPool::connect(&url).await?;
            build_consumer_lanes(&pg, &admin).await?;

            let ledger = || async {
                sqlx::query_scalar::<_, i64>(
                    "SELECT version FROM public._sqlx_migrations
                      UNION ALL
                     SELECT version FROM public._sqlx_migrations_consumer
                      ORDER BY 1",
                )
                .fetch_all(&admin)
                .await
            };
            let consumer_fks = || async {
                sqlx::query_scalar::<_, i64>(
                    "SELECT count(*) FROM pg_constraint
                      WHERE conrelid = 'consumer.orders'::regclass AND contype = 'f'",
                )
                .fetch_one(&admin)
                .await
            };
            let core_exists = || async {
                sqlx::query_scalar::<_, bool>("SELECT to_regnamespace('proxima_core') IS NOT NULL")
                    .fetch_one(&admin)
                    .await
            };

            // Plain reset: refuses before dropping or deleting anything, and
            // names every object the CASCADE would have taken.
            let before = ledger().await?;
            let refusal = reset_local_dev_database_confirmed(&pg, &url, DependentLanes::Refuse)
                .await
                .expect_err("a consumer lane depends on proxima_core")
                .to_string();
            for named in [
                "constraint orders_memory_t_fkey on table consumer.orders",
                "view consumer.memory_ids",
                "trigger consumer_audit on table proxima_core.memory",
            ] {
                assert!(refusal.contains(named), "{named} missing from: {refusal}");
            }
            assert!(core_exists().await?, "refusal must drop nothing");
            assert_eq!(
                consumer_fks().await?,
                1,
                "refusal must keep the consumer FK"
            );
            assert_eq!(ledger().await?, before, "refusal must delete no ledger row");

            // Opt-in over a lane ledger this role cannot delete from: refused
            // up front, so it never stops halfway.
            sqlx::query("CREATE TABLE public._sqlx_migrations_foreign (version bigint)")
                .execute(&admin)
                .await?;
            let refusal = reset_local_dev_database_confirmed(&pg, &url, DependentLanes::Reset)
                .await
                .expect_err("an undeletable lane ledger must refuse the opt-in")
                .to_string();
            assert!(
                refusal.contains("public._sqlx_migrations_foreign"),
                "{refusal}"
            );
            assert!(core_exists().await?, "the refused opt-in must drop nothing");
            assert_eq!(
                ledger().await?,
                before,
                "the refused opt-in must delete no row"
            );
            sqlx::query("DROP TABLE public._sqlx_migrations_foreign")
                .execute(&admin)
                .await?;

            // Opt-in: the dependents go with Proxima's schemas, and so does
            // every ledger row no Proxima lane owns, shared or per-lane —
            // none is left claiming an object that no longer exists.
            reset_local_dev_database_confirmed(&pg, &url, DependentLanes::Reset).await?;
            assert!(!core_exists().await?, "proxima_core is dropped");
            assert_eq!(consumer_fks().await?, 0, "the consumer FK went with it");
            assert!(
                ledger().await?.is_empty(),
                "no orphaned ledger row may survive the opt-in reset"
            );

            // Proxima's own lane is not a dependent of itself, nor is a
            // default ACL scoped to one of its schemas: a freshly migrated
            // database resets without the opt-in.
            run_core_and_flavor_migrations(&pg, proxima_code::CodeFlavor::migrators()).await?;
            sqlx::query(
                "ALTER DEFAULT PRIVILEGES IN SCHEMA proxima_core GRANT SELECT ON TABLES TO PUBLIC",
            )
            .execute(&admin)
            .await?;
            reset_local_dev_database_confirmed(&pg, &url, DependentLanes::Refuse).await?;
            run_core_and_flavor_migrations(&pg, proxima_code::CodeFlavor::migrators()).await?;
            Ok(())
        }
        .await;

        let _ = drop_db(&db_name).await;
        result.expect("dev reset consumer-lane regression failed");
    }
}

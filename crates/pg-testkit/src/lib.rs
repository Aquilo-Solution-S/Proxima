//! Postgres test harness for clone-per-test isolation.
//!
//! Matches the `#[sqlx::test]` shape without a proc-macro crate:
//!
//! - each clone is recorded in `_proxima_test.databases` on the admin DB
//! - a successful [`DbGuard`] drop runs `DROP DATABASE … WITH (FORCE)`
//! - a panicking test **keeps** the database and prints a redacted `psql` URL
//! - the first admin operation in a process sweeps clones older than
//!   five minutes with no live backend (`pg_stat_activity`)
//! - [`ensure_template`] drops other hashes in the same family (`proxima_tmpl_core_*`
//!   / `proxima_tmpl_split_*`), keeping only the current fingerprint
//! - a host owns a [`TemplateFamily`] of its own: [`ensure_template_in`] builds
//!   `<prefix><hash>` and collects only idle templates of that family
//! - the admin URL is `PROXIMA_TEST_PG_URL` and nothing else: unset is a
//!   configuration error ([`admin_url_or_skip`] for a visible skip), never a
//!   guess at a local server
//! - every lock (builds, leases, the sweep) and the catalog live in the
//!   database the admin URL names, so processes coordinate only when their
//!   `PROXIMA_TEST_PG_URL` names the same database
//!
//! Failed tests stay until that grace elapses with no live session, so a
//! later nextest binary cannot drop an in-flight clone in the create-then-connect
//! window.

use std::fmt;
use std::future::Future;
use std::ops::Deref;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use sqlx::postgres::PgPoolOptions;
use sqlx::{AssertSqlSafe, Connection, PgConnection, PgPool};
use time::OffsetDateTime;
use uuid::Uuid;

const ADMIN_URL_VAR: &str = "PROXIMA_TEST_PG_URL";
/// The one message for an unconfigured admin URL: the panic of [`admin_url`]
/// and the `sqlx::Error::Configuration` of every async entry point. It names
/// the variable and never a URL, so it cannot carry a credential.
const ADMIN_URL_UNSET: &str = concat!(
    "PROXIMA_TEST_PG_URL is not set (or is empty): point it at the admin URL of a disposable ",
    "Postgres, postgres://USER:PASSWORD@HOST:PORT/DATABASE; there is no default server",
);
const DROP_RETRIES: usize = 25;
const DROP_RETRY_DELAY: Duration = Duration::from_millis(200);
const SQLSTATE_DATABASE_ACCESSED: &str = "55006";
const SQLSTATE_UNDEFINED_DATABASE: &str = "3D000";
/// Untracked leftovers (pre-harness leaks) older than this are swept at boot.
const UNTRACKED_GRACE: time::Duration = time::Duration::minutes(5);
const SPLIT_PLATFORM_ROLE: &str = "proxima_test_platform";
const SPLIT_RUNTIME_ROLE: &str = "proxima_test_runtime";
const SPLIT_ROLE_PASSWORD: &str = "proxima_test_fixture_password";
/// The SCRAM-SHA-256 verifier of [`SPLIT_ROLE_PASSWORD`] with a fixed salt
/// (`proxima-fixture!`, 4096 iterations). Fixture provisioning sets the
/// roles' password to this exact verifier on every run: a plaintext
/// `PASSWORD` would be re-salted each time, and a pooler that cached the
/// previous verifier's SCRAM keys (`PgBouncer` with `auth_query`) would then
/// fail the next server login with "password authentication failed".
const SPLIT_ROLE_SCRAM_VERIFIER: &str = "SCRAM-SHA-256$4096:cHJveGltYS1maXh0dXJlIQ==$KkHJiBZCT4yCgl53VpmtLPotR/SKsNM46MGUaurVMxA=:900L6wrNsK7qb8uGyn/tR71et8wN2/mx8DJHBUsaj4A=";
/// The query parameters `sqlx` lets override the authority's credentials.
const QUERY_USER: &str = "user";
const QUERY_PASSWORD: &str = "password";
/// A template database name ends in this many lowercase hex digits (`{hash:016x}`).
const TEMPLATE_HASH_DIGITS: usize = 16;
/// `PostgreSQL` identifiers hold 63 bytes; a family prefix leaves room for the hash.
const MAX_FAMILY_PREFIX_BYTES: usize = 63 - TEMPLATE_HASH_DIGITS;
/// The built-in template families, collected by [`ensure_template`] and
/// [`drop_stale_templates`].
const BUILTIN_FAMILIES: [&str; 2] = ["proxima_tmpl_core_", "proxima_tmpl_split_"];
/// Every prefix a [`TemplateFamily`] must keep clear of: the built-in
/// families and the staging databases of a template build.
const RESERVED_PREFIXES: [&str; 3] = [
    BUILTIN_FAMILIES[0],
    BUILTIN_FAMILIES[1],
    "proxima_tmpl_build_",
];
pub const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
pub const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

static PROCESS_START: OnceLock<OffsetDateTime> = OnceLock::new();
static SWEEP_DONE: AtomicBool = AtomicBool::new(false);
static CATALOG_READY: AtomicBool = AtomicBool::new(false);

/// `PROXIMA_TEST_PG_URL`, trimmed; `None` when it is unset, empty or
/// whitespace-only — the rule `proxima_core::env_value` states for every
/// configuration variable in the workspace. Applied by hand rather than by
/// calling it, because this is a deliberately minimal test-support crate
/// (sqlx/tokio/tracing/url/uuid) and one env var does not justify a
/// dependency on `proxima-core`.
///
/// The one place the variable is read: every other spelling of "no admin URL"
/// derives from this answer.
fn configured_admin_url() -> Option<String> {
    trimmed_url(std::env::var(ADMIN_URL_VAR).ok())
}

fn trimmed_url(raw: Option<String>) -> Option<String> {
    raw.map(|raw| raw.trim().to_string())
        .filter(|url| !url.is_empty())
}

const POOLER_URL_VAR: &str = "PROXIMA_TEST_PGBOUNCER_URL";

/// `PROXIMA_TEST_PGBOUNCER_URL`, trimmed: a transaction-mode pooler in front
/// of the `PROXIMA_TEST_PG_URL` server. `None` when unset or empty. When set,
/// the storage fixtures connect their serving pools through it; migrations
/// and the admin pool stay direct, as they must in production.
#[must_use]
pub fn test_pooler_url() -> Option<String> {
    trimmed_url(std::env::var(POOLER_URL_VAR).ok())
}

/// `url` with its host and port replaced by the pooler's from
/// [`test_pooler_url`]; `url` unchanged when no pooler is configured. Role,
/// password and database stay those of `url`.
///
/// # Panics
///
/// Panics when either URL does not parse or the pooler URL names no host.
#[must_use]
pub fn through_test_pooler(url: &str) -> String {
    let Some(pooler) = test_pooler_url() else {
        return url.to_owned();
    };
    let pooler =
        url::Url::parse(&pooler).unwrap_or_else(|_| panic!("{POOLER_URL_VAR} is not a URL"));
    let mut routed = url::Url::parse(url).expect("a fixture database URL parses");
    routed
        .set_host(Some(
            pooler
                .host_str()
                .unwrap_or_else(|| panic!("{POOLER_URL_VAR} names no host")),
        ))
        .expect("a postgres URL takes a host");
    routed
        .set_port(pooler.port())
        .expect("a postgres URL takes a port");
    routed.into()
}

/// [`configured_admin_url`] as the error every async entry point returns.
fn try_admin_url() -> Result<String, sqlx::Error> {
    configured_admin_url().ok_or_else(|| sqlx::Error::Configuration(ADMIN_URL_UNSET.into()))
}

/// The admin connection URL from `PROXIMA_TEST_PG_URL`.
///
/// Trims, and treats an empty or whitespace-only value as unset. There is no
/// default server: the sweep and the template GC act on every database
/// behind this URL that matches their patterns, so a suite pointed at
/// whatever answers on `localhost` could drop another suite's idle clones.
/// Use [`admin_url_or_skip`] to skip a test when it is unconfigured.
///
/// # Panics
///
/// Panics when the variable is unset, empty or whitespace-only. The message
/// names the variable and carries no URL.
#[must_use]
pub fn admin_url() -> String {
    configured_admin_url().unwrap_or_else(|| panic!("{ADMIN_URL_UNSET}"))
}

/// [`admin_url`] for a test that may skip: `None` after one line on stderr
/// when `PROXIMA_TEST_PG_URL` is unset, empty or whitespace-only.
///
/// # Panics
///
/// Panics under `CI=true` instead of skipping: a CI run that silently ran no
/// Postgres test would report green having tested nothing.
#[must_use]
pub fn admin_url_or_skip() -> Option<String> {
    let url = configured_admin_url();
    if url.is_none() {
        assert!(
            std::env::var("CI").as_deref() != Ok("true"),
            "{ADMIN_URL_UNSET}; a CI run does not skip its Postgres tests"
        );
        eprintln!("proxima-pg-testkit: skipping this Postgres test, {ADMIN_URL_VAR} is unset");
    }
    url
}

/// URL for one test database, retaining the admin endpoint configuration.
///
/// # Panics
///
/// Panics if `PROXIMA_TEST_PG_URL` is unset ([`admin_url`]) or not a valid
/// URL. The diagnostic does not include the URL or its credentials.
#[must_use]
pub fn db_url(name: &str) -> String {
    db_url_from_admin(&admin_url(), name)
        .expect("PROXIMA_TEST_PG_URL must be a valid database URL")
        .into()
}

fn db_url_from_admin(admin: &str, name: &str) -> Result<url::Url, url::ParseError> {
    let mut url = url::Url::parse(admin)?;
    url.path_segments_mut()
        .map_err(|()| url::ParseError::RelativeUrlWithCannotBeABaseBase)?
        .clear()
        .push(name);
    let parameters: Vec<_> = url
        .query_pairs()
        .filter(|(key, _)| key != "dbname")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.set_query(None);
    // SQLx gives dbname precedence over the path. Keep exactly one target
    // override, including names (such as ".") that URL paths normalize.
    url.query_pairs_mut()
        .extend_pairs(parameters)
        .append_pair("dbname", name);
    Ok(url)
}

/// Owns one ephemeral test database.
///
/// Dropping a guard after a **passing** test deletes the database with
/// `DROP DATABASE … WITH (FORCE)`. Dropping during unwind keeps the
/// database and prints a redacted `psql` URL, same as `#[sqlx::test]`.
///
/// Keep the guard alive for the whole test. `let (pg, _) = fresh_pg(…)`
/// drops the clone before the body runs.
#[derive(Debug)]
#[must_use = "dropping DbGuard deletes the test database (kept if the test is panicking)"]
pub struct DbGuard {
    name: String,
    drop_on_success: bool,
}

impl DbGuard {
    /// Take ownership of an already-created clone.
    ///
    /// The name must already have been recorded by [`create_db`] or
    /// [`create_db_from_template`].
    pub fn adopt(name: String) -> Self {
        Self {
            name,
            drop_on_success: true,
        }
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Leave the database in place even if the test passes.
    pub fn keep(&mut self) {
        self.drop_on_success = false;
    }
}

impl Deref for DbGuard {
    type Target = str;

    fn deref(&self) -> &str {
        &self.name
    }
}

impl AsRef<str> for DbGuard {
    fn as_ref(&self) -> &str {
        &self.name
    }
}

impl fmt::Display for DbGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name)
    }
}

impl Drop for DbGuard {
    fn drop(&mut self) {
        if !self.drop_on_success {
            return;
        }
        if std::thread::panicking() {
            eprintln!(
                "proxima-pg-testkit: keeping test database `{}` after panic\n  psql '{}'",
                self.name,
                redacted_db_url(&self.name)
            );
            return;
        }
        let name = self.name.clone();
        match std::thread::Builder::new()
            .name("proxima-pg-testkit-drop".into())
            .spawn(move || drop_db_blocking(&name))
        {
            Ok(handle) => {
                if handle.join().is_err() {
                    tracing::warn!("proxima-pg-testkit drop thread panicked");
                }
            }
            Err(error) => {
                tracing::warn!(%error, "failed to spawn test database drop thread");
            }
        }
    }
}

fn drop_db_blocking(name: &str) {
    match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => {
            if let Err(error) = runtime.block_on(drop_db(name)) {
                tracing::warn!(database = name, %error, "failed to drop test database");
            }
        }
        Err(error) => {
            tracing::warn!(%error, "failed to build runtime to drop test database");
        }
    }
}

/// `parsed` with every password masked: the authority's, and the `password`
/// query parameter `sqlx` accepts in its place.
fn redacted_url(mut parsed: url::Url) -> url::Url {
    if parsed.password().is_some() {
        let _ = parsed.set_password(Some("****"));
    }
    if parsed.query_pairs().any(|(key, _)| key == QUERY_PASSWORD) {
        let pairs: Vec<(String, String)> = parsed
            .query_pairs()
            .map(|(key, value)| {
                let value = if key == QUERY_PASSWORD {
                    "****".to_owned()
                } else {
                    value.into_owned()
                };
                (key.into_owned(), value)
            })
            .collect();
        parsed.set_query(None);
        parsed.query_pairs_mut().extend_pairs(pairs);
    }
    parsed
}

/// Never panics: it runs while a panicking test unwinds, where a second
/// panic aborts the process. An unconfigured or invalid admin URL falls back
/// to the bare database name.
fn redacted_db_url(name: &str) -> String {
    match configured_admin_url().map(|admin| db_url_from_admin(&admin, name)) {
        Some(Ok(parsed)) => redacted_url(parsed).to_string(),
        Some(Err(_)) | None => name.to_owned(),
    }
}

#[must_use]
pub fn unique_db_name(prefix: &str) -> String {
    format!("{}_{}", prefix, Uuid::now_v7().simple())
}

#[must_use]
pub fn fnv1a64_extend(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

#[must_use]
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    fnv1a64_extend(FNV_OFFSET_BASIS, bytes)
}

/// # Errors
///
/// Returns any database connection or `CREATE DATABASE` error.
pub async fn create_db(name: &str) -> Result<(), sqlx::Error> {
    let mut conn = connect_admin().await?;
    sqlx::raw_sql(AssertSqlSafe(format!(
        "CREATE DATABASE {}",
        quoted_ident(name)
    )))
    .execute(&mut conn)
    .await?;
    record_new_db(&mut conn, name).await?;
    conn.close().await?;
    Ok(())
}

/// Provision the shared, non-escalating roles used by split-role PG tests and
/// return URLs for the target database.
///
/// The roles are intentionally reusable across disposable databases. Their
/// password is a local fixture value and must never be used outside tests.
/// Existing objects move to the platform role except candidate functions,
/// which keep their dedicated owner, before runtime grants are applied.
///
/// # Errors
/// Returns admin, target-database, catalog, or role-attribute errors.
pub async fn split_role_urls(database: &str) -> Result<(String, String), sqlx::Error> {
    split_role_urls_for(database, &[]).await
}

/// [`split_role_urls`] for a database that also holds out-of-tree flavor
/// schemas: objects already in `flavor_schemas` move to the platform role
/// and the runtime role gets the same grants it gets on `proxima_core`.
/// Each passed flavor schema also gets its migration ledger pre-created as
/// `public._sqlx_migrations_<schema>`: the ledger a flavor's
/// `NamedMigrator::flavor` records into when the flavor id's `-` maps to
/// the schema's `_` (`proxima-notes` → `proxima_notes`). Existing
/// public tables whose names start with `_sqlx_migrations` also move to the
/// platform role, with read-only access for the runtime role.
///
/// Only the first call on a database prepares it; a schema the platform
/// role creates later (the usual case: boot migrates as platform) is
/// covered by the platform role's default privileges instead.
///
/// # Errors
/// Returns admin, target-database, catalog, or role-attribute errors, and a
/// protocol error for a schema name that is not a plain lowercase
/// identifier.
pub async fn split_role_urls_for(
    database: &str,
    flavor_schemas: &[&str],
) -> Result<(String, String), sqlx::Error> {
    split_roles(
        database,
        flavor_schemas,
        &["vector", "btree_gin", "pg_trgm"],
    )
    .await
}

/// [`split_role_urls`] for the database of a store whose vectors live in a
/// host index: the same roles and grants, without the `vector` extension, so
/// installing the schema there proves it needs none.
///
/// # Errors
/// As [`split_role_urls`].
pub async fn split_role_urls_without_pgvector(
    database: &str,
) -> Result<(String, String), sqlx::Error> {
    split_roles(database, &[], &["btree_gin", "pg_trgm"]).await
}

#[expect(
    clippy::too_many_lines,
    reason = "isolated split-role fixture provisioning"
)]
async fn split_roles(
    database: &str,
    flavor_schemas: &[&str],
    extensions: &[&str],
) -> Result<(String, String), sqlx::Error> {
    let database = database.trim();
    if database.is_empty() {
        return Err(sqlx::Error::Protocol("database name is empty".into()));
    }
    if let Some(schema) = flavor_schemas.iter().find(|schema| {
        schema.is_empty()
            || !schema.as_bytes()[0].is_ascii_lowercase()
            || !schema
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    }) {
        return Err(sqlx::Error::Protocol(format!(
            "flavor schema {schema:?} is not a plain lowercase identifier"
        )));
    }
    let mut schemas: Vec<&str> = vec!["proxima_core"];
    for schema in flavor_schemas {
        if !schemas.contains(schema) {
            schemas.push(schema);
        }
    }
    let schema_list = schemas
        .iter()
        .map(|schema| sql_literal(schema))
        .collect::<Vec<_>>()
        .join(", ");
    let flavor_ledger_list = schemas[1..]
        .iter()
        .map(|schema| sql_literal(schema))
        .collect::<Vec<_>>()
        .join(", ");
    let mut control = connect_admin().await?;
    let lock_key = advisory_lock_key("_proxima_test.split_roles");
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(lock_key)
        .execute(&mut control)
        .await?;
    let result = async {
        for role in [SPLIT_PLATFORM_ROLE, SPLIT_RUNTIME_ROLE] {
            let statement = format!(
                "DO $$ BEGIN
                   IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = {name}) THEN
                     CREATE ROLE {role} LOGIN PASSWORD {password}
                       NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT NOBYPASSRLS;
                   ELSE
                     ALTER ROLE {role} LOGIN PASSWORD {password}
                       NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT NOBYPASSRLS;
                   END IF;
                 END $$",
                name = sql_literal(role),
                role = quoted_ident(role),
                password = sql_literal(SPLIT_ROLE_SCRAM_VERIFIER),
            );
            // SQL-POLICY: fixed-fragment — generated role identifiers and a local fixture password.
            sqlx::raw_sql(AssertSqlSafe(statement))
                .execute(&mut control)
                .await?;
        }
        let search_membership = format!(
            "GRANT proxima_search TO {platform} WITH ADMIN FALSE, INHERIT FALSE, SET TRUE;
             REVOKE proxima_search FROM {runtime}",
            platform = quoted_ident(SPLIT_PLATFORM_ROLE),
            runtime = quoted_ident(SPLIT_RUNTIME_ROLE),
        );
        // SQL-POLICY: fixed-fragment — closed fixture role identifiers only.
        sqlx::raw_sql(AssertSqlSafe(search_membership))
            .execute(&mut control)
            .await?;
        // Neither fixture role may inherit membership from the other.
        for (member, role) in [(SPLIT_RUNTIME_ROLE, SPLIT_PLATFORM_ROLE), (SPLIT_PLATFORM_ROLE, SPLIT_RUNTIME_ROLE)] {
            let statement = format!(
                "REVOKE {role} FROM {member}",
                role = quoted_ident(role),
                member = quoted_ident(member),
            );
            // SQL-POLICY: fixed-fragment — generated role identifiers only.
            sqlx::raw_sql(AssertSqlSafe(statement))
                .execute(&mut control)
                .await?;
        }
        let statement = format!(
            "GRANT CREATE ON DATABASE {} TO {}",
            quoted_ident(database),
            quoted_ident(SPLIT_PLATFORM_ROLE),
        );
        // SQL-POLICY: fixed-fragment — generated database and role identifiers only.
        sqlx::raw_sql(AssertSqlSafe(statement))
            .execute(&mut control)
            .await?;

        let mut target = PgConnection::connect(&db_url(database)).await?;
        // Several HTTP test binaries share one isolated database. Once its
        // grants/defaults are prepared, never repeat ownership DDL alongside
        // a host's migration transaction (SQLx's ledger lock has another order).
        let prepared: bool = sqlx::query_scalar(
            "SELECT to_regclass('_proxima_test.split_roles_ready') IS NOT NULL",
        )
        .fetch_one(&mut target)
        .await?;
        if prepared {
            target.close().await?;
            return Ok(());
        }
        for extension in extensions.iter().copied() {
            let statement = format!(
                "CREATE EXTENSION IF NOT EXISTS {}",
                quoted_ident(extension)
            );
            // SQL-POLICY: fixed-fragment — extension names are the closed lists above.
            sqlx::raw_sql(AssertSqlSafe(statement))
                .execute(&mut target)
                .await?;
        }
        let public_grants = format!(
            "GRANT USAGE ON SCHEMA public TO {runtime}, {platform}, proxima_search;
             GRANT CREATE ON SCHEMA public TO {platform}",
            runtime = quoted_ident(SPLIT_RUNTIME_ROLE),
            platform = quoted_ident(SPLIT_PLATFORM_ROLE),
        );
        // SQL-POLICY: fixed-fragment — generated role identifiers only.
        sqlx::raw_sql(AssertSqlSafe(public_grants))
            .execute(&mut target)
            .await?;
        let global_defaults = format!(
            "ALTER DEFAULT PRIVILEGES FOR ROLE {platform}
               GRANT USAGE ON SCHEMAS TO {runtime};
             ALTER DEFAULT PRIVILEGES FOR ROLE {platform}
               REVOKE CREATE ON SCHEMAS FROM PUBLIC;
             ALTER DEFAULT PRIVILEGES FOR ROLE {platform}
               GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO {runtime};
             ALTER DEFAULT PRIVILEGES FOR ROLE {platform}
               GRANT USAGE, SELECT, UPDATE ON SEQUENCES TO {runtime};
             ALTER DEFAULT PRIVILEGES FOR ROLE proxima_search
               GRANT EXECUTE ON FUNCTIONS TO {runtime}",
            runtime = quoted_ident(SPLIT_RUNTIME_ROLE),
            platform = quoted_ident(SPLIT_PLATFORM_ROLE),
        );
        // SQL-POLICY: fixed-fragment — generated role identifiers only.
        sqlx::raw_sql(AssertSqlSafe(global_defaults))
            .execute(&mut target)
            .await?;
        sqlx::raw_sql(
            "CREATE TABLE IF NOT EXISTS public._sqlx_migrations (
                 version bigint PRIMARY KEY,
                 description text NOT NULL,
                 installed_on timestamptz NOT NULL DEFAULT now(),
                 success boolean NOT NULL,
                 checksum bytea NOT NULL,
                 execution_time bigint NOT NULL
             )",
        )
        .execute(&mut target)
        .await?;
        let transfer = format!(
            "DO $$ DECLARE r record; s text; BEGIN
               FOREACH s IN ARRAY ARRAY[{flavor_ledger_list}]::text[] LOOP
                 EXECUTE format('CREATE TABLE IF NOT EXISTS public.%I (
                     version bigint PRIMARY KEY,
                     description text NOT NULL,
                     installed_on timestamptz NOT NULL DEFAULT now(),
                     success boolean NOT NULL,
                     checksum bytea NOT NULL,
                     execution_time bigint NOT NULL
                   )', '_sqlx_migrations_' || s);
               END LOOP;
               FOR r IN SELECT n.nspname FROM pg_namespace n
                  WHERE n.nspname IN ({schema_list}) LOOP
                 EXECUTE format('ALTER SCHEMA %I OWNER TO %I', r.nspname, '{SPLIT_PLATFORM_ROLE}');
               END LOOP;
               FOR r IN SELECT n.nspname, c.relname, c.relkind
                   FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
                  WHERE n.nspname IN ({schema_list})
                    AND c.relkind IN ('r','p','v','S') LOOP
                 IF r.relkind = 'S' THEN
                   EXECUTE format('ALTER SEQUENCE %I.%I OWNER TO %I', r.nspname, r.relname, '{SPLIT_PLATFORM_ROLE}');
                 ELSIF r.relkind = 'v' THEN
                   EXECUTE format('ALTER VIEW %I.%I OWNER TO %I', r.nspname, r.relname, '{SPLIT_PLATFORM_ROLE}');
                 ELSE
                   EXECUTE format('ALTER TABLE %I.%I OWNER TO %I', r.nspname, r.relname, '{SPLIT_PLATFORM_ROLE}');
                 END IF;
               END LOOP;
               FOR r IN SELECT n.nspname, t.typname
                   FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace
                  WHERE n.nspname IN ({schema_list}) AND t.typtype IN ('e','d') LOOP
                 EXECUTE format('ALTER TYPE %I.%I OWNER TO %I', r.nspname, r.typname, '{SPLIT_PLATFORM_ROLE}');
               END LOOP;
               FOR r IN SELECT n.nspname, p.proname,
                                pg_get_function_identity_arguments(p.oid) AS args
                   FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
                   JOIN pg_roles owner_role ON owner_role.oid = p.proowner
                  WHERE n.nspname IN ({schema_list})
                    AND owner_role.rolname <> 'proxima_search' LOOP
                 EXECUTE format('ALTER FUNCTION %I.%I(%s) OWNER TO %I',
                                r.nspname, r.proname, r.args, '{SPLIT_PLATFORM_ROLE}');
               END LOOP;
               FOR r IN SELECT n.nspname, p.proname,
                                pg_get_function_identity_arguments(p.oid) AS args
                   FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
                   JOIN pg_roles owner_role ON owner_role.oid = p.proowner
                  WHERE n.nspname IN ({schema_list})
                    AND owner_role.rolname = 'proxima_search' AND p.prosecdef LOOP
                 EXECUTE format('GRANT EXECUTE ON FUNCTION %I.%I(%s) TO %I',
                                r.nspname, r.proname, r.args, '{SPLIT_RUNTIME_ROLE}');
               END LOOP;
               FOR r IN SELECT n.nspname, c.relname
                   FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
                  WHERE n.nspname = 'public' AND c.relkind IN ('r','p')
                    AND starts_with(c.relname, '_sqlx_migrations') LOOP
                 EXECUTE format('ALTER TABLE %I.%I OWNER TO %I', r.nspname, r.relname, '{SPLIT_PLATFORM_ROLE}');
                 EXECUTE format('REVOKE INSERT, UPDATE, DELETE, TRUNCATE ON %I.%I FROM %I',
                                r.nspname, r.relname, '{SPLIT_RUNTIME_ROLE}');
                 EXECUTE format('GRANT SELECT ON %I.%I TO %I', r.nspname, r.relname, '{SPLIT_RUNTIME_ROLE}');
               END LOOP;
             END $$",
        );
        // SQL-POLICY: fixed-fragment — validated schema literals and closed role names; catalog identifiers use PostgreSQL format %I.
        sqlx::raw_sql(AssertSqlSafe(transfer))
            .execute(&mut target)
            .await?;
        for schema in &schemas {
            let statement = format!(
                "DO $$ BEGIN
                   IF to_regnamespace('{schema_name}') IS NOT NULL THEN
                     EXECUTE 'GRANT USAGE ON SCHEMA {schema} TO {runtime}';
                     EXECUTE 'REVOKE CREATE ON SCHEMA {schema} FROM PUBLIC';
                     EXECUTE 'REVOKE CREATE ON SCHEMA {schema} FROM {runtime}';
                     EXECUTE 'GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA {schema} TO {runtime}';
                     EXECUTE 'GRANT USAGE, SELECT, UPDATE ON ALL SEQUENCES IN SCHEMA {schema} TO {runtime}';
                     EXECUTE 'ALTER DEFAULT PRIVILEGES FOR ROLE {platform} IN SCHEMA {schema} GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO {runtime}';
                     EXECUTE 'ALTER DEFAULT PRIVILEGES FOR ROLE {platform} IN SCHEMA {schema} GRANT USAGE, SELECT, UPDATE ON SEQUENCES TO {runtime}';
                   END IF;
                 END $$",
                schema = quoted_ident(schema),
                schema_name = schema,
                runtime = quoted_ident(SPLIT_RUNTIME_ROLE),
                platform = quoted_ident(SPLIT_PLATFORM_ROLE),
            );
            // SQL-POLICY: fixed-fragment — schema and role identifiers are closed fixture values.
            sqlx::raw_sql(AssertSqlSafe(statement))
                .execute(&mut target)
                .await?;
        }
        for role in [SPLIT_PLATFORM_ROLE, SPLIT_RUNTIME_ROLE] {
            let attributes: (bool, bool, bool, bool, bool) = sqlx::query_as(
                "SELECT rolsuper, rolbypassrls, rolcreaterole, rolcreatedb, rolinherit
                   FROM pg_roles WHERE rolname = $1",
            )
            .bind(role)
            .fetch_one(&mut target)
            .await?;
            if attributes != (false, false, false, false, false) {
                return Err(sqlx::Error::Protocol(format!(
                    "split-role fixture role {role} has unsafe attributes"
                )));
            }
        }
        sqlx::raw_sql(AssertSqlSafe(
            "CREATE SCHEMA IF NOT EXISTS _proxima_test;
             CREATE TABLE _proxima_test.split_roles_ready (singleton boolean PRIMARY KEY);
             INSERT INTO _proxima_test.split_roles_ready VALUES (true)".to_owned(),
        ))
        .execute(&mut target)
        .await?;
        target.close().await?;
        Ok(())
    }
    .await;
    let unlock = sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(lock_key)
        .execute(&mut control)
        .await;
    control.close().await?;
    result?;
    unlock?;
    let admin = try_admin_url()?;
    let runtime = role_url_from_admin(&admin, database, SPLIT_RUNTIME_ROLE)?;
    let platform = role_url_from_admin(&admin, database, SPLIT_PLATFORM_ROLE)?;
    Ok((runtime.to_string(), platform.to_string()))
}

/// `admin` retargeted at `database`, signing in as the fixture `role`.
///
/// `sqlx` gives a `user=` or `password=` query parameter precedence over the
/// authority, so an admin URL that carries its credentials there would turn
/// the "role" URL back into an admin connection and defeat the split. Both
/// parameters are dropped from the query before the role is set; every other
/// parameter (`sslmode`, `host`, `port`, `dbname`) stays.
fn role_url_from_admin(admin: &str, database: &str, role: &str) -> Result<url::Url, sqlx::Error> {
    let mut url = db_url_from_admin(admin, database)
        .map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
    let kept: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(key, _)| key != QUERY_USER && key != QUERY_PASSWORD)
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.set_query(None);
    url.query_pairs_mut().extend_pairs(kept);
    url.set_username(role)
        .map_err(|()| sqlx::Error::Protocol("invalid fixture role".into()))?;
    url.set_password(Some(SPLIT_ROLE_PASSWORD))
        .map_err(|()| sqlx::Error::Protocol("invalid fixture password".into()))?;
    Ok(url)
}

/// A fresh database with the split platform/runtime roles, dropped on a
/// successful drop (kept after a panic, like [`DbGuard`]).
///
/// Boot the host with [`Self::runtime_url`] as its database URL and
/// [`Self::platform_url`] as its platform URL: migrations then run as the
/// NOSUPERUSER NOBYPASSRLS platform role, which owns every schema and ledger
/// it creates, and the runtime role reaches them through the platform
/// role's default privileges — the shape the runtime RLS guard requires.
#[derive(Debug)]
#[must_use]
pub struct SplitRoleDb {
    guard: DbGuard,
    runtime_url: String,
    platform_url: String,
}

impl SplitRoleDb {
    /// Create `<prefix>_<uuid>` and provision the split roles on it
    /// ([`split_role_urls_for`] with `flavor_schemas`).
    ///
    /// # Errors
    /// Returns database creation or role provisioning errors; a database
    /// created before the failure is dropped.
    pub async fn create(prefix: &str, flavor_schemas: &[&str]) -> Result<Self, sqlx::Error> {
        let name = unique_db_name(prefix);
        create_db(&name).await?;
        let guard = DbGuard::adopt(name);
        let (runtime_url, platform_url) = split_role_urls_for(guard.name(), flavor_schemas).await?;
        Ok(Self {
            guard,
            runtime_url,
            platform_url,
        })
    }

    /// Clone `<prefix>_<uuid>` from the leased `template` and assert the split
    /// roles on the clone ([`split_role_urls_for`] with `flavor_schemas`).
    ///
    /// The clone half of a migrated, split-role template: build the template
    /// with the roles provisioned on its staging database
    /// ([`ensure_template_in`]), and the clone arrives with them prepared, so
    /// the call here re-asserts the shared roles and the database grant and
    /// returns early. A template built without the roles gets them
    /// provisioned on the clone, as [`Self::create`] does.
    ///
    /// Taking the [`TemplateLease`] rather than a name is what keeps the
    /// template from being collected between its build and this clone.
    ///
    /// ```compile_fail,E0308
    /// // A template name is not a lease.
    /// async fn clone() -> Result<(), sqlx::Error> {
    ///     let _ = proxima_pg_testkit::SplitRoleDb::from_template("p", "a_template", &[]).await?;
    ///     Ok(())
    /// }
    /// ```
    ///
    /// # Errors
    /// Returns clone creation, role provisioning or schema-name errors; a
    /// clone created before the failure is dropped.
    pub async fn from_template(
        prefix: &str,
        template: &TemplateLease,
        flavor_schemas: &[&str],
    ) -> Result<Self, sqlx::Error> {
        let guard = DbGuard::adopt(create_db_from_template(prefix, template.name()).await?);
        let (runtime_url, platform_url) = split_role_urls_for(guard.name(), flavor_schemas).await?;
        Ok(Self {
            guard,
            runtime_url,
            platform_url,
        })
    }

    /// Database name.
    #[must_use]
    pub fn name(&self) -> &str {
        self.guard.name()
    }

    /// URL for the runtime role: DML only, never an owner.
    #[must_use]
    pub fn runtime_url(&self) -> &str {
        &self.runtime_url
    }

    /// URL for the platform role: migrations and platform-scope maintenance.
    #[must_use]
    pub fn platform_url(&self) -> &str {
        &self.platform_url
    }

    /// Superuser URL for fixture setup and assertions outside RLS.
    #[must_use]
    pub fn admin_url(&self) -> String {
        db_url(self.guard.name())
    }

    /// Keep the database after drop, for inspection.
    pub fn keep(&mut self) {
        self.guard.keep();
    }
}

/// A host's own family of template databases: `<prefix><hash:016x>`.
///
/// The family is what makes garbage collection safe: [`ensure_template_in`]
/// and [`drop_stale_templates_in`] drop idle templates of this family only,
/// so a host's stale templates are collected and nobody else's are touched.
/// [`ensure_template`] and [`drop_stale_templates`] keep their two built-in
/// families (`proxima_tmpl_{core,split}_`); [`Self::new`] refuses any
/// prefix that could reach one, so the two collectors never meet.
///
/// The family is passed to the call that builds the template. There is no
/// registry of families.
///
/// ```compile_fail,E0451
/// // `new` is the only way in: a family is validated or it does not exist.
/// let _ = proxima_pg_testkit::TemplateFamily { prefix: "proxima_tmpl_core_".to_owned() };
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateFamily {
    prefix: String,
}

impl TemplateFamily {
    /// A family named by `prefix`, a database-name prefix: lowercase ASCII
    /// letters, digits and `_`, starting with a letter, ending with `_`, at
    /// most 47 bytes (so `prefix` plus 16 hex digits fits the 63-byte
    /// database name limit), and neither equal to, prefixed by, nor a prefix
    /// of a built-in family or of the `proxima_tmpl_build_` staging prefix.
    ///
    /// # Errors
    ///
    /// Returns the first [`TemplateFamilyError`] `prefix` earns.
    pub fn new(prefix: &str) -> Result<Self, TemplateFamilyError> {
        if prefix.is_empty() {
            return Err(TemplateFamilyError::Empty);
        }
        if prefix.len() > MAX_FAMILY_PREFIX_BYTES {
            return Err(TemplateFamilyError::TooLong { len: prefix.len() });
        }
        if let Some(found) = prefix
            .chars()
            .find(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_'))
        {
            return Err(TemplateFamilyError::InvalidCharacter { found });
        }
        if !prefix.starts_with(|c: char| c.is_ascii_lowercase()) {
            return Err(TemplateFamilyError::MustStartWithLetter);
        }
        if !prefix.ends_with('_') {
            return Err(TemplateFamilyError::MustEndWithUnderscore);
        }
        if let Some(reserved) = RESERVED_PREFIXES
            .into_iter()
            .find(|reserved| reserved.starts_with(prefix) || prefix.starts_with(reserved))
        {
            return Err(TemplateFamilyError::ReservedOverlap { reserved });
        }
        Ok(Self {
            prefix: prefix.to_owned(),
        })
    }

    /// The prefix every template of this family starts with.
    #[must_use]
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// The database name of the template with fingerprint `hash`: the prefix
    /// and 16 lowercase hex digits. The digits keep the name from parsing as
    /// a clone name (`<prefix>_<uuid>`), so the untracked-leftover sweep
    /// cannot take a template.
    #[must_use]
    pub fn template_name(&self, hash: u64) -> String {
        format!("{}{hash:016x}", self.prefix)
    }

    /// Whether `name` is a template of this family: the prefix and exactly
    /// 16 lowercase hex digits. Stricter than a `LIKE` on the prefix, so a
    /// family whose prefix extends another's does not reach into it.
    fn owns(&self, name: &str) -> bool {
        name.strip_prefix(self.prefix.as_str()).is_some_and(|hash| {
            hash.len() == TEMPLATE_HASH_DIGITS
                && hash
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
    }
}

/// Why [`TemplateFamily::new`] refused a prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemplateFamilyError {
    Empty,
    /// More than 47 bytes: the prefix plus 16 hex digits would not fit a
    /// 63-byte database name.
    TooLong {
        len: usize,
    },
    /// Not a lowercase ASCII letter, a digit or `_`.
    InvalidCharacter {
        found: char,
    },
    MustStartWithLetter,
    /// The trailing `_` keeps the prefix a whole name segment: the hash
    /// never fuses with the last word of it.
    MustEndWithUnderscore,
    /// Equal to, prefixed by, or a prefix of a family `ensure_template`
    /// collects (or of the staging prefix of a build).
    ReservedOverlap {
        reserved: &'static str,
    },
}

impl fmt::Display for TemplateFamilyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("template family prefix is empty"),
            Self::TooLong { len } => write!(
                f,
                "template family prefix is {len} bytes; at most {MAX_FAMILY_PREFIX_BYTES} fit \
                 beside the {TEMPLATE_HASH_DIGITS} hash digits in a 63-byte database name"
            ),
            Self::InvalidCharacter { found } => write!(
                f,
                "template family prefix holds {found:?}; only lowercase ASCII letters, digits and `_` are allowed"
            ),
            Self::MustStartWithLetter => {
                f.write_str("template family prefix must start with a lowercase ASCII letter")
            }
            Self::MustEndWithUnderscore => f.write_str("template family prefix must end with `_`"),
            Self::ReservedOverlap { reserved } => write!(
                f,
                "template family prefix overlaps the built-in prefix `{reserved}`: it must be neither equal to, prefixed by, nor a prefix of it"
            ),
        }
    }
}

impl std::error::Error for TemplateFamilyError {}

/// Ensure a pre-migrated template database exists.
///
/// Serialized by a session-scoped advisory lock on the admin DB. `build`
/// runs only when `template` did not already exist, against a
/// max-one-connection pool to a staging database. The template name becomes
/// visible only after the build succeeds, so failed or interrupted builds
/// cannot be reused as complete templates.
///
/// A success then drops the idle templates of `template`'s built-in family
/// (`proxima_tmpl_{core,split}_`) other than `template`; a name in no
/// built-in family collects nothing. A host names its templates with
/// [`ensure_template_in`] instead, so that they are collected too.
///
/// # Errors
///
/// Returns admin connection/query errors, template creation errors,
/// template connection errors, or errors returned by `build`.
pub async fn ensure_template<F, Fut>(template: &str, build: F) -> Result<(), sqlx::Error>
where
    F: FnOnce(PgPool) -> Fut,
    Fut: Future<Output = Result<(), sqlx::Error>>,
{
    ensure_template_collecting(template, None, build)
        .await?
        .release()
        .await
}

/// [`ensure_template`] for the template `family` names `hash`:
/// `<prefix><hash:016x>`, returned inside a [`TemplateLease`]. Builds it at
/// most once per server; after a success the idle templates of `family` other
/// than this one are dropped, and no other family's.
///
/// The lease keeps the template from being collected until the caller has
/// cloned it ([`SplitRoleDb::from_template`]) and released the lease.
///
/// A template is a function of what built it, so the host derives `hash` from
/// everything `build` consumes (`proxima::testkit::HostTemplate` does this
/// for migrators) and a changed input is a new template. The old one is
/// dropped once nothing is connected to it; two checkouts with different
/// hashes in one family evict each other's idle template, and the loser
/// rebuilds.
///
/// ```compile_fail,E0308
/// // The family is a validated type, never a raw prefix.
/// let _ = proxima_pg_testkit::ensure_template_in("proxima_tmpl_core_", 1, |_| async { Ok(()) });
/// ```
///
/// # Errors
///
/// As [`ensure_template`].
pub async fn ensure_template_in<F, Fut>(
    family: &TemplateFamily,
    hash: u64,
    build: F,
) -> Result<TemplateLease, sqlx::Error>
where
    F: FnOnce(PgPool) -> Fut,
    Fut: Future<Output = Result<(), sqlx::Error>>,
{
    ensure_template_collecting(&family.template_name(hash), Some(family), build).await
}

/// A hold on a template database, from the call that ensured it until its
/// clone exists.
///
/// The collection that [`ensure_template_in`] and [`drop_stale_templates_in`]
/// run drops every idle template of a family but one, so a template that was
/// just ensured is a candidate until it is cloned. The lease is a shared
/// session advisory lock on the template, taken before the template is looked
/// for and kept on the admin connection stored here. Collection takes the same
/// key exclusively and skips a template it cannot lock; it drops without
/// `FORCE`, so a session that connected after its idle check keeps the
/// database too.
///
/// [`Self::release`] gives the lock up at once. Dropping the lease closes the
/// connection, and Postgres releases the lock when it notices.
///
/// Like the build lock, the lease is an advisory lock in the admin database:
/// a collection run through an admin URL that names another database on the
/// same server does not see it.
///
/// ```compile_fail,E0451
/// // A lease exists only because a template was ensured: none is forged.
/// let _ = proxima_pg_testkit::TemplateLease { name: String::new(), conn: todo!() };
/// ```
#[derive(Debug)]
#[must_use = "the template may be collected as soon as the lease is gone"]
pub struct TemplateLease {
    name: String,
    conn: PgConnection,
}

impl TemplateLease {
    /// The leased template database.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Give the lease up: the template is a collection candidate again.
    ///
    /// # Errors
    ///
    /// Returns the unlock or close error of the admin connection.
    pub async fn release(mut self) -> Result<(), sqlx::Error> {
        let unlocked = sqlx::query("SELECT pg_advisory_unlock_shared($1)")
            .bind(lease_key(&self.name))
            .execute(&mut self.conn)
            .await;
        let closed = self.conn.close().await;
        unlocked?;
        closed
    }
}

/// The advisory-lock key of a template's lease: distinct from the build lock
/// (`advisory_lock_key(template)`), which only serializes builds.
fn lease_key(template: &str) -> i64 {
    advisory_lock_key(&format!("_proxima_test.lease:{template}"))
}

/// Drop the idle templates of `family` other than the one `keep` names,
/// without building: [`ensure_template_in`]'s collection, for a process that
/// wants it alone. Never touches a database outside `family`.
///
/// # Errors
///
/// Returns admin connection or catalog errors. Individual drop failures
/// are logged and skipped.
pub async fn drop_stale_templates_in(
    family: &TemplateFamily,
    keep: u64,
) -> Result<usize, sqlx::Error> {
    let mut conn = connect_admin().await?;
    let dropped = drop_stale_in_family_on(&mut conn, family, &family.template_name(keep)).await?;
    conn.close().await?;
    Ok(dropped)
}

/// The body of [`ensure_template`] and [`ensure_template_in`]. `family` is
/// `None` for a template named by hand, whose built-in family (if any) is
/// read off its name. The returned lease is held on `conn`.
async fn ensure_template_collecting<F, Fut>(
    template: &str,
    family: Option<&TemplateFamily>,
    build: F,
) -> Result<TemplateLease, sqlx::Error>
where
    F: FnOnce(PgPool) -> Fut,
    Fut: Future<Output = Result<(), sqlx::Error>>,
{
    let mut conn = connect_admin().await?;
    let lock_key = advisory_lock_key(template);

    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(lock_key)
        .execute(&mut conn)
        .await?;

    let result = async {
        // The lease comes before the look: a collection that is dropping this
        // very template holds the key until it is gone, and then the template
        // is built again instead of being found and lost.
        sqlx::query("SELECT pg_advisory_lock_shared($1)")
            .bind(lease_key(template))
            .execute(&mut conn)
            .await?;
        let exists = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname = $1)",
        )
        .bind(template)
        .fetch_one(&mut conn)
        .await?;

        if exists {
            return Ok(());
        }

        let staging = unique_db_name("proxima_tmpl_build");
        sqlx::raw_sql(AssertSqlSafe(format!(
            "CREATE DATABASE {}",
            quoted_ident(&staging)
        )))
        .execute(&mut conn)
        .await?;
        record_new_db(&mut conn, &staging).await?;

        let build_result: Result<(), sqlx::Error> = async {
            let pool = PgPoolOptions::new()
                .max_connections(1)
                .connect(&db_url(&staging))
                .await?;
            let outcome = build(pool.clone()).await;
            pool.close().await;
            outcome?;
            sqlx::raw_sql(AssertSqlSafe(format!(
                "ALTER DATABASE {} RENAME TO {}",
                quoted_ident(&staging),
                quoted_ident(template)
            )))
            .execute(&mut conn)
            .await?;
            unrecord_db(&mut conn, &staging).await?;
            Ok(())
        }
        .await;
        if build_result.is_err()
            && let Err(error) = drop_db_on(&mut conn, &staging).await
        {
            tracing::warn!(database = staging, %error, "failed to clean up incomplete test template");
        }
        build_result
    }
    .await;

    if result.is_ok() {
        let collected = match family {
            Some(family) => drop_stale_in_family_on(&mut conn, family, template).await,
            None => drop_stale_templates_on(&mut conn, template).await,
        };
        if let Err(error) = collected {
            tracing::warn!(template, %error, "failed to drop stale test templates");
        }
    }

    let unlocked = sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(lock_key)
        .execute(&mut conn)
        .await
        .map(|_| ());

    match result.and(unlocked) {
        Ok(()) => Ok(TemplateLease {
            name: template.to_owned(),
            conn,
        }),
        Err(error) => {
            // Closing the connection also gives up the lease.
            let _ = conn.close().await;
            Err(error)
        }
    }
}

/// Drop idle `proxima_tmpl_{core,split}_*` databases of `keep`'s
/// family other than `keep`.
///
/// No-op when `keep` is not a core/split template name. Called from
/// [`ensure_template`]; exposed so a process can GC without rebuilding.
///
/// # Errors
///
/// Returns admin connection or catalog errors. Individual drop failures
/// are logged and skipped.
pub async fn drop_stale_templates(keep: &str) -> Result<usize, sqlx::Error> {
    let mut conn = connect_admin().await?;
    let dropped = drop_stale_templates_on(&mut conn, keep).await?;
    conn.close().await?;
    Ok(dropped)
}

/// Create a unique database cloned from `template`.
///
/// Retries the transient `55006` source-template-accessed error raised
/// when concurrent test processes clone the same template.
///
/// # Errors
///
/// Returns admin connection errors, non-retryable `CREATE DATABASE`
/// errors, or the last retryable error after retries are exhausted.
pub async fn create_db_from_template(prefix: &str, template: &str) -> Result<String, sqlx::Error> {
    let name = unique_db_name(prefix);
    create_named_db_from_template(&name, template).await?;
    Ok(name)
}

/// [`create_db_from_template`] under a caller-chosen `name`: the drop-in
/// for [`create_db`] when the caller already holds the name.
///
/// # Errors
///
/// As [`create_db_from_template`].
pub async fn create_named_db_from_template(name: &str, template: &str) -> Result<(), sqlx::Error> {
    let mut conn = connect_admin().await?;
    let statement = format!(
        "CREATE DATABASE {} TEMPLATE {}",
        quoted_ident(name),
        quoted_ident(template)
    );
    let mut last_error = None;

    for _ in 0..DROP_RETRIES {
        match sqlx::raw_sql(AssertSqlSafe(statement.clone()))
            .execute(&mut conn)
            .await
        {
            Ok(_) => {
                record_new_db(&mut conn, name).await?;
                conn.close().await?;
                return Ok(());
            }
            Err(err) if is_sqlstate(&err, SQLSTATE_DATABASE_ACCESSED) => {
                last_error = Some(err);
                tokio::time::sleep(DROP_RETRY_DELAY).await;
            }
            Err(err) => return Err(err),
        }
    }

    let _ = conn.close().await;
    Err(last_error.unwrap_or_else(|| {
        sqlx::Error::Protocol(
            "create_db_from_template exhausted retries with no recorded error".into(),
        )
    }))
}

/// Drop one test database, terminating leftover backends.
///
/// Uses `DROP DATABASE … WITH (FORCE)` (Postgres 13+). A missing database
/// is success. The tracking row is removed whether or not the catalog
/// still had the name.
///
/// # Errors
///
/// Returns admin connection errors, non-`IF EXISTS` drop errors, or
/// connection close errors.
pub async fn drop_db(name: &str) -> Result<(), sqlx::Error> {
    let mut conn = connect_admin().await?;
    drop_db_on(&mut conn, name).await?;
    conn.close().await?;
    Ok(())
}

/// Sweep clones left by earlier test processes.
///
/// Called automatically from the first admin operation. Safe to invoke
/// by hand. Idle `psql` sessions keep a database (it is visible in
/// `pg_stat_activity`). Tracked rows younger than five minutes
/// are kept even with no backend: nextest starts many binaries against
/// one Postgres, and `created_at < this_process_start` would otherwise
/// `DROP DATABASE` a sibling's clone in the window between
/// [`create_db`] and its first connection.
///
/// # Errors
///
/// Returns admin connection or catalog errors. Individual drop failures
/// are logged and skipped.
pub async fn sweep_stale_test_dbs() -> Result<usize, sqlx::Error> {
    let mut conn = connect_admin().await?;
    let dropped = sweep_stale_on(&mut conn).await?;
    conn.close().await?;
    Ok(dropped)
}

async fn connect_admin() -> Result<PgConnection, sqlx::Error> {
    let mut conn = PgConnection::connect(&try_admin_url()?).await?;
    maybe_sweep(&mut conn).await?;
    Ok(conn)
}

async fn maybe_sweep(conn: &mut PgConnection) -> Result<(), sqlx::Error> {
    if SWEEP_DONE.load(Ordering::Acquire) {
        return Ok(());
    }
    let dropped = sweep_stale_on(conn).await?;
    if dropped > 0 {
        tracing::info!(dropped, "swept leftover test databases");
    }
    SWEEP_DONE.store(true, Ordering::Release);
    Ok(())
}

async fn process_start(conn: &mut PgConnection) -> Result<OffsetDateTime, sqlx::Error> {
    if let Some(start) = PROCESS_START.get() {
        return Ok(*start);
    }
    let now: OffsetDateTime = sqlx::query_scalar("SELECT now()")
        .fetch_one(&mut *conn)
        .await?;
    Ok(*PROCESS_START.get_or_init(|| now))
}

async fn ensure_catalog(conn: &mut PgConnection) -> Result<(), sqlx::Error> {
    if CATALOG_READY.load(Ordering::Acquire) {
        return Ok(());
    }
    let lock_key = advisory_lock_key("_proxima_test.catalog");
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(lock_key)
        .execute(&mut *conn)
        .await?;
    let result: Result<(), sqlx::Error> = async {
        ensure_search_role(conn).await?;
        sqlx::query("CREATE SCHEMA IF NOT EXISTS _proxima_test")
            .execute(&mut *conn)
            .await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS _proxima_test.databases (
                db_name text PRIMARY KEY,
                created_at timestamptz NOT NULL DEFAULT now()
            )",
        )
        .execute(&mut *conn)
        .await?;
        Ok(())
    }
    .await;
    let unlock = sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(lock_key)
        .execute(&mut *conn)
        .await;
    result?;
    unlock?;
    CATALOG_READY.store(true, Ordering::Release);
    Ok(())
}

async fn ensure_search_role(conn: &mut PgConnection) -> Result<(), sqlx::Error> {
    sqlx::raw_sql(
        "DO $$ BEGIN
           IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'proxima_search') THEN
             CREATE ROLE proxima_search NOLOGIN NOSUPERUSER NOBYPASSRLS
               NOCREATEDB NOCREATEROLE NOINHERIT;
           ELSE
             ALTER ROLE proxima_search NOLOGIN NOSUPERUSER NOBYPASSRLS
               NOCREATEDB NOCREATEROLE NOINHERIT;
           END IF;
         END $$",
    )
    .execute(conn)
    .await?;
    Ok(())
}

async fn record_db(conn: &mut PgConnection, name: &str) -> Result<(), sqlx::Error> {
    ensure_catalog(conn).await?;
    sqlx::query(
        "INSERT INTO _proxima_test.databases (db_name) VALUES ($1)
         ON CONFLICT (db_name) DO NOTHING",
    )
    .bind(name)
    .execute(&mut *conn)
    .await?;
    let mut target = PgConnection::connect(&db_url(name)).await?;
    sqlx::query("GRANT USAGE ON SCHEMA public TO proxima_search")
        .execute(&mut target)
        .await?;
    target.close().await?;
    Ok(())
}

/// [`record_db`] for a database this call just created: when it cannot be
/// recorded it is dropped again, so no created database is left outside the
/// catalog the sweep reads.
async fn record_new_db(conn: &mut PgConnection, name: &str) -> Result<(), sqlx::Error> {
    if let Err(error) = record_db(conn, name).await {
        let _ = drop_db_on(conn, name).await;
        return Err(error);
    }
    Ok(())
}

async fn unrecord_db(conn: &mut PgConnection, name: &str) -> Result<(), sqlx::Error> {
    ensure_catalog(conn).await?;
    sqlx::query("DELETE FROM _proxima_test.databases WHERE db_name = $1")
        .bind(name)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

async fn drop_db_on(conn: &mut PgConnection, name: &str) -> Result<(), sqlx::Error> {
    let quoted_name = quoted_ident(name);
    match sqlx::raw_sql(AssertSqlSafe(format!(
        "DROP DATABASE IF EXISTS {quoted_name} WITH (FORCE)"
    )))
    .execute(&mut *conn)
    .await
    {
        Ok(_) => {}
        Err(err) if is_sqlstate(&err, SQLSTATE_UNDEFINED_DATABASE) => {}
        Err(err) => return Err(err),
    }
    unrecord_db(conn, name).await?;
    Ok(())
}

async fn sweep_stale_on(conn: &mut PgConnection) -> Result<usize, sqlx::Error> {
    ensure_catalog(conn).await?;
    let start = process_start(conn).await?;
    let now: OffsetDateTime = sqlx::query_scalar("SELECT now()")
        .fetch_one(&mut *conn)
        .await?;
    let tracked_cutoff = now - UNTRACKED_GRACE;
    let cutoff = start - UNTRACKED_GRACE;
    let tracked: Vec<String> = sqlx::query_scalar(
        "SELECT d.db_name FROM _proxima_test.databases d
         WHERE d.created_at < $1
           AND NOT EXISTS (
             SELECT 1 FROM pg_stat_activity a WHERE a.datname = d.db_name
           )",
    )
    .bind(tracked_cutoff)
    .fetch_all(&mut *conn)
    .await?;
    let untracked: Vec<String> = sqlx::query_scalar(
        "SELECT datname FROM pg_database
         WHERE datistemplate = false
           AND datname <> current_database()
           AND (
             datname LIKE 'proxima\\_%' ESCAPE '\\'
             OR datname LIKE 'pub\\_%' ESCAPE '\\'
           )
           AND NOT EXISTS (
             SELECT 1 FROM pg_stat_activity a WHERE a.datname = pg_database.datname
           )",
    )
    .fetch_all(&mut *conn)
    .await?;

    let mut names = tracked;
    for name in untracked {
        if name_is_stale_clone(&name, cutoff) && !names.contains(&name) {
            names.push(name);
        }
    }

    let mut dropped = 0;
    for name in names {
        match drop_db_on(conn, &name).await {
            Ok(()) => dropped += 1,
            Err(error) => {
                tracing::warn!(database = name, %error, "sweep failed to drop leftover test database");
            }
        }
    }
    Ok(dropped)
}

async fn drop_stale_templates_on(
    conn: &mut PgConnection,
    keep: &str,
) -> Result<usize, sqlx::Error> {
    let Some(family) = template_family(keep) else {
        return Ok(0);
    };
    drop_idle_templates_on(conn, family, |_| true, keep).await
}

async fn drop_stale_in_family_on(
    conn: &mut PgConnection,
    family: &TemplateFamily,
    keep: &str,
) -> Result<usize, sqlx::Error> {
    drop_idle_templates_on(conn, family.prefix(), |name| family.owns(name), keep).await
}

/// Drop every idle database starting with `prefix` that `owned` accepts,
/// except `keep`. The `LIKE` narrows the catalog scan; `owned` is the
/// family's own definition of a member.
async fn drop_idle_templates_on(
    conn: &mut PgConnection,
    prefix: &str,
    owned: impl Fn(&str) -> bool,
    keep: &str,
) -> Result<usize, sqlx::Error> {
    let pattern = format!("{}%", prefix.replace('_', r"\_"));
    let stale: Vec<String> = sqlx::query_scalar(
        "SELECT datname FROM pg_database
         WHERE datname LIKE $1 ESCAPE '\\'
           AND datname <> $2
           AND datistemplate = false
           AND NOT EXISTS (
             SELECT 1 FROM pg_stat_activity a WHERE a.datname = pg_database.datname
           )",
    )
    .bind(&pattern)
    .bind(keep)
    .fetch_all(&mut *conn)
    .await?;

    let mut dropped = 0;
    for name in stale.into_iter().filter(|name| owned(name)) {
        match drop_unleased_on(conn, &name).await {
            Ok(true) => dropped += 1,
            // Leased or in use: someone is about to clone it, or is using it.
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(database = name, %error, "failed to drop stale test template");
            }
        }
    }
    if dropped > 0 {
        tracing::info!(keep, dropped, "dropped stale test templates");
    }
    Ok(dropped)
}

/// Drop the template `name` unless it is leased ([`TemplateLease`]) or in
/// use. `false` is a skip, not a failure.
async fn drop_unleased_on(conn: &mut PgConnection, name: &str) -> Result<bool, sqlx::Error> {
    let key = lease_key(name);
    let free: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
        .bind(key)
        .fetch_one(&mut *conn)
        .await?;
    if !free {
        return Ok(false);
    }
    let dropped = drop_unused_db_on(conn, name).await;
    let unlocked = sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(key)
        .execute(&mut *conn)
        .await;
    let dropped = dropped?;
    unlocked?;
    Ok(dropped)
}

/// `DROP DATABASE` without `FORCE`: a database somebody is connected to is
/// kept (`false`), also when they connected after the caller looked.
async fn drop_unused_db_on(conn: &mut PgConnection, name: &str) -> Result<bool, sqlx::Error> {
    // SQL-POLICY: fixed-fragment — a quoted database identifier; DDL takes no bind parameter.
    match sqlx::raw_sql(AssertSqlSafe(format!(
        "DROP DATABASE IF EXISTS {}",
        quoted_ident(name)
    )))
    .execute(&mut *conn)
    .await
    {
        Ok(_) => {}
        Err(err) if is_sqlstate(&err, SQLSTATE_DATABASE_ACCESSED) => return Ok(false),
        Err(err) if is_sqlstate(&err, SQLSTATE_UNDEFINED_DATABASE) => {}
        Err(err) => return Err(err),
    }
    unrecord_db(conn, name).await?;
    Ok(true)
}

fn template_family(name: &str) -> Option<&'static str> {
    BUILTIN_FAMILIES
        .into_iter()
        .find(|prefix| name.starts_with(prefix))
}

fn name_is_stale_clone(name: &str, older_than: OffsetDateTime) -> bool {
    let Some(suffix) = name.rsplit('_').next() else {
        return false;
    };
    let Ok(uuid) = Uuid::parse_str(suffix) else {
        return false;
    };
    let Some(timestamp) = uuid.get_timestamp() else {
        return false;
    };
    let (seconds, nanos) = timestamp.to_unix();
    let Ok(seconds) = i64::try_from(seconds) else {
        return false;
    };
    let Ok(created) = OffsetDateTime::from_unix_timestamp(seconds) else {
        return false;
    };
    let created = created + time::Duration::nanoseconds(i64::from(nanos));
    created < older_than
}

fn quoted_ident(input: &str) -> String {
    format!("\"{}\"", input.replace('"', "\"\""))
}

fn sql_literal(input: &str) -> String {
    format!("'{}'", input.replace('\'', "''"))
}

fn advisory_lock_key(input: &str) -> i64 {
    let hash = fnv1a64(input.as_bytes());
    i64::from_be_bytes(hash.to_be_bytes())
}

fn is_sqlstate(err: &sqlx::Error, expected: &str) -> bool {
    match err {
        sqlx::Error::Database(db_err) => db_err.code().is_some_and(|code| code == expected),
        _ => false,
    }
}

/// The test server's connection slots and how many are in use.
///
/// Every test process nextest runs at once shares one server's
/// `max_connections`, less the slots reserved for superusers. A test whose
/// pool cannot open a connection sees `PoolTimedOut` after the acquire
/// timeout, because the pool retries a refused connect (SQLSTATE 53300)
/// until then. [`describe_connection_error`] attaches this to such an error
/// so the failure names the resource instead of only the timeout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ConnectionBudget {
    /// `max_connections`.
    pub max_connections: i64,
    /// `superuser_reserved_connections`.
    pub superuser_reserved: i64,
    /// `reserved_connections` (Postgres 16+, otherwise 0).
    pub reserved: i64,
    /// Client backends connected to the server, every database included.
    pub client_backends: i64,
    /// The client backends of roles that are not superusers: the ones the
    /// reserved slots are not kept for.
    pub non_superuser_backends: i64,
}

impl fmt::Display for ConnectionBudget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "max_connections={} (superuser_reserved_connections={}, reserved_connections={}); \
             {} client backends connected, {} of them non-superuser",
            self.max_connections,
            self.superuser_reserved,
            self.reserved,
            self.client_backends,
            self.non_superuser_backends
        )
    }
}

/// Read the test server's [`ConnectionBudget`] over one admin connection.
///
/// # Errors
///
/// Returns the admin URL's configuration error, or the connection or query
/// error.
pub async fn connection_budget() -> Result<ConnectionBudget, sqlx::Error> {
    let mut conn = PgConnection::connect(&try_admin_url()?).await?;
    let (max_connections, superuser_reserved, reserved, client_backends, non_superuser_backends) =
        sqlx::query_as::<_, (i64, i64, i64, i64, i64)>(
            "SELECT current_setting('max_connections')::bigint,
                    current_setting('superuser_reserved_connections')::bigint,
                    coalesce(current_setting('reserved_connections', true), '0')::bigint,
                    count(*)::bigint,
                    count(*) FILTER (WHERE NOT coalesce(r.rolsuper, false))::bigint
               FROM pg_stat_activity AS a LEFT JOIN pg_roles AS r ON r.oid = a.usesysid
              WHERE a.backend_type = 'client backend'",
        )
        .fetch_one(&mut conn)
        .await?;
    conn.close().await?;
    Ok(ConnectionBudget {
        max_connections,
        superuser_reserved,
        reserved,
        client_backends,
        non_superuser_backends,
    })
}

/// Whether an error's text reads as the server or a pool running out of
/// connections: a pool timeout, or a connect refused for lack of slots.
#[must_use]
pub fn is_connection_exhaustion(message: &str) -> bool {
    [
        "pool timed out while waiting for an open connection",
        "too many clients already",
        "remaining connection slots are reserved",
        "too many connections for",
        "53300",
    ]
    .iter()
    .any(|needle| message.contains(needle))
}

/// `context: error`, followed by the server's [`ConnectionBudget`] when the
/// error reads as connection exhaustion ([`is_connection_exhaustion`]).
///
/// For a fixture's panic message: a boot that timed out on its pool then
/// says how full the server was, which separates a server out of
/// connections from an operation that stalled for another reason. The
/// budget is read over a superuser admin connection, which the reserved
/// slots normally still admit; when even that fails, the message says so.
pub async fn describe_connection_error(context: &str, error: &dyn fmt::Display) -> String {
    let message = format!("{context}: {error}");
    if !is_connection_exhaustion(&message) {
        return message;
    }
    match connection_budget().await {
        Ok(budget) => format!("{message}; test server: {budget}"),
        Err(budget_error) => {
            format!("{message}; test server connection budget unavailable: {budget_error}")
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn connection_exhaustion_is_read_from_the_pool_and_server_messages() {
        for message in [
            "runtime connection: Unavailable(\"pool timed out while waiting for an open connection\")",
            "error returned from database: sorry, too many clients already",
            "remaining connection slots are reserved for roles with the SUPERUSER attribute",
            "too many connections for database \"proxima_test_x\"",
        ] {
            assert!(super::is_connection_exhaustion(message), "{message}");
        }
        for message in [
            "RLS catalog guard refused pool: runtime role proxima_test_runtime is a superuser",
            "scope not registered",
        ] {
            assert!(!super::is_connection_exhaustion(message), "{message}");
        }
    }

    use time::OffsetDateTime;

    use super::{
        ADMIN_URL_UNSET, ADMIN_URL_VAR, BUILTIN_FAMILIES, MAX_FAMILY_PREFIX_BYTES,
        RESERVED_PREFIXES, SPLIT_ROLE_PASSWORD, TemplateFamily, TemplateFamilyError, connect_admin,
        create_db, db_url, db_url_from_admin, drop_unused_db_on, name_is_stale_clone, quoted_ident,
        redacted_url, role_url_from_admin, template_family, trimmed_url, unique_db_name,
    };

    /// Provisioning twice leaves the fixture roles with one, fixed SCRAM
    /// verifier (a re-salted verifier breaks a pooler's cached SCRAM keys),
    /// and the fixture password still logs in against it.
    #[tokio::test]
    async fn split_role_provisioning_keeps_one_scram_verifier() {
        use sqlx::Connection as _;
        if super::admin_url_or_skip().is_none() {
            return;
        }
        let db = unique_db_name("scram_verifier");
        create_db(&db).await.expect("fixture database");
        let verifier = async {
            let mut admin = connect_admin().await.expect("admin");
            sqlx::query_scalar::<_, String>(
                "SELECT rolpassword FROM pg_authid WHERE rolname = 'proxima_test_runtime'",
            )
            .fetch_one(&mut admin)
            .await
            .expect("runtime role verifier")
        };
        let (runtime_url, _) = super::split_role_urls(&db).await.expect("provisioned");
        let first = verifier.await;
        let verifier = async {
            let mut admin = connect_admin().await.expect("admin");
            sqlx::query_scalar::<_, String>(
                "SELECT rolpassword FROM pg_authid WHERE rolname = 'proxima_test_runtime'",
            )
            .fetch_one(&mut admin)
            .await
            .expect("runtime role verifier")
        };
        super::split_role_urls(&db)
            .await
            .expect("provisioned again");
        let second = verifier.await;
        assert_eq!(first, super::SPLIT_ROLE_SCRAM_VERIFIER);
        assert_eq!(first, second, "provisioning again does not re-salt");
        sqlx::PgConnection::connect(&runtime_url)
            .await
            .expect("the fixture password matches the verifier")
            .close()
            .await
            .expect("close");
        super::drop_db(&db).await.expect("drop fixture database");
    }

    #[test]
    fn invalid_admin_url_error_contains_no_credentials() {
        let result = db_url_from_admin("postgres://user:private-secret@[broken", "isolated_test");
        let error = result.expect_err("invalid IPv6 address");
        assert!(!error.to_string().contains("private-secret"));
    }

    #[test]
    fn quoted_ident_escapes_embedded_quotes() {
        assert_eq!(quoted_ident("a\"b"), "\"a\"\"b\"");
    }

    #[test]
    fn redacted_url_hides_the_password() {
        let parsed =
            url::Url::parse("postgres://user:secret@localhost/isolated_test").expect("test URL");
        let redacted = redacted_url(parsed);
        assert_eq!(redacted.password(), Some("****"));
        assert_eq!(redacted.username(), "user");
        assert!(!redacted.as_str().contains("secret"));
    }

    #[test]
    fn redacted_url_hides_a_password_in_the_query() {
        let parsed = url::Url::parse(
            "postgres://localhost/isolated_test?sslmode=disable&password=private-secret&user=u",
        )
        .expect("test URL");
        let redacted = redacted_url(parsed);
        assert!(!redacted.as_str().contains("private-secret"));
        let pairs: Vec<(String, String)> = redacted
            .query_pairs()
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        assert_eq!(
            pairs,
            [
                ("sslmode".to_owned(), "disable".to_owned()),
                ("password".to_owned(), "****".to_owned()),
                ("user".to_owned(), "u".to_owned()),
            ],
            "only the password value changes"
        );
    }

    #[test]
    fn a_role_url_drops_the_admin_credentials_wherever_they_are() {
        let admin = "postgres://admin:authority-secret@localhost:5433/postgres\
                     ?sslmode=disable&user=query-admin&password=private-secret&dbname=other";
        let url = role_url_from_admin(admin, "target", "proxima_test_runtime").expect("role URL");
        assert_eq!(url.username(), "proxima_test_runtime");
        assert_eq!(url.password(), Some(SPLIT_ROLE_PASSWORD));
        assert_eq!(
            (url.host_str(), url.port()),
            (Some("localhost"), Some(5433))
        );
        let pairs: Vec<(String, String)> = url
            .query_pairs()
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        assert_eq!(
            pairs,
            [
                ("sslmode".to_owned(), "disable".to_owned()),
                ("dbname".to_owned(), "target".to_owned()),
            ],
            "no user or password override survives; the target database does"
        );
        let text = url.to_string();
        // The failure names the credential, never its value or the URL.
        for (credential, value) in [
            ("the admin password", "authority-secret"),
            ("the query password", "private-secret"),
            ("the query user", "query-admin"),
        ] {
            assert!(
                !text.contains(value),
                "{credential} leaked into the role URL"
            );
        }
    }

    /// The window a lease does not cover is a session that connects after the
    /// collector looked: `DROP DATABASE` without `FORCE` refuses it, and the
    /// refusal is a skip.
    #[tokio::test]
    async fn dropping_a_database_somebody_is_connected_to_is_a_skip() {
        use sqlx::Connection;

        let name = unique_db_name("tfbusy_unit");
        create_db(&name).await.expect("create");
        let session = sqlx::PgConnection::connect(&db_url(&name))
            .await
            .expect("a session on the database");
        let mut admin = connect_admin().await.expect("admin");
        let busy = drop_unused_db_on(&mut admin, &name).await;
        session.close().await.expect("close the session");
        let idle = drop_unused_db_on(&mut admin, &name).await;
        admin.close().await.expect("close admin");

        assert!(!busy.expect("a busy drop is not an error"), "kept");
        assert!(idle.expect("an idle drop"), "dropped once nobody is on it");
    }

    /// A database that was created and cannot be recorded (here: it accepts no
    /// connection, so the grant on it fails) is dropped again.
    #[tokio::test]
    async fn a_created_database_that_cannot_be_recorded_is_dropped() {
        use sqlx::Connection;

        let name = unique_db_name("record_fails");
        let mut admin = connect_admin().await.expect("admin");
        // SQL-POLICY: fixed-fragment — a quoted generated database identifier.
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "CREATE DATABASE {name} ALLOW_CONNECTIONS false",
            name = quoted_ident(&name)
        )))
        .execute(&mut admin)
        .await
        .expect("create");

        let recorded = super::record_new_db(&mut admin, &name).await;
        let left: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname = $1)")
                .bind(&name)
                .fetch_one(&mut admin)
                .await
                .expect("exists");
        admin.close().await.expect("close admin");

        recorded.expect_err("a database that accepts no connection cannot be recorded");
        assert!(!left, "the unrecorded database was dropped");
    }

    #[tokio::test]
    async fn candidate_ownership_and_execution_survive_split_role_provisioning() {
        use sqlx::Connection;

        let Some(_) = super::admin_url_or_skip() else {
            return;
        };
        let name = unique_db_name("proxima_search_role");
        create_db(&name).await.expect("create");
        let guard = super::DbGuard::adopt(name);
        let (runtime_url, _) = super::split_role_urls(guard.name()).await.expect("roles");
        let mut admin = sqlx::PgConnection::connect(&db_url(guard.name()))
            .await
            .expect("admin");
        sqlx::raw_sql(
            "CREATE SCHEMA proxima_core;
             GRANT USAGE, CREATE ON SCHEMA proxima_core TO proxima_search;
             SET ROLE proxima_search;
             CREATE FUNCTION proxima_core.candidate_probe() RETURNS integer
               LANGUAGE sql SECURITY DEFINER SET search_path = pg_catalog AS 'SELECT 7';
             REVOKE ALL ON FUNCTION proxima_core.candidate_probe() FROM PUBLIC;
             RESET ROLE;
             REVOKE CREATE ON SCHEMA proxima_core FROM proxima_search",
        )
        .execute(&mut admin)
        .await
        .expect("candidate creation");
        let default_execute: bool = sqlx::query_scalar(
            "SELECT has_function_privilege($1, 'proxima_core.candidate_probe()', 'EXECUTE')",
        )
        .bind(super::SPLIT_RUNTIME_ROLE)
        .fetch_one(&mut admin)
        .await
        .expect("default execution grant");
        assert!(default_execute);
        sqlx::raw_sql(
            "REVOKE EXECUTE ON FUNCTION proxima_core.candidate_probe() FROM proxima_test_runtime;
             DROP TABLE _proxima_test.split_roles_ready",
        )
        .execute(&mut admin)
        .await
        .expect("replay preparation");
        super::split_role_urls(guard.name()).await.expect("replay");
        let owner: String = sqlx::query_scalar(
            "SELECT owner_role.rolname FROM pg_proc p
               JOIN pg_roles owner_role ON owner_role.oid = p.proowner
              WHERE p.oid = 'proxima_core.candidate_probe()'::regprocedure",
        )
        .fetch_one(&mut admin)
        .await
        .expect("owner");
        assert_eq!(owner, "proxima_search");
        let attributes: (bool, bool, bool, bool, bool, bool) = sqlx::query_as(
            "SELECT rolcanlogin, rolsuper, rolbypassrls, rolcreatedb, rolcreaterole, rolinherit
               FROM pg_roles WHERE rolname = 'proxima_search'",
        )
        .fetch_one(&mut admin)
        .await
        .expect("search attributes");
        assert_eq!(attributes, (false, false, false, false, false, false));
        let membership: (bool, bool, bool) = sqlx::query_as(
            "SELECT m.admin_option, m.inherit_option, m.set_option
               FROM pg_auth_members m JOIN pg_roles r ON r.oid = m.roleid
               JOIN pg_roles member_role ON member_role.oid = m.member
              WHERE r.rolname = 'proxima_search' AND member_role.rolname = $1",
        )
        .bind(super::SPLIT_PLATFORM_ROLE)
        .fetch_one(&mut admin)
        .await
        .expect("platform membership");
        assert_eq!(membership, (false, false, true));
        let runtime_member: bool =
            sqlx::query_scalar("SELECT pg_has_role($1, 'proxima_search', 'MEMBER')")
                .bind(super::SPLIT_RUNTIME_ROLE)
                .fetch_one(&mut admin)
                .await
                .expect("runtime membership");
        assert!(!runtime_member);
        let mut runtime = sqlx::PgConnection::connect(&runtime_url)
            .await
            .expect("runtime");
        let result: i32 = sqlx::query_scalar("SELECT proxima_core.candidate_probe()")
            .fetch_one(&mut runtime)
            .await
            .expect("explicit execution grant");
        assert_eq!(result, 7);
        runtime.close().await.expect("close runtime");
        admin.close().await.expect("close admin");
    }

    #[test]
    fn a_role_url_from_an_unparsable_admin_url_is_an_error_without_credentials() {
        let error = role_url_from_admin("postgres://u:private-secret@[broken", "t", "r")
            .expect_err("invalid IPv6 address");
        assert!(!error.to_string().contains("private-secret"));
    }

    #[test]
    fn template_family_only_core_and_split() {
        assert_eq!(
            template_family("proxima_tmpl_core_3f94e256ef4f396f"),
            Some("proxima_tmpl_core_")
        );
        assert_eq!(template_family("proxima_tmpl_other_2d01c98992e2f0a0"), None);
        assert_eq!(
            template_family("proxima_tmpl_split_9a41c0d2e6b37f58"),
            Some("proxima_tmpl_split_")
        );
        assert_eq!(template_family("proxima_tmpl_build_abc"), None);
        assert_eq!(template_family("proxima_test_abc"), None);
    }

    #[test]
    fn an_unset_empty_or_blank_admin_url_is_unconfigured() {
        assert_eq!(trimmed_url(None), None);
        assert_eq!(trimmed_url(Some(String::new())), None);
        assert_eq!(trimmed_url(Some(" \t\n ".into())), None);
        assert_eq!(
            trimmed_url(Some("  postgres://u:p@h/db \n".into())).as_deref(),
            Some("postgres://u:p@h/db")
        );
    }

    #[test]
    fn the_unconfigured_message_names_the_variable_and_carries_no_url() {
        assert!(ADMIN_URL_UNSET.contains(ADMIN_URL_VAR));
        assert!(ADMIN_URL_UNSET.contains("no default"));
    }

    fn refusal(prefix: &str) -> TemplateFamilyError {
        TemplateFamily::new(prefix).expect_err("prefix must be refused")
    }

    #[test]
    fn a_family_prefix_is_validated_at_construction() {
        for accepted in ["host_", "a_", "my_host2_tmpl_", "proxima_split_core_"] {
            let family = TemplateFamily::new(accepted).expect(accepted);
            assert_eq!(family.prefix(), accepted);
        }
        let longest = format!("{}_", "a".repeat(MAX_FAMILY_PREFIX_BYTES - 1));
        assert_eq!(longest.len(), 47);
        TemplateFamily::new(&longest).expect("47 bytes fit beside 16 digits in 63");

        assert_eq!(refusal(""), TemplateFamilyError::Empty);
        assert_eq!(
            refusal(&format!("a{longest}")),
            TemplateFamilyError::TooLong { len: 48 }
        );
        assert_eq!(
            refusal("Host_"),
            TemplateFamilyError::InvalidCharacter { found: 'H' }
        );
        assert_eq!(
            refusal("ho-st_"),
            TemplateFamilyError::InvalidCharacter { found: '-' }
        );
        assert_eq!(
            refusal("h\u{00f6}st_"),
            TemplateFamilyError::InvalidCharacter { found: '\u{00f6}' }
        );
        assert_eq!(refusal("1host_"), TemplateFamilyError::MustStartWithLetter);
        assert_eq!(refusal("_host_"), TemplateFamilyError::MustStartWithLetter);
        assert_eq!(refusal("host"), TemplateFamilyError::MustEndWithUnderscore);
        assert_eq!(
            refusal("host_x"),
            TemplateFamilyError::MustEndWithUnderscore
        );
    }

    #[test]
    fn a_family_prefix_never_overlaps_a_reserved_one() {
        for reserved in RESERVED_PREFIXES {
            // Every `_`-terminated prefix of the reserved one (itself last),
            // and an extension of it: equal, a prefix of, prefixed by.
            let extended = format!("{reserved}host_");
            let prefixes = reserved
                .match_indices('_')
                .map(|(at, _)| &reserved[..=at])
                .chain([extended.as_str()]);
            for prefix in prefixes {
                assert!(
                    matches!(
                        TemplateFamily::new(prefix),
                        Err(TemplateFamilyError::ReservedOverlap { .. })
                    ),
                    "{prefix:?} overlaps {reserved:?}"
                );
            }
        }
        // Near misses are other people's families.
        for prefix in ["proxima_tmpl_cor_", "proxima_tmplx_", "proxima_tmpl_coree_"] {
            TemplateFamily::new(prefix).expect(prefix);
        }
    }

    #[test]
    fn a_family_owns_its_own_templates_and_no_database_it_could_confuse_them_with() {
        let family = TemplateFamily::new("host_").expect("family");
        let nested = TemplateFamily::new("host_tmpl_").expect("family");
        let own = family.template_name(0xabc);
        assert_eq!(own, "host_0000000000000abc");
        assert!(family.owns(&own));
        assert!(family.owns(&family.template_name(u64::MAX)));
        // A family whose prefix extends this one is another family, even
        // though `host_%` matches its templates.
        assert!(!family.owns(&nested.template_name(1)));
        assert!(nested.owns(&nested.template_name(1)));
        for other in [
            "host_",
            "host_0000000000000ABC",
            "host_000000000000abc",
            "host_00000000000000abc",
            "host_0000000000000abg",
            "other_0000000000000abc",
            "host_00000000-0000-7000-8000-000000000000",
        ] {
            assert!(!family.owns(other), "{other:?}");
        }
        for builtin in BUILTIN_FAMILIES {
            assert!(!family.owns(&format!("{builtin}0000000000000abc")));
        }
    }

    #[test]
    fn a_template_name_never_parses_as_a_clone_name() {
        // The sweep drops any `proxima_*` database whose last segment is a
        // UUID v7 older than its grace. A template's last segment is 16 hex
        // digits, whatever the hash.
        let far_future = OffsetDateTime::now_utc() + time::Duration::days(3650);
        let hashes = [0, 1, 0xdead_beef_dead_beef, u64::MAX, 0x0192_8f5a_0000_0000];
        let host_families = ["host_", "proxima_split_core_", "a_b_"]
            .map(|prefix| TemplateFamily::new(prefix).expect("family"));
        let names = host_families
            .iter()
            .flat_map(|family| hashes.map(|hash| family.template_name(hash)))
            .chain(
                BUILTIN_FAMILIES
                    .into_iter()
                    .flat_map(|prefix| hashes.map(move |hash| format!("{prefix}{hash:016x}"))),
            );
        for name in names {
            assert!(!name_is_stale_clone(&name, far_future), "{name}");
            assert!(name.len() <= 63, "{name}");
        }
    }
}

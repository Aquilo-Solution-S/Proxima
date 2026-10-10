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
//!   / `proxima_tmpl_code_*`), keeping only the current fingerprint
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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
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
/// The query parameters `sqlx` lets override the authority's credentials.
const QUERY_USER: &str = "user";
const QUERY_PASSWORD: &str = "password";
/// A template database name ends in this many lowercase hex digits (`{hash:016x}`).
const TEMPLATE_HASH_DIGITS: usize = 16;
/// `PostgreSQL` identifiers hold 63 bytes; a family prefix leaves room for the hash.
const MAX_FAMILY_PREFIX_BYTES: usize = 63 - TEMPLATE_HASH_DIGITS;
/// The built-in template families, collected by [`ensure_template`] and
/// [`drop_stale_templates`].
const BUILTIN_FAMILIES: [&str; 3] = [
    "proxima_tmpl_core_",
    "proxima_tmpl_code_",
    "proxima_tmpl_split_",
];
/// Every prefix a [`TemplateFamily`] must keep clear of: the built-in
/// families and the staging databases of a template build.
const RESERVED_PREFIXES: [&str; 4] = [
    BUILTIN_FAMILIES[0],
    BUILTIN_FAMILIES[1],
    BUILTIN_FAMILIES[2],
    "proxima_tmpl_build_",
];
pub const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
pub const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

static PROCESS_START: OnceLock<OffsetDateTime> = OnceLock::new();
static SWEEP_DONE: Mutex<bool> = Mutex::new(false);
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
    if let Err(error) = record_db(&mut conn, name).await {
        let _ = drop_db_on(&mut conn, name).await;
        conn.close().await?;
        return Err(error);
    }
    conn.close().await?;
    Ok(())
}

/// Provision the shared, non-escalating roles used by split-role PG tests and
/// return URLs for the target database.
///
/// The roles are intentionally reusable across disposable databases. Their
/// password is a local fixture value and must never be used outside tests.
/// Existing Proxima objects are transferred to the platform role before the
/// runtime grants are applied.
///
/// # Errors
/// Returns admin, target-database, catalog, or role-attribute errors.
pub async fn split_role_urls(database: &str) -> Result<(String, String), sqlx::Error> {
    split_role_urls_for(database, &[]).await
}

/// [`split_role_urls`] for a database that also holds out-of-tree flavor
/// schemas: objects already in `flavor_schemas` move to the platform role
/// and the runtime role gets the same grants it gets on `proxima_core` and
/// `proxima_code`.
/// Existing public tables whose names start with `_sqlx_migrations` also
/// move to the platform role, with read-only access for the runtime role.
///
/// Only the first call on a database prepares it; a schema the platform
/// role creates later (the usual case: boot migrates as platform) is
/// covered by the platform role's default privileges instead.
///
/// # Errors
/// Returns admin, target-database, catalog, or role-attribute errors, and a
/// protocol error for a schema name that is not a plain lowercase
/// identifier.
#[expect(
    clippy::too_many_lines,
    reason = "isolated split-role fixture provisioning"
)]
pub async fn split_role_urls_for(
    database: &str,
    flavor_schemas: &[&str],
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
    let mut schemas: Vec<&str> = vec!["proxima_core", "proxima_code"];
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
                password = sql_literal(SPLIT_ROLE_PASSWORD),
            );
            // SQL-POLICY: fixed-fragment — generated role identifiers and a local fixture password.
            sqlx::raw_sql(AssertSqlSafe(statement))
                .execute(&mut control)
                .await?;
        }
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
        for extension in ["vector", "btree_gin", "pg_trgm"] {
            let statement = format!(
                "CREATE EXTENSION IF NOT EXISTS {}",
                quoted_ident(extension)
            );
            // SQL-POLICY: fixed-fragment — extension names are closed above.
            sqlx::raw_sql(AssertSqlSafe(statement))
                .execute(&mut target)
                .await?;
        }
        let public_grants = format!(
            "GRANT USAGE ON SCHEMA public TO {runtime}, {platform};
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
               GRANT USAGE, SELECT, UPDATE ON SEQUENCES TO {runtime}",
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
             );
             CREATE TABLE IF NOT EXISTS public._sqlx_migrations_proxima_code (
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
            "DO $$ DECLARE r record; BEGIN
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
                  WHERE n.nspname IN ({schema_list}) LOOP
                 EXECUTE format('ALTER FUNCTION %I.%I(%s) OWNER TO %I',
                                r.nspname, r.proname, r.args, '{SPLIT_PLATFORM_ROLE}');
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
        let attrs: (bool, bool, bool, bool, bool) = sqlx::query_as(
            "SELECT rolsuper, rolbypassrls, rolcreaterole, rolcreatedb, rolinherit
               FROM pg_roles WHERE rolname = $1",
        )
        .bind(SPLIT_PLATFORM_ROLE)
        .fetch_one(&mut target)
        .await?;
        let runtime_attrs: (bool, bool, bool, bool, bool) = sqlx::query_as(
            "SELECT rolsuper, rolbypassrls, rolcreaterole, rolcreatedb, rolinherit
               FROM pg_roles WHERE rolname = $1",
        )
        .bind(SPLIT_RUNTIME_ROLE)
        .fetch_one(&mut target)
        .await?;
        if attrs != (false, false, false, false, false)
            || runtime_attrs != (false, false, false, false, false)
        {
            return Err(sqlx::Error::Protocol("split-role fixture attributes are unsafe".into()));
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
    /// the call here re-asserts the two roles and the database grant and
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
/// [`ensure_template`] and [`drop_stale_templates`] keep their three built-in
/// families (`proxima_tmpl_{core,code,split}_`); [`Self::new`] refuses any
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
/// (`proxima_tmpl_{core,code,split}_`) other than `template`; a name in no
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
        record_db(&mut conn, &staging).await?;

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

/// Drop idle `proxima_tmpl_{core,code,split}_*` databases of `keep`'s
/// family other than `keep`.
///
/// No-op when `keep` is not a core/code/split template name. Called from
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
                if let Err(error) = record_db(&mut conn, name).await {
                    let _ = drop_db_on(&mut conn, name).await;
                    conn.close().await?;
                    return Err(error);
                }
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
    if sweep_is_done() {
        return Ok(());
    }
    let dropped = sweep_stale_on(conn).await?;
    if dropped > 0 {
        tracing::info!(dropped, "swept leftover test databases");
    }
    mark_sweep_done();
    Ok(())
}

fn sweep_is_done() -> bool {
    SWEEP_DONE.lock().is_ok_and(|guard| *guard)
}

fn mark_sweep_done() {
    if let Ok(mut guard) = SWEEP_DONE.lock() {
        *guard = true;
    }
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

async fn record_db(conn: &mut PgConnection, name: &str) -> Result<(), sqlx::Error> {
    ensure_catalog(conn).await?;
    sqlx::query(
        "INSERT INTO _proxima_test.databases (db_name) VALUES ($1)
         ON CONFLICT (db_name) DO NOTHING",
    )
    .bind(name)
    .execute(&mut *conn)
    .await?;
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
             OR datname LIKE 'nats\\_%' ESCAPE '\\'
           )
           AND datname NOT LIKE 'proxima\\_tmpl\\_core\\_%' ESCAPE '\\'
           AND datname NOT LIKE 'proxima\\_tmpl\\_code\\_%' ESCAPE '\\'
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

#[cfg(test)]
mod tests {

    use time::OffsetDateTime;

    use super::{
        ADMIN_URL_UNSET, ADMIN_URL_VAR, BUILTIN_FAMILIES, MAX_FAMILY_PREFIX_BYTES,
        RESERVED_PREFIXES, SPLIT_ROLE_PASSWORD, TemplateFamily, TemplateFamilyError, connect_admin,
        create_db, db_url, db_url_from_admin, drop_unused_db_on, name_is_stale_clone, quoted_ident,
        redacted_url, role_url_from_admin, template_family, trimmed_url, unique_db_name,
    };

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
        for secret in ["authority-secret", "private-secret", "query-admin"] {
            assert!(!text.contains(secret), "{secret} leaked into {text}");
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

    #[test]
    fn a_role_url_from_an_unparsable_admin_url_is_an_error_without_credentials() {
        let error = role_url_from_admin("postgres://u:private-secret@[broken", "t", "r")
            .expect_err("invalid IPv6 address");
        assert!(!error.to_string().contains("private-secret"));
    }

    #[test]
    fn template_family_only_core_code_and_split() {
        assert_eq!(
            template_family("proxima_tmpl_core_3f94e256ef4f396f"),
            Some("proxima_tmpl_core_")
        );
        assert_eq!(
            template_family("proxima_tmpl_code_2d01c98992e2f0a0"),
            Some("proxima_tmpl_code_")
        );
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
        for prefix in ["host_", "proxima_split_core_", "a_b_"] {
            let family = TemplateFamily::new(prefix).expect("family");
            for hash in [0, 1, 0xdead_beef_dead_beef, u64::MAX, 0x0192_8f5a_0000_0000] {
                let name = family.template_name(hash);
                assert!(!name_is_stale_clone(&name, far_future), "{name}");
                assert!(name.len() <= 63, "{name}");
            }
        }
    }
}

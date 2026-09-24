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

const DEFAULT_ADMIN_URL: &str = "postgres://proxima:proxima@localhost/proxima";
const DROP_RETRIES: usize = 25;
const DROP_RETRY_DELAY: Duration = Duration::from_millis(200);
const SQLSTATE_DATABASE_ACCESSED: &str = "55006";
const SQLSTATE_UNDEFINED_DATABASE: &str = "3D000";
/// Untracked leftovers (pre-harness leaks) older than this are swept at boot.
const UNTRACKED_GRACE: time::Duration = time::Duration::minutes(5);
const SPLIT_PLATFORM_ROLE: &str = "proxima_test_platform";
const SPLIT_RUNTIME_ROLE: &str = "proxima_test_runtime";
const SPLIT_ROLE_PASSWORD: &str = "proxima_test_fixture_password";
pub const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
pub const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

static PROCESS_START: OnceLock<OffsetDateTime> = OnceLock::new();
static SWEEP_DONE: Mutex<bool> = Mutex::new(false);
static CATALOG_READY: AtomicBool = AtomicBool::new(false);

/// The admin connection URL from `PROXIMA_TEST_PG_URL`, or the local default
/// when unconfigured.
///
/// Trims, and treats an empty or whitespace-only value as unset — the rule
/// `proxima_core::env_value` states for every configuration variable in the
/// workspace. Applied by hand rather than by calling it, because this is a
/// deliberately minimal test-support crate (sqlx/tokio/tracing/url/uuid) and
/// one env var does not justify a dependency on `proxima-core`.
///
/// Without the empty check, `PROXIMA_TEST_PG_URL=` handed an empty string
/// straight to the connector, and every test in the run failed against a
/// connection error that named nothing an operator had typed.
#[must_use]
pub fn admin_url() -> String {
    std::env::var("PROXIMA_TEST_PG_URL")
        .ok()
        .map(|raw| raw.trim().to_string())
        .filter(|url| !url.is_empty())
        .unwrap_or_else(|| DEFAULT_ADMIN_URL.into())
}

/// URL for one test database, retaining the admin endpoint configuration.
///
/// # Panics
///
/// Panics if `PROXIMA_TEST_PG_URL` is not a valid URL. The diagnostic does
/// not include the URL or its credentials.
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

fn redacted_url(mut parsed: url::Url) -> url::Url {
    if parsed.password().is_some() {
        let _ = parsed.set_password(Some("****"));
    }
    parsed
}

fn redacted_db_url(name: &str) -> String {
    match db_url_from_admin(&admin_url(), name) {
        Ok(parsed) => redacted_url(parsed).to_string(),
        Err(_) => name.to_owned(),
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
        let ledgers = format!(
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
             );
             ALTER TABLE public._sqlx_migrations OWNER TO {platform};
             ALTER TABLE public._sqlx_migrations_proxima_code OWNER TO {platform};
             REVOKE INSERT, UPDATE, DELETE, TRUNCATE ON public._sqlx_migrations FROM {runtime};
             REVOKE INSERT, UPDATE, DELETE, TRUNCATE ON public._sqlx_migrations_proxima_code FROM {runtime};
             GRANT SELECT ON public._sqlx_migrations, public._sqlx_migrations_proxima_code TO {runtime}",
            platform = quoted_ident(SPLIT_PLATFORM_ROLE),
            runtime = quoted_ident(SPLIT_RUNTIME_ROLE),
        );
        // SQL-POLICY: fixed-fragment — ledger names and role identifiers are closed fixture values.
        sqlx::raw_sql(AssertSqlSafe(ledgers))
            .execute(&mut target)
            .await?;
        let transfer = format!(
            "DO $$ DECLARE r record; BEGIN
               FOR r IN SELECT n.nspname FROM pg_namespace n
                  WHERE n.nspname IN ({schema_list}) LOOP
                 EXECUTE format('ALTER SCHEMA %I OWNER TO %I', r.nspname, '{platform}');
               END LOOP;
               FOR r IN SELECT n.nspname, c.relname, c.relkind
                   FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
                  WHERE n.nspname IN ({schema_list})
                    AND c.relkind IN ('r','p','S') LOOP
                 IF r.relkind = 'S' THEN
                   EXECUTE format('ALTER SEQUENCE %I.%I OWNER TO %I', r.nspname, r.relname, '{platform}');
                 ELSE
                   EXECUTE format('ALTER TABLE %I.%I OWNER TO %I', r.nspname, r.relname, '{platform}');
                 END IF;
               END LOOP;
               FOR r IN SELECT n.nspname, p.proname,
                                pg_get_function_identity_arguments(p.oid) AS args
                   FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
                  WHERE n.nspname IN ({schema_list}) LOOP
                 EXECUTE format('ALTER FUNCTION %I.%I(%s) OWNER TO %I',
                                r.nspname, r.proname, r.args, '{platform}');
               END LOOP;
               IF to_regclass('public._sqlx_migrations') IS NOT NULL THEN
                 ALTER TABLE public._sqlx_migrations OWNER TO {platform_ident};
               END IF;
               IF to_regclass('public._sqlx_migrations_proxima_code') IS NOT NULL THEN
                 ALTER TABLE public._sqlx_migrations_proxima_code OWNER TO {platform_ident};
               END IF;
             END $$",
            platform = SPLIT_PLATFORM_ROLE,
            platform_ident = quoted_ident(SPLIT_PLATFORM_ROLE),
        );
        // SQL-POLICY: fixed-fragment — schema names are validated lowercase identifiers quoted as literals; the role is a closed fixture value.
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
    let mut runtime = db_url_from_admin(&admin_url(), database)
        .map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
    runtime
        .set_username(SPLIT_RUNTIME_ROLE)
        .map_err(|()| sqlx::Error::Protocol("invalid runtime role".into()))?;
    runtime
        .set_password(Some(SPLIT_ROLE_PASSWORD))
        .map_err(|()| sqlx::Error::Protocol("invalid fixture password".into()))?;
    let mut platform = runtime.clone();
    platform
        .set_username(SPLIT_PLATFORM_ROLE)
        .map_err(|()| sqlx::Error::Protocol("invalid platform role".into()))?;
    Ok((runtime.to_string(), platform.to_string()))
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

/// Ensure a pre-migrated template database exists.
///
/// Serialized by a session-scoped advisory lock on the admin DB. `build`
/// runs only when `template` did not already exist, against a
/// max-one-connection pool to a staging database. The template name becomes
/// visible only after the build succeeds, so failed or interrupted builds
/// cannot be reused as complete templates.
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
    let mut conn = connect_admin().await?;
    let lock_key = advisory_lock_key(template);

    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(lock_key)
        .execute(&mut conn)
        .await?;

    let result = async {
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

    if result.is_ok()
        && let Err(error) = drop_stale_templates_on(&mut conn, template).await
    {
        tracing::warn!(template, %error, "failed to drop stale test templates");
    }

    let unlock_result = sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(lock_key)
        .execute(&mut conn)
        .await;
    let close_result = conn.close().await;

    result?;
    unlock_result?;
    close_result?;

    Ok(())
}

/// Drop idle `proxima_tmpl_{core,code}_*` databases other than `keep`.
///
/// No-op when `keep` is not a core/code template name. Called from
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
    let mut conn = connect_admin().await?;
    let statement = format!(
        "CREATE DATABASE {} TEMPLATE {}",
        quoted_ident(&name),
        quoted_ident(template)
    );
    let mut last_error = None;

    for _ in 0..DROP_RETRIES {
        match sqlx::raw_sql(AssertSqlSafe(statement.clone()))
            .execute(&mut conn)
            .await
        {
            Ok(_) => {
                if let Err(error) = record_db(&mut conn, &name).await {
                    let _ = drop_db_on(&mut conn, &name).await;
                    conn.close().await?;
                    return Err(error);
                }
                conn.close().await?;
                return Ok(name);
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
    let mut conn = PgConnection::connect(&admin_url()).await?;
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
    let pattern = format!("{}%", family.replace('_', r"\_"));
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
    for name in stale {
        match drop_db_on(conn, &name).await {
            Ok(()) => dropped += 1,
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

fn template_family(name: &str) -> Option<&'static str> {
    const FAMILIES: [&str; 2] = ["proxima_tmpl_core_", "proxima_tmpl_code_"];
    FAMILIES.into_iter().find(|prefix| name.starts_with(prefix))
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
    use sqlx::ConnectOptions;
    use sqlx::postgres::{PgConnectOptions, PgSslMode};

    use super::{
        advisory_lock_key, db_url_from_admin, name_is_stale_clone, quoted_ident, redacted_url,
        template_family, unique_db_name,
    };
    use time::{Duration, OffsetDateTime};
    use uuid::Uuid;

    #[test]
    fn database_url_preserves_query_options_and_userinfo() {
        let admin = url::Url::parse(
            "postgres://user%40domain:p%40ss%2Fword@[::1]:55439/admin?sslmode=require\
             &application_name=pg-testkit&options=-c%20statement_timeout%3D5000\
             &sslrootcert=%2Ftmp%2Froot.crt#fragment",
        )
        .expect("test URL");
        let target = db_url_from_admin(admin.as_str(), "isolated_test").expect("target URL");
        let options = PgConnectOptions::from_url(&target).expect("target options");
        assert_eq!(options.get_database(), Some("isolated_test"));
        assert!(matches!(options.get_ssl_mode(), PgSslMode::Require));
        assert_eq!(options.get_application_name(), Some("pg-testkit"));
        assert_eq!(options.get_options(), Some("-c statement_timeout=5000"));
        assert_eq!(target.username(), admin.username());
        assert_eq!(target.password(), admin.password());
        assert_eq!(target.host(), admin.host());
        assert_eq!(target.port(), admin.port());
        assert_eq!(target.fragment(), admin.fragment());
        assert!(
            target
                .query_pairs()
                .any(|(key, value)| { key == "sslrootcert" && value == "/tmp/root.crt" })
        );
    }

    #[test]
    fn database_url_preserves_socket_and_replaces_database_overrides() {
        for host in ["/tmp/postgres", "%2Ftmp%2Fpostgres"] {
            let admin = format!(
                "postgres:///admin?dbname=admin&host={host}&db%6Eame=other&application_name=tests"
            );
            let target = db_url_from_admin(&admin, "isolated_test").expect("target URL");
            let options = PgConnectOptions::from_url(&target).expect("target options");
            assert_eq!(options.get_database(), Some("isolated_test"));
            assert_eq!(
                options.get_socket().map(std::path::PathBuf::as_path),
                Some(std::path::Path::new("/tmp/postgres"))
            );
            assert_eq!(options.get_application_name(), Some("tests"));
            let databases: Vec<_> = target
                .query_pairs()
                .filter(|(key, _)| key == "dbname")
                .map(|(_, value)| value.into_owned())
                .collect();
            assert_eq!(databases, ["isolated_test"]);
        }
    }

    #[test]
    fn database_url_preserves_a_host_without_database_path() {
        let target = db_url_from_admin(
            "postgres://user:pass@localhost:55439?application_name=tests",
            "isolated_test",
        )
        .expect("target URL");
        let options = PgConnectOptions::from_url(&target).expect("target options");
        assert_eq!(options.get_host(), "localhost");
        assert_eq!(options.get_port(), 55439);
        assert_eq!(options.get_database(), Some("isolated_test"));
        assert_eq!(options.get_application_name(), Some("tests"));
    }

    #[test]
    fn database_url_round_trips_database_names_as_data() {
        for name in [
            "Grüße 世界",
            "a/b%2Fc?#&dbname=admin",
            "/leading",
            ".",
            "..",
        ] {
            let target = db_url_from_admin("postgres://localhost/admin", name).expect("target URL");
            let options = PgConnectOptions::from_url(&target).expect("target options");
            assert_eq!(options.get_database(), Some(name));
        }
    }

    #[test]
    fn invalid_admin_url_error_contains_no_credentials() {
        let result = db_url_from_admin("postgres://user:private-secret@[broken", "isolated_test");
        let error = result.expect_err("invalid IPv6 address");
        assert!(!error.to_string().contains("private-secret"));
    }

    #[test]
    fn unique_db_name_uses_prefix_and_simple_uuidv7() {
        let name = unique_db_name("proxima_test");

        assert!(name.starts_with("proxima_test_"));
        assert_eq!(name.len(), "proxima_test_".len() + 32);
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
    fn template_family_only_core_and_code() {
        assert_eq!(
            template_family("proxima_tmpl_core_3f94e256ef4f396f"),
            Some("proxima_tmpl_core_")
        );
        assert_eq!(
            template_family("proxima_tmpl_code_2d01c98992e2f0a0"),
            Some("proxima_tmpl_code_")
        );
        assert_eq!(template_family("proxima_tmpl_build_abc"), None);
        assert_eq!(template_family("proxima_test_abc"), None);
    }

    #[test]
    fn uuid_v7_suffix_is_stale_before_cutoff() {
        let uuid = Uuid::now_v7();
        let name = format!("proxima_test_{}", uuid.simple());
        let future = OffsetDateTime::now_utc() + Duration::minutes(1);
        let past = OffsetDateTime::now_utc() - Duration::minutes(1);
        assert!(name_is_stale_clone(&name, future));
        assert!(!name_is_stale_clone(&name, past));
        assert!(!name_is_stale_clone("not_a_clone", future));
        assert!(!name_is_stale_clone("proxima_tmpl_core_deadbeef", future));
    }

    #[test]
    fn advisory_lock_key_is_deterministic() {
        assert_eq!(
            advisory_lock_key("proxima_tmpl_core_abc"),
            advisory_lock_key("proxima_tmpl_core_abc")
        );
    }
}

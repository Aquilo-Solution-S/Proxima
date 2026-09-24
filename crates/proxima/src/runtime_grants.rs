//! Opt-in boot step (`PROXIMA_RUNTIME_GRANTS`, [`crate::ProximaBuilder::runtime_grants`]):
//! the platform role grants the runtime role its privileges — the runtime
//! half of docs/15 §Owner-RLS rollout step 2.
//!
//! Runs once per boot, after migrations (or the `skip_migrations` preflight)
//! and before the runtime pool connects, on the platform (migration) pool, in
//! one transaction:
//!
//! | # | Statement | Scope |
//! |---|---|---|
//! | 1 | `SELECT pg_advisory_xact_lock(RUNTIME_GRANTS_LOCK_KEY)` | serializes pods booting together (`pg_default_acl` "tuple concurrently updated") |
//! | 2 | `GRANT USAGE ON SCHEMA s` | each composed schema that exists |
//! | 3 | `REVOKE CREATE ON SCHEMA s FROM PUBLIC` / `FROM runtime` | " |
//! | 4 | `GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA s` | " |
//! | 5 | `GRANT USAGE, SELECT, UPDATE ON ALL SEQUENCES IN SCHEMA s` | " |
//! | 6 | `ALTER DEFAULT PRIVILEGES IN SCHEMA s GRANT` (4) `ON TABLES` / (5) `ON SEQUENCES` | " — objects the platform role creates later |
//! | 7 | `GRANT USAGE ON SCHEMA l` | a ledger's schema that is not composed (`public`), when runtime lacks `USAGE` |
//! | 8 | `REVOKE INSERT, UPDATE, DELETE, TRUNCATE ON l.t`, then `GRANT SELECT ON l.t` | each existing migration ledger of this binary |
//!
//! Never granted: `TRUNCATE`, `REFERENCES`, `TRIGGER`, `CREATE`, ownership,
//! role membership; no DML on `public` beyond the ledgers' `SELECT` (host
//! tables may live there). All statements are `GRANT` / `REVOKE` /
//! `ALTER DEFAULT PRIVILEGES`: idempotent. Every identifier is
//! [`quote_ident`]ed.
//!
//! Not issued: `GRANT SET ON PARAMETER app.proxima_scope` — superuser-only,
//! and unused by this release (the scope is bound transaction-locally).
//!
//! Runtime role = the `DATABASE_URL` user. Refused with
//! [`EmbedError::Config`] before any SQL unless
//! `PROXIMA_PLATFORM_DATABASE_URL` is set and names a different user.

use std::str::FromStr;

use sqlx::postgres::PgConnectOptions;
use sqlx::{AssertSqlSafe, PgPool};

use crate::EmbedError;
use crate::migrations::NamedMigrator;

/// `pg_advisory_xact_lock` key owned by this step: ASCII `proxgrnt`, beside
/// storage's `proxembm` / `proxretn` maintenance keys.
const RUNTIME_GRANTS_LOCK_KEY: i64 = i64::from_be_bytes(*b"proxgrnt");

/// A validated runtime-grant plan: whom to grant, over which candidate
/// schemas and ledgers. Candidates that do not exist are skipped at apply.
#[derive(Debug)]
pub(crate) struct RuntimeGrants {
    runtime_role: String,
    schemas: Vec<String>,
    ledgers: Vec<String>,
}

impl RuntimeGrants {
    /// Validate the role split. Issues no SQL.
    ///
    /// `ledgers` are qualified ledger names ([`ledger_names`]).
    ///
    /// # Errors
    ///
    /// [`EmbedError::Config`] when `platform_database_url` is unset, either
    /// URL does not parse, or both URLs name the same user.
    pub(crate) fn plan(
        database_url: &str,
        platform_database_url: Option<&str>,
        schemas: Vec<String>,
        ledgers: Vec<String>,
    ) -> Result<Self, EmbedError> {
        let Some(platform_database_url) = platform_database_url else {
            return Err(EmbedError::Config(
                "runtime grants require PROXIMA_PLATFORM_DATABASE_URL: the platform role \
                 grants the DATABASE_URL role"
                    .into(),
            ));
        };
        let runtime_role = url_user(database_url, "DATABASE_URL")?;
        let platform_role = url_user(platform_database_url, "PROXIMA_PLATFORM_DATABASE_URL")?;
        if runtime_role == platform_role {
            return Err(EmbedError::Config(format!(
                "runtime grants require split roles: DATABASE_URL and \
                 PROXIMA_PLATFORM_DATABASE_URL both connect as {runtime_role:?}"
            )));
        }
        Ok(Self {
            runtime_role,
            schemas,
            ledgers,
        })
    }

    /// Issue the grants on `platform` in one transaction.
    ///
    /// # Errors
    ///
    /// [`EmbedError::Config`] when the runtime role does not exist;
    /// [`EmbedError::Storage`] naming the statement class that failed. The
    /// transaction rolls back: nothing is granted.
    pub(crate) async fn apply(&self, platform: &PgPool) -> Result<(), EmbedError> {
        let mut tx = platform
            .begin()
            .await
            .map_err(|error| failed("BEGIN", "platform pool", &error))?;
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(RUNTIME_GRANTS_LOCK_KEY)
            .execute(&mut *tx)
            .await
            .map_err(|error| failed("advisory lock", "runtime grants", &error))?;
        let runtime_exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = $1)")
                .bind(&self.runtime_role)
                .fetch_one(&mut *tx)
                .await
                .map_err(|error| failed("role lookup", &self.runtime_role, &error))?;
        if !runtime_exists {
            return Err(EmbedError::Config(format!(
                "runtime grants: DATABASE_URL role {:?} does not exist",
                self.runtime_role
            )));
        }
        let schemas: Vec<String> = sqlx::query_scalar(
            "SELECT nspname::text FROM pg_namespace WHERE nspname = ANY($1::text[]) ORDER BY 1",
        )
        .bind(&self.schemas)
        .fetch_all(&mut *tx)
        .await
        .map_err(|error| failed("schema lookup", "composed schemas", &error))?;
        // `to_regclass` resolves each compiled-in ledger name the way SQLx's
        // pinned-`public` migrator does and yields NULL for a ledger not yet
        // created; the join drops those. Schema `USAGE` is probed so an
        // already-usable `public` (PUBLIC holds it by default) is not
        // re-granted by a non-owner, which Postgres answers with a WARNING.
        let ledgers: Vec<Ledger> = sqlx::query_as::<_, (String, String, bool)>(
            "SELECT DISTINCT n.nspname::text, c.relname::text,
                    has_schema_privilege($2, n.oid, 'USAGE')
               FROM unnest($1::text[]) AS ledger(name)
               JOIN pg_class AS c ON c.oid = to_regclass(ledger.name)
               JOIN pg_namespace AS n ON n.oid = c.relnamespace
              WHERE c.relkind IN ('r', 'p')
              ORDER BY 1, 2",
        )
        .bind(&self.ledgers)
        .bind(&self.runtime_role)
        .fetch_all(&mut *tx)
        .await
        .map_err(|error| failed("ledger lookup", "migration ledgers", &error))?
        .into_iter()
        .map(|(schema, table, runtime_schema_usage)| Ledger {
            schema,
            table,
            runtime_schema_usage,
        })
        .collect();
        for statement in grant_statements(&self.runtime_role, &schemas, &ledgers) {
            // SQL-POLICY: fixed-fragment — fixed privilege lists; the role is
            // the parsed `DATABASE_URL` user and schema/table names are read
            // from the catalog above, each passed through `quote_ident`.
            sqlx::raw_sql(AssertSqlSafe(statement.sql))
                .execute(&mut *tx)
                .await
                .map_err(|error| failed(statement.class, &statement.object, &error))?;
        }
        tx.commit()
            .await
            .map_err(|error| failed("COMMIT", "runtime grants", &error))?;
        tracing::info!(
            runtime_role = %self.runtime_role,
            schemas = ?schemas,
            ledgers = ledgers.len(),
            "runtime grants applied"
        );
        Ok(())
    }
}

/// Qualified names of every ledger this binary migrates into: core's, then
/// each composed migrator's `table_name`. An unqualified name lives in
/// `public`, where the migration facade pins `search_path`.
pub(crate) fn ledger_names(migrators: &[NamedMigrator]) -> Vec<String> {
    let core = proxima_storage_pg::core_migrator();
    let mut ledgers: Vec<String> = std::iter::once(core.table_name.as_ref())
        .chain(
            migrators
                .iter()
                .map(|named| named.migrator().table_name.as_ref()),
        )
        .map(|table| {
            if table.contains('.') {
                table.to_owned()
            } else {
                format!("public.{table}")
            }
        })
        .collect();
    ledgers.sort();
    ledgers.dedup();
    ledgers
}

/// A `PostgreSQL` identifier, quoted, embedded `"` doubled.
fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn url_user(url: &str, variable: &str) -> Result<String, EmbedError> {
    // The parse error is not echoed: it could carry URL fragments.
    PgConnectOptions::from_str(url)
        .map(|options| options.get_username().to_owned())
        .map_err(|_| EmbedError::Config(format!("{variable} is not a valid Postgres URL")))
}

fn failed(class: &str, object: &str, error: &sqlx::Error) -> EmbedError {
    EmbedError::Storage(format!(
        "runtime grants: {class} on {object} failed: {error}"
    ))
}

/// One existing migration ledger, resolved from the catalog.
#[derive(Debug)]
struct Ledger {
    schema: String,
    table: String,
    /// Runtime can already use `schema` (before this step's grants).
    runtime_schema_usage: bool,
}

#[derive(Debug, PartialEq, Eq)]
struct GrantStatement {
    /// Statement class named in a failure.
    class: &'static str,
    /// The schema or table it targets, for the failure message.
    object: String,
    sql: String,
}

impl GrantStatement {
    fn new(class: &'static str, object: &str, sql: String) -> Self {
        Self {
            class,
            object: object.to_owned(),
            sql,
        }
    }
}

/// The statements, in order, for existing `schemas` and `ledgers`. Ledgers
/// are narrowed last, so a ledger inside a composed schema loses the DML its
/// schema-wide grant just gave it.
fn grant_statements(
    runtime_role: &str,
    schemas: &[String],
    ledgers: &[Ledger],
) -> Vec<GrantStatement> {
    let runtime = quote_ident(runtime_role);
    let mut statements = Vec::new();
    for name in schemas {
        let schema = quote_ident(name);
        statements.extend([
            GrantStatement::new(
                "GRANT USAGE ON SCHEMA",
                name,
                format!("GRANT USAGE ON SCHEMA {schema} TO {runtime}"),
            ),
            GrantStatement::new(
                "REVOKE CREATE ON SCHEMA FROM PUBLIC",
                name,
                format!("REVOKE CREATE ON SCHEMA {schema} FROM PUBLIC"),
            ),
            GrantStatement::new(
                "REVOKE CREATE ON SCHEMA FROM runtime",
                name,
                format!("REVOKE CREATE ON SCHEMA {schema} FROM {runtime}"),
            ),
            GrantStatement::new(
                "GRANT DML ON ALL TABLES",
                name,
                format!(
                    "GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA {schema} \
                     TO {runtime}"
                ),
            ),
            GrantStatement::new(
                "GRANT ON ALL SEQUENCES",
                name,
                format!(
                    "GRANT USAGE, SELECT, UPDATE ON ALL SEQUENCES IN SCHEMA {schema} TO {runtime}"
                ),
            ),
            GrantStatement::new(
                "ALTER DEFAULT PRIVILEGES ON TABLES",
                name,
                format!(
                    "ALTER DEFAULT PRIVILEGES IN SCHEMA {schema} \
                     GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO {runtime}"
                ),
            ),
            GrantStatement::new(
                "ALTER DEFAULT PRIVILEGES ON SEQUENCES",
                name,
                format!(
                    "ALTER DEFAULT PRIVILEGES IN SCHEMA {schema} \
                     GRANT USAGE, SELECT, UPDATE ON SEQUENCES TO {runtime}"
                ),
            ),
        ]);
    }
    let mut ledger_schemas: Vec<&String> = ledgers
        .iter()
        .filter(|ledger| !ledger.runtime_schema_usage && !schemas.contains(&ledger.schema))
        .map(|ledger| &ledger.schema)
        .collect();
    ledger_schemas.sort();
    ledger_schemas.dedup();
    for name in ledger_schemas {
        statements.push(GrantStatement::new(
            "GRANT USAGE ON ledger SCHEMA",
            name,
            format!("GRANT USAGE ON SCHEMA {} TO {runtime}", quote_ident(name)),
        ));
    }
    for Ledger { schema, table, .. } in ledgers {
        let object = format!("{schema}.{table}");
        let ledger = format!("{}.{}", quote_ident(schema), quote_ident(table));
        statements.extend([
            GrantStatement::new(
                "REVOKE writes ON ledger",
                &object,
                format!("REVOKE INSERT, UPDATE, DELETE, TRUNCATE ON {ledger} FROM {runtime}"),
            ),
            GrantStatement::new(
                "GRANT SELECT ON ledger",
                &object,
                format!("GRANT SELECT ON {ledger} TO {runtime}"),
            ),
        ]);
    }
    statements
}

#[cfg(test)]
mod tests {
    use super::{EmbedError, Ledger, RuntimeGrants, grant_statements, ledger_names, quote_ident};

    fn owned(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    fn ledger(schema: &str, table: &str) -> Ledger {
        Ledger {
            schema: schema.to_owned(),
            table: table.to_owned(),
            runtime_schema_usage: false,
        }
    }

    fn sql(statements: &[super::GrantStatement]) -> Vec<&str> {
        statements.iter().map(|s| s.sql.as_str()).collect()
    }

    #[test]
    fn identifiers_are_quoted_and_embedded_quotes_doubled() {
        assert_eq!(quote_ident("proxima_core"), "\"proxima_core\"");
        assert_eq!(quote_ident("a\"; DROP"), "\"a\"\"; DROP\"");
        let statements = grant_statements("rt\"x", &owned(&["proxima_core"]), &[]);
        assert!(
            sql(&statements).contains(&"GRANT USAGE ON SCHEMA \"proxima_core\" TO \"rt\"\"x\"")
        );
        assert!(sql(&statements).iter().all(|s| !s.contains("TO rt\"x")));
    }

    #[test]
    fn composed_schemas_get_dml_and_scoped_defaults_and_never_escalation() {
        let schemas = owned(&["proxima_code", "proxima_core"]);
        let statements = grant_statements(
            "runtime",
            &schemas,
            &[
                ledger("public", "_sqlx_migrations"),
                ledger("public", "_sqlx_migrations_proxima_code"),
            ],
        );
        let sql = sql(&statements);
        for schema in &schemas {
            for expected in [
                format!("GRANT USAGE ON SCHEMA \"{schema}\" TO \"runtime\""),
                format!("REVOKE CREATE ON SCHEMA \"{schema}\" FROM PUBLIC"),
                format!("REVOKE CREATE ON SCHEMA \"{schema}\" FROM \"runtime\""),
                format!(
                    "GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA \"{schema}\" TO \"runtime\""
                ),
                format!(
                    "GRANT USAGE, SELECT, UPDATE ON ALL SEQUENCES IN SCHEMA \"{schema}\" TO \"runtime\""
                ),
                format!(
                    "ALTER DEFAULT PRIVILEGES IN SCHEMA \"{schema}\" GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO \"runtime\""
                ),
                format!(
                    "ALTER DEFAULT PRIVILEGES IN SCHEMA \"{schema}\" GRANT USAGE, SELECT, UPDATE ON SEQUENCES TO \"runtime\""
                ),
            ] {
                assert!(sql.contains(&expected.as_str()), "missing {expected}");
            }
        }
        for statement in &sql {
            // Only REVOKEs may name the escalating privileges.
            if statement.starts_with("REVOKE") {
                continue;
            }
            for forbidden in [
                "TRUNCATE",
                "REFERENCES",
                "TRIGGER",
                "CREATE",
                "OWNER",
                "ALL PRIVILEGES",
            ] {
                assert!(!statement.contains(forbidden), "{statement}");
            }
            // Defaults are schema-scoped, never global.
            if statement.starts_with("ALTER DEFAULT PRIVILEGES") {
                assert!(statement.contains(" IN SCHEMA "), "{statement}");
            }
        }
        // The ledger schema gets USAGE only: no DML on `public`'s host tables.
        assert!(sql.contains(&"GRANT USAGE ON SCHEMA \"public\" TO \"runtime\""));
        assert!(
            sql.iter()
                .all(|s| !s.contains("ALL TABLES IN SCHEMA \"public\""))
        );
        assert!(
            sql.iter()
                .all(|s| !s.contains("IN SCHEMA \"public\" GRANT"))
        );
        assert!(sql.contains(
            &"GRANT SELECT ON \"public\".\"_sqlx_migrations_proxima_code\" TO \"runtime\""
        ));
    }

    #[test]
    fn usable_ledger_schema_is_not_regranted() {
        let usable = Ledger {
            runtime_schema_usage: true,
            ..ledger("public", "_sqlx_migrations")
        };
        let statements = grant_statements("runtime", &[], &[usable]);
        assert_eq!(
            sql(&statements),
            [
                "REVOKE INSERT, UPDATE, DELETE, TRUNCATE ON \"public\".\"_sqlx_migrations\" FROM \"runtime\"",
                "GRANT SELECT ON \"public\".\"_sqlx_migrations\" TO \"runtime\"",
            ]
        );
    }

    #[test]
    fn ledger_inside_a_composed_schema_is_narrowed_after_its_schema_grant() {
        let statements = grant_statements(
            "runtime",
            &owned(&["proxima_x"]),
            &[ledger("proxima_x", "_ledger")],
        );
        let sql = sql(&statements);
        let schema_dml = sql
            .iter()
            .position(|s| s.contains("ALL TABLES IN SCHEMA \"proxima_x\""))
            .expect("schema DML");
        let narrowed = sql
            .iter()
            .position(|s| {
                *s == "REVOKE INSERT, UPDATE, DELETE, TRUNCATE ON \"proxima_x\".\"_ledger\" FROM \"runtime\""
            })
            .expect("ledger narrowed");
        assert!(narrowed > schema_dml);
        // A composed ledger schema is not granted USAGE twice.
        assert_eq!(
            sql.iter()
                .filter(|s| s.starts_with("GRANT USAGE ON SCHEMA \"proxima_x\""))
                .count(),
            1
        );
    }

    #[test]
    fn ledger_names_are_qualified_and_deduplicated() {
        let named = crate::migrations::NamedMigrator::new(
            "proxima-code",
            proxima_code_like_migrator("public._sqlx_migrations_proxima_code"),
        );
        let unqualified = crate::migrations::NamedMigrator::new(
            "inline-flavor",
            proxima_code_like_migrator("_sqlx_migrations"),
        );
        assert_eq!(
            ledger_names(&[named, unqualified]),
            owned(&[
                "public._sqlx_migrations",
                "public._sqlx_migrations_proxima_code"
            ])
        );
    }

    fn proxima_code_like_migrator(table: &'static str) -> sqlx::migrate::Migrator {
        let mut migrator = sqlx::migrate::Migrator::DEFAULT;
        migrator.dangerous_set_table_name(table);
        migrator
    }

    #[test]
    fn plan_refuses_without_split_roles() {
        let missing =
            RuntimeGrants::plan("postgres://rt@localhost/db", None, Vec::new(), Vec::new())
                .expect_err("no platform URL");
        assert!(matches!(missing, EmbedError::Config(_)), "{missing}");
        let same = RuntimeGrants::plan(
            "postgres://same:a@localhost/db",
            Some("postgres://same:b@localhost/db"),
            Vec::new(),
            Vec::new(),
        )
        .expect_err("same user");
        assert!(
            matches!(same, EmbedError::Config(ref m) if m.contains("split roles")),
            "{same}"
        );
        let split = RuntimeGrants::plan(
            "postgres://rt:a@localhost/db",
            Some("postgres://platform:b@localhost/db"),
            Vec::new(),
            Vec::new(),
        )
        .expect("split roles plan");
        assert_eq!(split.runtime_role, "rt");
    }
}

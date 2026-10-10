//! `proxima_core.install_owner_rls_table` and `assert_owner_rls_census`
//! against the schema installer: a table installed in one call gets exactly
//! the policies `install_owner_rls` would give the same class, the schema
//! installer builds what core 0023 shipped, every classification that does not
//! name one owner per table is refused with nothing applied, and the policies
//! mean what the classes say.

mod common;

use common::{TestDb, apply_current_migrations};
use sqlx::{Connection, PgConnection};
use uuid::Uuid;

/// Core 0023, the release that last defined `install_owner_rls`.
const RELEASED_MIGRATION: &str =
    include_str!("../../../crates/storage-pg/migrations/0023_v029_kind_scoped_owner_rls.sql");

/// The released installer under another name, to build the policies the
/// per-table and the redefined schema installer are compared with.
fn released_installer() -> String {
    let start = RELEASED_MIGRATION
        .find("CREATE OR REPLACE FUNCTION proxima_core.install_owner_rls(")
        .expect("0023 defines the installer");
    let end = RELEASED_MIGRATION[start..]
        .find("$install_owner_rls$;")
        .expect("and ends it")
        + start
        + "$install_owner_rls$;".len();
    RELEASED_MIGRATION[start..end].replacen(
        "proxima_core.install_owner_rls(",
        "proxima_core.install_owner_rls_v029(",
        1,
    )
}

/// One table per class and per way a class reads its key: the FK on the key,
/// the only FK, a parent in the schema with a `t`, a goal parent, a
/// partitioned parent and child, a memory key that is `t` itself.
const FIXTURE: &str = "CREATE SCHEMA {s};
    CREATE TABLE {s}.cursor (owner_id uuid PRIMARY KEY, position bigint NOT NULL);
    CREATE TABLE {s}.settings (key text PRIMARY KEY, value text NOT NULL);
    CREATE TABLE {s}.note (t uuid PRIMARY KEY REFERENCES proxima_core.memory (t), body text NOT NULL);
    CREATE TABLE {s}.note_item (id bigint PRIMARY KEY, note_t uuid NOT NULL REFERENCES {s}.note (t));
    CREATE TABLE {s}.goal_note (id bigint PRIMARY KEY, goal_t uuid NOT NULL REFERENCES proxima_core.goal (t));
    CREATE TABLE {s}.fk_keyed (t uuid PRIMARY KEY REFERENCES proxima_core.memory (t),
        cited_t uuid REFERENCES proxima_core.memory (t));
    CREATE TABLE {s}.fk_only (id bigint PRIMARY KEY, x uuid REFERENCES proxima_core.memory (t));
    CREATE TABLE {s}.u (t uuid PRIMARY KEY REFERENCES proxima_core.memory (t)) PARTITION BY HASH (t);
    CREATE TABLE {s}.u0 PARTITION OF {s}.u FOR VALUES WITH (MODULUS 2, REMAINDER 0);
    CREATE TABLE {s}.u1 PARTITION OF {s}.u FOR VALUES WITH (MODULUS 2, REMAINDER 1);
    CREATE TABLE {s}.c (id bigint PRIMARY KEY, ut uuid REFERENCES {s}.u (t)) PARTITION BY HASH (id);
    CREATE TABLE {s}.c0 PARTITION OF {s}.c FOR VALUES WITH (MODULUS 1, REMAINDER 0);
    CREATE TABLE {s}.self_v1 (t uuid PRIMARY KEY REFERENCES proxima_core.memory (t));
    CREATE TABLE {s}.projection (memory_id uuid PRIMARY KEY REFERENCES proxima_core.memory (t),
        owner_id uuid NOT NULL);
    CREATE TABLE {s}.m_keyed (memory_id uuid PRIMARY KEY REFERENCES proxima_core.memory (t),
        cited_t uuid REFERENCES proxima_core.memory (t));
    CREATE TABLE {s}.m_only (id bigint PRIMARY KEY, x uuid REFERENCES proxima_core.memory (t));
    CREATE TABLE {s}.m_plain (t uuid NOT NULL)";

const OWNER_ID: &[&str] = &["cursor"];
const FK_PARENT: &[&str] = &[
    "note",
    "note_item",
    "goal_note",
    "fk_keyed",
    "fk_only",
    "u",
    "u0",
    "u1",
    "c",
    "c0",
];
const OWNERLESS: &[&str] = &["settings"];
const MEMORY_OWNER: &[&str] = &["self_v1", "projection", "m_keyed", "m_only", "m_plain"];

/// The code schema as core 0023 and the code flavor's v0.0.29 migration
/// classify it.
const CODE_OWNER_ID: &[&str] = &["repo_ingestion_runs", "repos"];
const CODE_FK_PARENT: &[&str] = &[
    "acceptance_criteria_v1",
    "acceptance_criterion_v1",
    "acceptance_summary_v1",
    "acceptance_verification_v1",
    "code_chunk_call_v1",
    "code_chunk_v1",
    "commit_summary_v1",
    "commit_v1",
    "development_perspective_v1",
    "execution_plan_item_v1",
    "execution_plan_v1",
    "execution_result_v1",
    "file_revision_v1",
    "test_requested_criterion_v1",
    "test_requested_v1",
    "test_result_v1",
    "work_assignment_v1",
    "work_requested_v1",
];
const CODE_MEMORY_OWNER: &[&str] = &[
    "commit_summarizer_self_v1",
    "engineer_self_v1",
    "projection",
];

const POLICIES: &str = "SELECT c.relname::text, p.polname::text, p.polcmd::text,
        p.polpermissive, p.polroles::text,
        replace(COALESCE(pg_get_expr(p.polqual, p.polrelid), ''), n.nspname, '<schema>'),
        replace(COALESCE(pg_get_expr(p.polwithcheck, p.polrelid), ''), n.nspname, '<schema>'),
        c.relrowsecurity, c.relforcerowsecurity
   FROM pg_policy AS p
   JOIN pg_class AS c ON c.oid = p.polrelid
   JOIN pg_namespace AS n ON n.oid = c.relnamespace
  WHERE n.nspname = $1
  ORDER BY 1, 2";

/// Policy identity: it changes when a policy is dropped and created again.
const POLICY_OIDS: &str = "SELECT c.relname::text, p.polname::text, p.oid::bigint
   FROM pg_policy AS p
   JOIN pg_class AS c ON c.oid = p.polrelid
   JOIN pg_namespace AS n ON n.oid = c.relnamespace
  WHERE n.nspname = $1
  ORDER BY 1, 2";

/// Every base table of a schema back to no policies and no RLS.
const STRIP_POLICIES: &str = "DO $strip$
DECLARE target record;
BEGIN
    FOR target IN
        SELECT n.nspname, c.relname
          FROM pg_class AS c JOIN pg_namespace AS n ON n.oid = c.relnamespace
         WHERE n.nspname = 'proxima_code' AND c.relkind IN ('r', 'p')
    LOOP
        EXECUTE format('DROP POLICY proxima_owner_read ON %I.%I', target.nspname, target.relname);
        EXECUTE format('DROP POLICY proxima_owner_write ON %I.%I', target.nspname, target.relname);
        EXECUTE format('DROP POLICY proxima_platform ON %I.%I', target.nspname, target.relname);
        EXECUTE format('ALTER TABLE %I.%I NO FORCE ROW LEVEL SECURITY', target.nspname, target.relname);
        EXECUTE format('ALTER TABLE %I.%I DISABLE ROW LEVEL SECURITY', target.nspname, target.relname);
    END LOOP;
END
$strip$";

type PolicyRow = (
    String,
    String,
    String,
    bool,
    String,
    String,
    String,
    bool,
    bool,
);

fn owned(tables: &[&str]) -> Vec<String> {
    tables.iter().map(|table| (*table).to_owned()).collect()
}

/// Which schema installer a call goes to.
#[derive(Clone, Copy)]
enum Installer {
    /// Core 0023's body, loaded by [`released_installer`].
    Released,
    /// `proxima_core.install_owner_rls`, as the current migrations define it.
    Current,
}

async fn platform_connection(db: &TestDb) -> PgConnection {
    let (_, platform_url) = proxima_pg_testkit::split_role_urls(&db.name)
        .await
        .expect("split roles");
    PgConnection::connect(&platform_url)
        .await
        .expect("platform connection")
}

/// A migrated database and a connection as the role that owns its tables.
async fn migrated_platform() -> (TestDb, PgConnection) {
    let db = TestDb::fresh().await;
    apply_current_migrations(&db.pg)
        .await
        .expect("current migrations");
    let conn = platform_connection(&db).await;
    (db, conn)
}

async fn create_fixture(conn: &mut PgConnection, schema: &str) {
    sqlx::raw_sql(sqlx::AssertSqlSafe(FIXTURE.replace("{s}", schema)))
        .execute(&mut *conn)
        .await
        .expect("fixture tables");
}

async fn install_schema(
    conn: &mut PgConnection,
    installer: Installer,
    schema: &str,
    lists: [&[&str]; 4],
) -> Result<(), sqlx::Error> {
    let [owner_id, fk_parent, ownerless, memory_owner] = lists;
    let query = match installer {
        Installer::Released => {
            sqlx::query("SELECT proxima_core.install_owner_rls_v029($1, $2, $3, $4, $5)")
        }
        Installer::Current => {
            sqlx::query("SELECT proxima_core.install_owner_rls($1, $2, $3, $4, $5)")
        }
    };
    query
        .bind(schema)
        .bind(owned(owner_id))
        .bind(owned(fk_parent))
        .bind(owned(ownerless))
        .bind(owned(memory_owner))
        .execute(&mut *conn)
        .await
        .map(|_| ())
}

async fn install_table(
    conn: &mut PgConnection,
    schema: &str,
    table: &str,
    class: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "SELECT proxima_core.install_owner_rls_table($1, $2, $3::proxima_core.owner_rls_class)",
    )
    .bind(schema)
    .bind(table)
    .bind(class)
    .execute(&mut *conn)
    .await
    .map(|_| ())
}

async fn install_table_column(
    conn: &mut PgConnection,
    schema: &str,
    table: &str,
    class: &str,
    owner_column: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "SELECT proxima_core.install_owner_rls_table($1, $2, $3::proxima_core.owner_rls_class, $4)",
    )
    .bind(schema)
    .bind(table)
    .bind(class)
    .bind(owner_column)
    .execute(&mut *conn)
    .await
    .map(|_| ())
}

/// Every `(table, class)` of a classification.
fn classified(lists: [&[&'static str]; 4]) -> Vec<(&'static str, &'static str)> {
    let [owner_id, fk_parent, ownerless, memory_owner] = lists;
    let mut tables = Vec::new();
    for (class, names) in [
        ("owner_id", owner_id),
        ("fk_parent", fk_parent),
        ("ownerless", ownerless),
        ("memory_owner", memory_owner),
    ] {
        tables.extend(names.iter().map(|name| (*name, class)));
    }
    tables
}

async fn policies(conn: &mut PgConnection, schema: &str) -> Vec<PolicyRow> {
    sqlx::query_as(POLICIES)
        .bind(schema)
        .fetch_all(&mut *conn)
        .await
        .expect("policies")
}

async fn policy_oids(conn: &mut PgConnection, schema: &str) -> Vec<(String, String, i64)> {
    sqlx::query_as(POLICY_OIDS)
        .bind(schema)
        .fetch_all(&mut *conn)
        .await
        .expect("policy oids")
}

/// How many tables of `schema` have RLS enabled or forced.
async fn rls_flagged(conn: &mut PgConnection, schema: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM pg_class AS c JOIN pg_namespace AS n ON n.oid = c.relnamespace
          WHERE n.nspname = $1 AND (c.relrowsecurity OR c.relforcerowsecurity)",
    )
    .bind(schema)
    .fetch_one(&mut *conn)
    .await
    .expect("RLS flags")
}

/// The message of a call that must be refused.
fn refusal(result: Result<(), sqlx::Error>, why: &str) -> String {
    result.expect_err(why).to_string()
}

/// For each of the four classes, a table installed through
/// `install_owner_rls_table` has the three policies the same table gets from
/// `install_owner_rls`, and the redefined schema installer builds what core
/// 0023 shipped. Three copies of one schema: the released installer, the
/// current one, and one call per table.
#[tokio::test]
#[allow(clippy::too_many_lines)] // one fixture schema, three installs, one comparison
async fn a_table_installed_per_call_has_the_policies_of_the_schema_installer() {
    let (_db, mut conn) = migrated_platform().await;
    let mut tx = conn.begin().await.expect("transaction");
    sqlx::raw_sql(sqlx::AssertSqlSafe(released_installer()))
        .execute(&mut *tx)
        .await
        .expect("the released installer loads under its own name");
    let lists: [&[&str]; 4] = [OWNER_ID, FK_PARENT, OWNERLESS, MEMORY_OWNER];
    for schema in ["rls_released", "rls_current", "rls_per_table"] {
        create_fixture(&mut tx, schema).await;
    }
    install_schema(&mut tx, Installer::Released, "rls_released", lists)
        .await
        .expect("the released installer takes the fixture");
    install_schema(&mut tx, Installer::Current, "rls_current", lists)
        .await
        .expect("the current installer takes the fixture");
    let tables = classified(lists);
    for (table, class) in &tables {
        install_table(&mut tx, "rls_per_table", table, class)
            .await
            .unwrap_or_else(|error| panic!("{table} as {class}: {error}"));
    }

    let released = policies(&mut tx, "rls_released").await;
    assert_eq!(
        released.len(),
        3 * tables.len(),
        "every table carries the three policies"
    );
    assert!(
        released
            .iter()
            .all(|(.., enabled, forced)| *enabled && *forced),
        "RLS is enabled and forced on every table"
    );
    assert_eq!(
        policies(&mut tx, "rls_current").await,
        released,
        "install_owner_rls builds what core 0023 built"
    );
    assert_eq!(
        policies(&mut tx, "rls_per_table").await,
        released,
        "one call per table builds what the schema installer builds"
    );
    for class in ["owner_id", "fk_parent", "ownerless", "memory_owner"] {
        assert!(
            tables.iter().any(|(_, listed)| *listed == class),
            "the fixture holds a {class} table"
        );
    }
    tx.rollback().await.expect("rollback");
}

/// The same comparison on the code flavor's schema, with each table's
/// policies stripped first so that the per-table call is the only author.
#[tokio::test]
async fn per_table_calls_rebuild_the_code_schemas_policies() {
    let (_db, mut conn) = migrated_platform().await;
    let mut tx = conn.begin().await.expect("transaction");
    sqlx::raw_sql(sqlx::AssertSqlSafe(released_installer()))
        .execute(&mut *tx)
        .await
        .expect("the released installer loads under its own name");
    let lists: [&[&str]; 4] = [CODE_OWNER_ID, CODE_FK_PARENT, &[], CODE_MEMORY_OWNER];
    install_schema(&mut tx, Installer::Released, "proxima_code", lists)
        .await
        .expect("the released installer takes the code schema");
    let released = policies(&mut tx, "proxima_code").await;
    assert_eq!(
        released.len(),
        3 * (CODE_OWNER_ID.len() + CODE_FK_PARENT.len() + CODE_MEMORY_OWNER.len())
    );

    install_schema(&mut tx, Installer::Current, "proxima_code", lists)
        .await
        .expect("the current installer takes the code schema");
    assert_eq!(
        policies(&mut tx, "proxima_code").await,
        released,
        "install_owner_rls builds what core 0023 built for the code schema"
    );

    sqlx::raw_sql(STRIP_POLICIES)
        .execute(&mut *tx)
        .await
        .expect("strip the code schema");
    assert!(policies(&mut tx, "proxima_code").await.is_empty());
    for (table, class) in classified([CODE_OWNER_ID, CODE_FK_PARENT, &[], CODE_MEMORY_OWNER]) {
        install_table(&mut tx, "proxima_code", table, class)
            .await
            .unwrap_or_else(|error| panic!("{table} as {class}: {error}"));
    }
    assert_eq!(
        policies(&mut tx, "proxima_code").await,
        released,
        "one call per code table builds what the schema installer builds"
    );
    tx.rollback().await.expect("rollback");
}

const REFUSALS: &str = "CREATE SCHEMA refused;
    CREATE SCHEMA elsewhere;
    CREATE TABLE elsewhere.outside (id bigint PRIMARY KEY);
    CREATE TABLE refused.plain (id bigint PRIMARY KEY);
    CREATE TABLE refused.text_owner (owner_id text PRIMARY KEY);
    CREATE TABLE refused.with_owner (owner_id uuid PRIMARY KEY);
    CREATE TABLE refused.ambiguous (id bigint PRIMARY KEY,
        cited_t uuid REFERENCES proxima_core.memory (t),
        about_t uuid REFERENCES proxima_core.memory (t));
    CREATE TABLE refused.loop_a (t uuid PRIMARY KEY);
    CREATE TABLE refused.loop_b (t uuid PRIMARY KEY REFERENCES refused.loop_a (t));
    ALTER TABLE refused.loop_a ADD FOREIGN KEY (t) REFERENCES refused.loop_b (t);
    CREATE TABLE refused.feeds_loop (t uuid PRIMARY KEY REFERENCES refused.loop_a (t));
    CREATE TABLE refused.foreign_parent (id bigint PRIMARY KEY,
        outside_id bigint REFERENCES elsewhere.outside (id));
    CREATE TABLE refused.keyless_parent (id bigint PRIMARY KEY);
    CREATE TABLE refused.under_keyless (id bigint PRIMARY KEY,
        parent_id bigint REFERENCES refused.keyless_parent (id));
    CREATE VIEW refused.a_view AS SELECT 1 AS x";

/// `(schema, table, class, owner_column, refusal)`.
type Refusal = (
    &'static str,
    &'static str,
    Option<&'static str>,
    Option<&'static str>,
    &'static str,
);

/// Each of these is refused, names its reason, and leaves every table of the
/// probe schema without RLS and without policies.
#[tokio::test]
#[allow(clippy::too_many_lines)] // one refusal table; each case is one row
async fn every_classification_that_names_no_single_owner_is_refused_with_nothing_applied() {
    let (_db, mut conn) = migrated_platform().await;
    sqlx::raw_sql(REFUSALS)
        .execute(&mut conn)
        .await
        .expect("refusal fixtures");
    let core_before = policy_oids(&mut conn, "proxima_core").await;

    let cases: [Refusal; 22] = [
        (
            "proxima_core",
            "memory",
            Some("ownerless"),
            None,
            "proxima_core is not a flavor schema",
        ),
        (
            "public",
            "plain",
            Some("ownerless"),
            None,
            "public is not a flavor schema",
        ),
        (
            "pg_catalog",
            "pg_class",
            Some("ownerless"),
            None,
            "pg_catalog is not a flavor schema",
        ),
        (
            "information_schema",
            "sql_features",
            Some("ownerless"),
            None,
            "information_schema is not a flavor schema",
        ),
        (
            "nowhere",
            "plain",
            Some("ownerless"),
            None,
            "schema nowhere does not exist",
        ),
        (
            "refused",
            "missing",
            Some("ownerless"),
            None,
            "refused.missing is not an existing base table",
        ),
        (
            "refused",
            "a_view",
            Some("ownerless"),
            None,
            "refused.a_view is not an existing base table",
        ),
        (
            "refused",
            "plain",
            Some("owner_id"),
            None,
            "refused.plain is class owner_id but has no owner_id column",
        ),
        (
            "refused",
            "plain",
            Some("owner_id"),
            Some("tenant"),
            "refused.plain is class owner_id but has no tenant column",
        ),
        (
            "refused",
            "text_owner",
            Some("owner_id"),
            None,
            "refused.text_owner.owner_id is text, not uuid",
        ),
        (
            "refused",
            "plain",
            Some("ownerless"),
            Some("tenant"),
            "owner_column belongs to class owner_id",
        ),
        (
            "refused",
            "plain",
            Some("memory_owner"),
            Some("tenant"),
            "owner_column belongs to class owner_id",
        ),
        (
            "refused",
            "with_owner",
            Some("fk_parent"),
            None,
            "refused.with_owner has an owner_id column but is class fk_parent",
        ),
        (
            "refused",
            "with_owner",
            Some("ownerless"),
            None,
            "refused.with_owner has an owner_id column but is class ownerless",
        ),
        (
            "refused",
            "ambiguous",
            Some("fk_parent"),
            None,
            "refused.ambiguous has 2 candidate parent FKs and none on its leading primary-key column",
        ),
        (
            "refused",
            "loop_a",
            Some("fk_parent"),
            None,
            "refused.loop_a reaches itself through its parent FKs",
        ),
        (
            "refused",
            "feeds_loop",
            Some("fk_parent"),
            None,
            "refused.feeds_loop reaches itself through its parent FKs",
        ),
        (
            "refused",
            "ambiguous",
            Some("memory_owner"),
            None,
            "refused.ambiguous has 2 foreign key(s) to proxima_core.memory",
        ),
        (
            "refused",
            "plain",
            Some("memory_owner"),
            None,
            "refused.plain is class memory_owner but has no uuid t column",
        ),
        (
            "refused",
            "foreign_parent",
            Some("fk_parent"),
            None,
            "no single-column FK to proxima_core or refused",
        ),
        (
            "refused",
            "under_keyless",
            Some("fk_parent"),
            None,
            "owner RLS writable classification missing for refused.under_keyless",
        ),
        ("refused", "plain", None, None, "refused.plain has no class"),
    ];
    for (schema, table, class, owner_column, expected) in cases {
        let result = sqlx::query(
            "SELECT proxima_core.install_owner_rls_table(
                 $1, $2, $3::proxima_core.owner_rls_class, COALESCE($4, 'owner_id'))",
        )
        .bind(schema)
        .bind(table)
        .bind(class)
        .bind(owner_column)
        .execute(&mut conn)
        .await
        .map(|_| ());
        let message = refusal(result, expected);
        assert!(
            message.contains(expected),
            "{schema}.{table} as {class:?}: expected {expected:?}, got {message}"
        );
    }

    // A class outside the enum never reaches the function.
    let message = refusal(
        install_table(&mut conn, "refused", "plain", "tenant").await,
        "a class outside the enum",
    );
    assert!(
        message.contains("invalid input value for enum proxima_core.owner_rls_class"),
        "{message}"
    );

    // The text owner column's refusal says what to store instead.
    let message = refusal(
        install_table(&mut conn, "refused", "text_owner", "owner_id").await,
        "a text owner column",
    );
    assert!(message.contains("OwnerRef::stable_key_uuid()"), "{message}");

    assert!(
        policies(&mut conn, "refused").await.is_empty(),
        "no refused call left a policy"
    );
    let flagged: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_class AS c JOIN pg_namespace AS n ON n.oid = c.relnamespace
          WHERE n.nspname = 'refused' AND (c.relrowsecurity OR c.relforcerowsecurity)",
    )
    .fetch_one(&mut conn)
    .await
    .expect("RLS flags");
    assert_eq!(flagged, 0, "no refused call enabled RLS");
    assert_eq!(
        policy_oids(&mut conn, "proxima_core").await,
        core_before,
        "a refused system schema left core's policies as they were"
    );
    conn.close().await.expect("close");
}

/// An explicit SQL NULL `owner_column` is refused for every class, whether
/// the class reads the column or not, and applies nothing: an untouched table
/// stays without RLS and policies, an installed one keeps its policy OIDs and
/// its RLS flags.
#[tokio::test]
async fn an_explicit_null_owner_column_is_refused_with_nothing_applied() {
    let (_db, mut conn) = migrated_platform().await;
    sqlx::raw_sql(
        "CREATE SCHEMA nullcol;
         CREATE TABLE nullcol.cursor (owner_id uuid PRIMARY KEY);
         CREATE TABLE nullcol.note (t uuid PRIMARY KEY REFERENCES proxima_core.memory (t));
         CREATE TABLE nullcol.settings (key text PRIMARY KEY);
         CREATE TABLE nullcol.facets (t uuid PRIMARY KEY REFERENCES proxima_core.memory (t));
         CREATE TABLE nullcol.installed (owner_id uuid PRIMARY KEY)",
    )
    .execute(&mut conn)
    .await
    .expect("null owner column fixtures");
    install_table(&mut conn, "nullcol", "installed", "owner_id")
        .await
        .expect("one table installed beforehand");
    let installed_before = policy_oids(&mut conn, "nullcol").await;
    assert_eq!(
        installed_before.len(),
        3,
        "the installed table has its policies"
    );
    assert_eq!(
        rls_flagged(&mut conn, "nullcol").await,
        1,
        "and only that table has RLS"
    );

    for (table, class) in [
        ("cursor", "owner_id"),
        ("note", "fk_parent"),
        ("settings", "ownerless"),
        ("facets", "memory_owner"),
        ("installed", "owner_id"),
        ("installed", "ownerless"),
    ] {
        let result = sqlx::query(
            "SELECT proxima_core.install_owner_rls_table(
                 $1, $2, $3::proxima_core.owner_rls_class, $4)",
        )
        .bind("nullcol")
        .bind(table)
        .bind(class)
        .bind(None::<&str>)
        .execute(&mut conn)
        .await
        .map(|_| ());
        let message = refusal(result, "a NULL owner_column");
        assert!(
            message.contains(&format!(
                "install_owner_rls_table: nullcol.{table} has a NULL owner_column"
            )),
            "{table} as {class}: {message}"
        );
    }

    assert_eq!(
        policy_oids(&mut conn, "nullcol").await,
        installed_before,
        "no refused call changed a policy: the installed table keeps its OIDs, the others gain none"
    );
    assert_eq!(
        rls_flagged(&mut conn, "nullcol").await,
        1,
        "no refused call changed an RLS flag"
    );
    conn.close().await.expect("close");
}

/// A host schema that grows by one table per migration: each call leaves the
/// earlier tables' policies (their OIDs) alone, a repeated call leaves the
/// same policies, and the census names a table nobody installed.
#[tokio::test]
async fn a_host_schema_grows_one_call_at_a_time() {
    let (_db, mut conn) = migrated_platform().await;
    sqlx::raw_sql("CREATE SCHEMA host")
        .execute(&mut conn)
        .await
        .expect("host schema");
    let lists: [&[&str]; 4] = [OWNER_ID, FK_PARENT, OWNERLESS, MEMORY_OWNER];
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        FIXTURE
            .replace("CREATE SCHEMA {s};", "")
            .replace("{s}", "host"),
    ))
    .execute(&mut conn)
    .await
    .expect("host tables");

    let tables = classified(lists);
    for (table, class) in &tables {
        let before = policy_oids(&mut conn, "host").await;
        install_table(&mut conn, "host", table, class)
            .await
            .unwrap_or_else(|error| panic!("{table} as {class}: {error}"));
        let after = policy_oids(&mut conn, "host").await;
        assert_eq!(
            after
                .iter()
                .filter(|(owner, ..)| owner != table)
                .cloned()
                .collect::<Vec<_>>(),
            before,
            "installing {table} touched no other table's policies"
        );
        assert_eq!(
            after.iter().filter(|(owner, ..)| owner == table).count(),
            3,
            "{table} has its three policies"
        );
    }

    let once = policies(&mut conn, "host").await;
    for (table, class) in &tables {
        install_table(&mut conn, "host", table, class)
            .await
            .unwrap_or_else(|error| panic!("{table} again: {error}"));
    }
    assert_eq!(
        policies(&mut conn, "host").await,
        once,
        "the same arguments leave the same policies"
    );

    sqlx::query("SELECT proxima_core.assert_owner_rls_census('host')")
        .execute(&mut conn)
        .await
        .expect("a complete schema passes the census");

    sqlx::raw_sql("CREATE TABLE host.forgotten (id bigint PRIMARY KEY)")
        .execute(&mut conn)
        .await
        .expect("a table added with no call");
    let message = sqlx::query("SELECT proxima_core.assert_owner_rls_census('host')")
        .execute(&mut conn)
        .await
        .expect_err("a table without the policies fails the census")
        .to_string();
    assert!(
        message.contains("table host.forgotten lacks ENABLE/FORCE RLS"),
        "{message}"
    );

    install_table(&mut conn, "host", "forgotten", "ownerless")
        .await
        .expect("the missing call");
    sqlx::query("SELECT proxima_core.assert_owner_rls_census('host')")
        .execute(&mut conn)
        .await
        .expect("and the census passes again");

    sqlx::raw_sql("DROP POLICY proxima_platform ON host.forgotten")
        .execute(&mut conn)
        .await
        .expect("drop one policy");
    let message = sqlx::query("SELECT proxima_core.assert_owner_rls_census('host')")
        .execute(&mut conn)
        .await
        .expect_err("a table short of a policy fails the census")
        .to_string();
    assert!(
        message.contains("table host.forgotten does not have exactly three RLS policies"),
        "{message}"
    );

    sqlx::raw_sql("CREATE SCHEMA empty")
        .execute(&mut conn)
        .await
        .expect("empty schema");
    for schema in ["nowhere", "empty"] {
        sqlx::query("SELECT proxima_core.assert_owner_rls_census($1)")
            .bind(schema)
            .execute(&mut conn)
            .await
            .expect_err("a census over no table proves nothing");
    }
    conn.close().await.expect("close");
}

async fn set_scope(conn: &mut PgConnection, scope: &str, owners: &[Uuid]) {
    let list = format!(
        "{{{}}}",
        owners
            .iter()
            .map(Uuid::to_string)
            .collect::<Vec<_>>()
            .join(",")
    );
    sqlx::query(
        "SELECT set_config('app.proxima_scope', $1, true),
                set_config('app.owner', $2, true),
                set_config('app.write_owner', $2, true)",
    )
    .bind(scope)
    .bind(list)
    .execute(&mut *conn)
    .await
    .expect("scope");
}

async fn platform_only_rows(conn: &mut PgConnection) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM host.platform_only")
        .fetch_one(&mut *conn)
        .await
        .expect("count")
}

async fn tenant_rows(conn: &mut PgConnection) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM host.tenant_rows")
        .fetch_one(&mut *conn)
        .await
        .expect("count")
}

/// A platform-only table is class `ownerless`: owner scope reads and writes
/// nothing, platform scope everything.
#[tokio::test]
async fn a_platform_only_table_is_closed_to_owner_scope_and_open_to_platform_scope() {
    let (_db, mut conn) = migrated_platform().await;
    let mut tx = conn.begin().await.expect("transaction");
    sqlx::raw_sql(
        "CREATE SCHEMA host;
         CREATE TABLE host.platform_only (key text PRIMARY KEY, value text NOT NULL)",
    )
    .execute(&mut *tx)
    .await
    .expect("host table");
    install_table(&mut tx, "host", "platform_only", "ownerless")
        .await
        .expect("a platform-only table is class ownerless");
    let owner = Uuid::now_v7();

    set_scope(&mut tx, "platform", &[]).await;
    sqlx::raw_sql("INSERT INTO host.platform_only VALUES ('k', 'v')")
        .execute(&mut *tx)
        .await
        .expect("platform scope writes");
    assert_eq!(platform_only_rows(&mut tx).await, 1);
    let updated = sqlx::query("UPDATE host.platform_only SET value = 'w'")
        .execute(&mut *tx)
        .await
        .expect("platform update");
    assert_eq!(updated.rows_affected(), 1);

    set_scope(&mut tx, "owner", &[owner]).await;
    assert_eq!(
        platform_only_rows(&mut tx).await,
        0,
        "owner scope reads nothing"
    );
    let mut insert = tx.begin().await.expect("savepoint");
    let error = sqlx::raw_sql("INSERT INTO host.platform_only VALUES ('o', 'v')")
        .execute(&mut *insert)
        .await
        .expect_err("owner scope writes nothing")
        .to_string();
    assert!(
        error.contains("row-level security"),
        "the insert is refused by the policy: {error}"
    );
    insert.rollback().await.expect("rollback savepoint");
    for statement in [
        "UPDATE host.platform_only SET value = 'x'",
        "DELETE FROM host.platform_only",
    ] {
        let touched = sqlx::raw_sql(statement)
            .execute(&mut *tx)
            .await
            .expect("owner statement")
            .rows_affected();
        assert_eq!(touched, 0, "{statement} reaches no row in owner scope");
    }

    set_scope(&mut tx, "platform", &[]).await;
    assert_eq!(
        platform_only_rows(&mut tx).await,
        1,
        "the row is still there for platform scope"
    );
    tx.rollback().await.expect("rollback");
}

/// An owner column other than `owner_id` scopes rows to its owner, in reads
/// and in writes.
#[tokio::test]
async fn a_named_owner_column_scopes_its_rows_to_the_owner() {
    let (_db, mut conn) = migrated_platform().await;
    let mut tx = conn.begin().await.expect("transaction");
    sqlx::raw_sql(
        "CREATE SCHEMA host;
         CREATE TABLE host.tenant_rows (id bigint PRIMARY KEY, tenant uuid NOT NULL, label text)",
    )
    .execute(&mut *tx)
    .await
    .expect("host table");
    install_table_column(&mut tx, "host", "tenant_rows", "owner_id", "tenant")
        .await
        .expect("a uuid owner column of any name");
    let policy: String = sqlx::query_scalar(
        "SELECT pg_get_expr(polqual, polrelid) FROM pg_policy
          WHERE polrelid = 'host.tenant_rows'::regclass AND polname = 'proxima_owner_read'",
    )
    .fetch_one(&mut *tx)
    .await
    .expect("read policy");
    assert!(policy.starts_with("(tenant = ANY"), "{policy}");

    let (mine, theirs) = (Uuid::now_v7(), Uuid::now_v7());
    set_scope(&mut tx, "platform", &[]).await;
    for (id, tenant) in [(1_i64, mine), (2, theirs)] {
        sqlx::query("INSERT INTO host.tenant_rows (id, tenant) VALUES ($1, $2)")
            .bind(id)
            .bind(tenant)
            .execute(&mut *tx)
            .await
            .expect("platform insert");
    }
    set_scope(&mut tx, "owner", &[mine]).await;
    assert_eq!(tenant_rows(&mut tx).await, 1, "reads own rows only");
    let updated = sqlx::query("UPDATE host.tenant_rows SET label = 'x'")
        .execute(&mut *tx)
        .await
        .expect("owner update");
    assert_eq!(updated.rows_affected(), 1, "writes own rows only");
    let mut insert = tx.begin().await.expect("savepoint");
    let error = sqlx::query("INSERT INTO host.tenant_rows (id, tenant) VALUES (3, $1)")
        .bind(theirs)
        .execute(&mut *insert)
        .await
        .expect_err("another owner's row is refused")
        .to_string();
    assert!(error.contains("row-level security"), "{error}");
    insert.rollback().await.expect("rollback savepoint");
    tx.rollback().await.expect("rollback");
}

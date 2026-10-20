//! `proxima_core.install_owner_rls` on a flavor schema, the example
//! flavor's: every classification that does not name one owner per table is
//! refused.

#[path = "fixtures/example_engine.rs"]
#[allow(dead_code, reason = "each suite uses part of the shared fixture")]
mod example_engine;

use example_engine::ExampleEngine;
use proxima::{FactPayload, FactWrite};
use proxima_core::{
    AuthPath, AuthzContext, OwnerId, UserId,
    test_fixtures::{authenticated_context, owner_scope_for_tests},
};
use proxima_example::StampV1;
use sqlx::{Connection, PgConnection, SqlSafeStr};
use uuid::Uuid;

/// The example schema's classification, as the example migrator states it
/// (baseline, `stamp_follows`, then `stamp_follows_memory_rls`).
const OWNER_ID_TABLES: &[&str] = &["notebooks"];
const FK_PARENT_TABLES: &[&str] = &["note_v1", "digest_v1"];
const OWNERLESS_TABLES: &[&str] = &["settings"];
/// The `owner: follows` stamp: visible exactly when its memory is.
const MEMORY_OWNER_TABLES: &[&str] = &["stamp_v1"];

const POLICIES: &str = "SELECT c.relname::text, p.polname::text, p.polcmd::text,
        p.polpermissive, p.polroles::text,
        COALESCE(pg_get_expr(p.polqual, p.polrelid), ''),
        COALESCE(pg_get_expr(p.polwithcheck, p.polrelid), ''),
        c.relrowsecurity, c.relforcerowsecurity
   FROM pg_policy AS p
   JOIN pg_class AS c ON c.oid = p.polrelid
   JOIN pg_namespace AS n ON n.oid = c.relnamespace
  WHERE n.nspname = 'proxima_example'
  ORDER BY 1, 2";

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

fn named_policy<'a>(policies: &'a [PolicyRow], table: &str, name: &str) -> &'a PolicyRow {
    policies
        .iter()
        .find(|policy| policy.0 == table && policy.1 == name)
        .unwrap_or_else(|| panic!("missing {name} policy on {table}"))
}

fn owned(tables: &[&str]) -> Vec<String> {
    tables.iter().map(|table| (*table).to_owned()).collect()
}

async fn install(
    conn: &mut PgConnection,
    owner_id: &[&str],
    fk_parent: &[&str],
    ownerless: &[&str],
    memory_owner: &[&str],
) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT proxima_core.install_owner_rls('proxima_example', $1, $2, $3, $4)")
        .bind(owned(owner_id))
        .bind(owned(fk_parent))
        .bind(owned(ownerless))
        .bind(owned(memory_owner))
        .execute(&mut *conn)
        .await
        .map(|_| ())
}

async fn refusal(
    conn: &mut PgConnection,
    owner_id: &[&str],
    fk_parent: &[&str],
    ownerless: &[&str],
    memory_owner: &[&str],
) -> String {
    let mut tx = conn.begin().await.expect("refusal transaction");
    let error = install(&mut tx, owner_id, fk_parent, ownerless, memory_owner)
        .await
        .expect_err("the installer must refuse this classification");
    tx.rollback().await.expect("rollback");
    error.to_string()
}

/// Every classification the installer must refuse, each in a rolled-back
/// transaction.
async fn assert_refusals(conn: &mut PgConnection) {
    let message = refusal(
        conn,
        &[],
        FK_PARENT_TABLES,
        OWNERLESS_TABLES,
        MEMORY_OWNER_TABLES,
    )
    .await;
    assert!(
        message.contains("owner RLS classification missing for proxima_example.notebooks"),
        "an unclassified table refuses: {message}"
    );

    let mut with_stale = FK_PARENT_TABLES.to_vec();
    with_stale.push("dropped_long_ago_v1");
    let message = refusal(
        conn,
        OWNER_ID_TABLES,
        &with_stale,
        OWNERLESS_TABLES,
        MEMORY_OWNER_TABLES,
    )
    .await;
    assert!(
        message.contains("classified table proxima_example.dropped_long_ago_v1 does not exist"),
        "a stale name refuses: {message}"
    );

    let mut twice = FK_PARENT_TABLES.to_vec();
    twice.push("notebooks");
    let message = refusal(
        conn,
        OWNER_ID_TABLES,
        &twice,
        OWNERLESS_TABLES,
        MEMORY_OWNER_TABLES,
    )
    .await;
    assert!(
        message.contains("proxima_example.notebooks is classified 2 times"),
        "a double classification refuses: {message}"
    );

    let mut owner_as_fk = FK_PARENT_TABLES.to_vec();
    owner_as_fk.push("notebooks");
    let message = refusal(
        conn,
        &[],
        &owner_as_fk,
        OWNERLESS_TABLES,
        MEMORY_OWNER_TABLES,
    )
    .await;
    assert!(
        message.contains(
            "proxima_example.notebooks has an owner_id column but is not in owner_id_tables"
        ),
        "an owner_id table cannot take a parent's policy: {message}"
    );

    let message = refusal(
        conn,
        OWNER_ID_TABLES,
        FK_PARENT_TABLES,
        &[],
        &["settings", "stamp_v1"],
    )
    .await;
    assert!(
        message.contains("settings is in memory_owner_tables but has no uuid t column"),
        "a memory-owner table must be keyed by t: {message}"
    );

    let error =
        sqlx::query("SELECT proxima_core.install_owner_rls('proxima_core', '{}', '{}', '{}')")
            .execute(&mut *conn)
            .await
            .expect_err("core's own schema is not a flavor schema");
    assert!(
        error
            .to_string()
            .contains("proxima_core is not a flavor schema")
    );
}

/// Classifications that do not name one owner per table, on scratch tables
/// in a throwaway schema.
/// (tables, `fk_parent_tables`, refusal or `""`, accepted `(table, read-policy fragment)`).
type ParentCase = (
    &'static str,
    &'static str,
    &'static str,
    &'static [(&'static str, &'static str)],
);

async fn assert_parent_refusals(conn: &mut PgConnection) {
    let cases: [ParentCase; 5] = [
        (
            "CREATE TABLE probe.self_ref (t uuid PRIMARY KEY, parent_t uuid REFERENCES probe.self_ref (t))",
            "ARRAY['self_ref']",
            "no single-column FK",
            &[],
        ),
        (
            "CREATE TABLE probe.a (t uuid PRIMARY KEY);
             CREATE TABLE probe.b (t uuid PRIMARY KEY REFERENCES probe.a (t));
             ALTER TABLE probe.a ADD FOREIGN KEY (t) REFERENCES probe.b (t)",
            "ARRAY['a', 'b']",
            "reaches itself through its parent FKs",
            &[],
        ),
        (
            "CREATE TABLE probe.amb (id bigint PRIMARY KEY,
                 cited_t uuid REFERENCES proxima_core.memory (t),
                 about_t uuid REFERENCES proxima_core.memory (t))",
            "ARRAY['amb']",
            "has 2 candidate parent FKs and none on its leading primary-key column",
            &[],
        ),
        (
            "CREATE TABLE probe.keyed (t uuid PRIMARY KEY REFERENCES proxima_core.memory (t),
                 cited_t uuid REFERENCES proxima_core.memory (t));
             CREATE TABLE probe.only_one (id bigint PRIMARY KEY, x uuid REFERENCES proxima_core.memory (t))",
            "ARRAY['keyed', 'only_one']",
            "",
            &[("probe.keyed", "parent.t = keyed.t"), ("probe.only_one", "parent.t = only_one.x")],
        ),
        // An FK to a partitioned table carries one clone per partition; a
        // partitioned child and its partition each still have one parent.
        (
            "CREATE TABLE probe.u (t uuid PRIMARY KEY REFERENCES proxima_core.memory (t)) PARTITION BY HASH (t);
             CREATE TABLE probe.u0 PARTITION OF probe.u FOR VALUES WITH (MODULUS 2, REMAINDER 0);
             CREATE TABLE probe.u1 PARTITION OF probe.u FOR VALUES WITH (MODULUS 2, REMAINDER 1);
             CREATE TABLE probe.c (id bigint PRIMARY KEY, ut uuid REFERENCES probe.u (t)) PARTITION BY HASH (id);
             CREATE TABLE probe.c0 PARTITION OF probe.c FOR VALUES WITH (MODULUS 1, REMAINDER 0)",
            "ARRAY['u', 'u0', 'u1', 'c', 'c0']",
            "",
            &[("probe.c", "parent.t = c.ut"), ("probe.c0", "parent.t = c0.ut")],
        ),
    ];
    for (tables, fk_parent, refusal, accepted) in cases {
        let mut tx = conn.begin().await.expect("probe transaction");
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "CREATE SCHEMA probe; {tables}"
        )))
        .execute(&mut *tx)
        .await
        .expect("probe tables");
        let result = sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "SELECT proxima_core.install_owner_rls('probe', '{{}}', {fk_parent}, '{{}}')"
        )))
        .execute(&mut *tx)
        .await;
        if refusal.is_empty() {
            result.expect("an FK on the key, or the only FK, names the owner");
            for (table, fragment) in accepted {
                let key: String = sqlx::query_scalar(
                    "SELECT pg_get_expr(polqual, polrelid) FROM pg_policy
                      WHERE polrelid = $1::regclass AND polname = 'proxima_owner_read'",
                )
                .bind(table)
                .fetch_one(&mut *tx)
                .await
                .expect("read policy");
                assert!(key.contains(fragment), "{table} keyed as {fragment}: {key}");
            }
        } else {
            let message = result.expect_err(refusal).to_string();
            assert!(message.contains(refusal), "{refusal}: {message}");
        }
        tx.rollback().await.expect("rollback");
    }
    let error = sqlx::query("SELECT proxima_core.install_owner_rls('public', '{}', '{}', '{}')")
        .execute(&mut *conn)
        .await
        .expect_err("public is not a flavor schema");
    assert!(error.to_string().contains("public is not a flavor schema"));
}

/// Memory-owner tables name their memory by the FK on the leading
/// primary-key column, else their only memory FK with no competing `t`, else
/// `t`; any other shape is ambiguous and refused.
/// (tables, `memory_owner_tables`, refusal or `""`, accepted `(table, fragment)`).
async fn assert_memory_owner_refusals(conn: &mut PgConnection) {
    let cases: [ParentCase; 3] = [
        (
            "CREATE TABLE probe.two_refs (id bigint PRIMARY KEY,
                 cited_t uuid REFERENCES proxima_core.memory (t),
                 about_t uuid REFERENCES proxima_core.memory (t))",
            "ARRAY['two_refs']",
            "its memory is ambiguous",
            &[],
        ),
        (
            "CREATE TABLE probe.t_and_ref (t uuid NOT NULL,
                 subject_t uuid REFERENCES proxima_core.memory (t))",
            "ARRAY['t_and_ref']",
            "its memory is ambiguous",
            &[],
        ),
        (
            "CREATE TABLE probe.keyed (memory_id uuid PRIMARY KEY REFERENCES proxima_core.memory (t),
                 cited_t uuid REFERENCES proxima_core.memory (t));
             CREATE TABLE probe.only_ref (id bigint PRIMARY KEY, x uuid REFERENCES proxima_core.memory (t));
             CREATE TABLE probe.plain (t uuid NOT NULL)",
            "ARRAY['keyed', 'only_ref', 'plain']",
            "",
            &[
                ("probe.keyed", "parent.t = keyed.memory_id"),
                ("probe.only_ref", "parent.t = only_ref.x"),
                ("probe.plain", "parent.t = plain.t"),
            ],
        ),
    ];
    for (tables, memory_owner, refusal, accepted) in cases {
        let mut tx = conn.begin().await.expect("probe transaction");
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "CREATE SCHEMA probe; {tables}"
        )))
        .execute(&mut *tx)
        .await
        .expect("probe tables");
        let result = sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "SELECT proxima_core.install_owner_rls('probe', '{{}}', '{{}}', '{{}}', {memory_owner})"
        )))
        .execute(&mut *tx)
        .await;
        if refusal.is_empty() {
            result.expect("one memory names each row");
            for (table, fragment) in accepted {
                let key: String = sqlx::query_scalar(
                    "SELECT pg_get_expr(polqual, polrelid) FROM pg_policy
                      WHERE polrelid = $1::regclass AND polname = 'proxima_owner_read'",
                )
                .bind(table)
                .fetch_one(&mut *tx)
                .await
                .expect("read policy");
                assert!(key.contains(fragment), "{table} keyed as {fragment}: {key}");
            }
        } else {
            let message = result.expect_err(refusal).to_string();
            assert!(message.contains(refusal), "{refusal}: {message}");
        }
        tx.rollback().await.expect("rollback");
    }
}

#[tokio::test]
async fn installer_refuses_invalid_classifications_and_preserves_live_policies() {
    let db = example_engine::migrated_db("proxima_owner_rls_installer").await;
    let mut conn = PgConnection::connect(db.platform_url())
        .await
        .expect("platform connection");

    let mut tx = conn.begin().await.expect("install transaction");
    install(
        &mut tx,
        OWNER_ID_TABLES,
        FK_PARENT_TABLES,
        OWNERLESS_TABLES,
        MEMORY_OWNER_TABLES,
    )
    .await
    .expect("the example classification installs");
    let expected: Vec<PolicyRow> = sqlx::query_as(POLICIES)
        .fetch_all(&mut *tx)
        .await
        .expect("installed policies");
    tx.rollback().await.expect("rollback");

    let live: Vec<PolicyRow> = sqlx::query_as(POLICIES)
        .fetch_all(&mut conn)
        .await
        .expect("live policies");
    assert_eq!(
        live.len(),
        3 * (OWNER_ID_TABLES.len()
            + FK_PARENT_TABLES.len()
            + OWNERLESS_TABLES.len()
            + MEMORY_OWNER_TABLES.len()),
        "every example table carries the three owner-RLS policies"
    );
    assert_eq!(live, expected, "live policies are the installer result");

    for table in FK_PARENT_TABLES {
        let read = named_policy(&live, table, "proxima_owner_read");
        let write = named_policy(&live, table, "proxima_owner_write");
        for policy in [read, write] {
            assert!(
                policy.5.contains(&format!("parent.t = {table}.t")),
                "{}: {}",
                policy.1,
                policy.5
            );
            assert!(!policy.5.contains("notebook_id"), "{}", policy.5);
            assert!(!policy.6.contains("notebook_id"), "{}", policy.6);
        }
    }

    for table in MEMORY_OWNER_TABLES {
        let read = named_policy(&live, table, "proxima_owner_read");
        let write = named_policy(&live, table, "proxima_owner_write");
        for policy in [read, write] {
            assert!(
                policy.5.contains("FROM proxima_core.memory parent")
                    && policy.5.contains(&format!("parent.t = {table}.t")),
                "{} reads through its memory: {}",
                policy.1,
                policy.5
            );
        }
    }

    assert_refusals(&mut conn).await;
    assert_parent_refusals(&mut conn).await;
    assert_memory_owner_refusals(&mut conn).await;
    conn.close().await.expect("close");
}

/// The host's Follows table of the example flavor.
const FOLLOWS_TABLE: &str = "proxima_example.stamp_v1";

const SYNC_SQL: &str = "SELECT proxima_core.sync_owner_follows($1, $2)";

/// Leading-`owner_id` btree indexes on `stamp_v1`.
const OWNER_INDEXES: &str = "SELECT count(*) FROM pg_index AS i
     JOIN pg_class AS ic ON ic.oid = i.indexrelid
     JOIN pg_am AS am ON am.oid = ic.relam
     JOIN pg_attribute AS a ON a.attrelid = i.indrelid AND a.attnum = i.indkey[0]
    WHERE i.indrelid = 'proxima_example.stamp_v1'::regclass
      AND am.amname = 'btree' AND i.indisvalid AND a.attname = 'owner_id'";

async fn platform_scope(conn: &mut PgConnection) {
    sqlx::query("SELECT set_config('app.proxima_scope', 'platform', true)")
        .execute(&mut *conn)
        .await
        .expect("platform scope");
}

async fn sync(
    conn: &mut PgConnection,
    schemas: &[&str],
    tables: &[&str],
) -> Result<(), sqlx::Error> {
    sqlx::query(SYNC_SQL)
        .bind(owned(schemas))
        .bind(owned(tables))
        .execute(&mut *conn)
        .await
        .map(|_| ())
}

async fn policies(conn: &mut PgConnection) -> Vec<PolicyRow> {
    sqlx::query_as(POLICIES)
        .fetch_all(&mut *conn)
        .await
        .expect("example policies")
}

/// Both owner policies of `stamp_v1` are the host follows rule: its own
/// `owner_id` check AND an EXISTS over its memory.
fn assert_follows_rule(policies: &[PolicyRow]) {
    let read = named_policy(policies, "stamp_v1", "proxima_owner_read");
    let write = named_policy(policies, "stamp_v1", "proxima_owner_write");
    for expr in [&read.5, &write.5, &write.6] {
        assert!(
            expr.contains("owner_id = ANY")
                && expr.contains("FROM proxima_core.memory parent")
                && expr.contains("parent.t = stamp_v1.t"),
            "stamp_v1 carries the follows rule: {expr}"
        );
    }
}

#[tokio::test]
async fn sync_gives_a_follows_table_the_combined_rule_and_an_owner_index() {
    let db = example_engine::migrated_db("proxima_owner_follows_sync").await;
    let mut conn = PgConnection::connect(db.platform_url())
        .await
        .expect("platform connection");
    let mut tx = conn.begin().await.expect("sync transaction");
    platform_scope(&mut tx).await;
    // The shared template now installs Follows state automatically. Recreate
    // the pre-sync state here so this SQL test still proves installation.
    sqlx::query("DELETE FROM proxima_core.owner_follows WHERE table_name = $1")
        .bind(FOLLOWS_TABLE)
        .execute(&mut *tx)
        .await
        .expect("remove the template's mark");
    sqlx::query("DROP INDEX proxima_example.stamp_v1_owner_follows_idx")
        .execute(&mut *tx)
        .await
        .expect("remove the template's generated owner index");
    install(
        &mut tx,
        OWNER_ID_TABLES,
        FK_PARENT_TABLES,
        OWNERLESS_TABLES,
        MEMORY_OWNER_TABLES,
    )
    .await
    .expect("install the flavor's pre-sync memory-owner policy");
    sqlx::query(
        "INSERT INTO proxima_core.owner_follows (table_name) VALUES ('proxima_other.thing_v1')",
    )
    .execute(&mut *tx)
    .await
    .expect("another binary's mark");
    let before: i64 = sqlx::query_scalar(OWNER_INDEXES)
        .fetch_one(&mut *tx)
        .await
        .expect("indexes");
    assert_eq!(
        before, 0,
        "the example declares no owner_id index on stamp_v1"
    );

    sync(&mut tx, &["proxima_example"], &[FOLLOWS_TABLE])
        .await
        .expect("the host syncs its Follows table");
    let first = policies(&mut tx).await;
    assert_follows_rule(&first);
    let indexes: i64 = sqlx::query_scalar(OWNER_INDEXES)
        .fetch_one(&mut *tx)
        .await
        .expect("indexes");
    assert_eq!(indexes, 1, "the sync adds one owner_id index");

    sync(&mut tx, &["proxima_example"], &[FOLLOWS_TABLE])
        .await
        .expect("the sync is idempotent");
    assert_eq!(
        policies(&mut tx).await,
        first,
        "a second sync changes no policy"
    );
    let indexes: i64 = sqlx::query_scalar(OWNER_INDEXES)
        .fetch_one(&mut *tx)
        .await
        .expect("indexes");
    assert_eq!(indexes, 1, "a second sync adds no index");

    let marks: Vec<String> =
        sqlx::query_scalar("SELECT table_name FROM proxima_core.owner_follows ORDER BY 1")
            .fetch_all(&mut *tx)
            .await
            .expect("marks");
    assert_eq!(
        marks,
        ["proxima_example.stamp_v1", "proxima_other.thing_v1"],
        "the sync leaves marks of schemas it does not compose"
    );

    sync(&mut tx, &["proxima_example"], &[])
        .await
        .expect("a registry without Follows tables unmarks the schema");
    let marks: Vec<String> =
        sqlx::query_scalar("SELECT table_name FROM proxima_core.owner_follows ORDER BY 1")
            .fetch_all(&mut *tx)
            .await
            .expect("marks");
    assert_eq!(marks, ["proxima_other.thing_v1"]);
    tx.rollback().await.expect("rollback");
    conn.close().await.expect("close");
}

#[tokio::test]
async fn installer_keeps_the_follows_rule_whatever_the_flavor_lists() {
    let db = example_engine::migrated_db("proxima_owner_follows_installer").await;
    let mut conn = PgConnection::connect(db.platform_url())
        .await
        .expect("platform connection");
    let mut tx = conn.begin().await.expect("install transaction");
    platform_scope(&mut tx).await;
    sync(&mut tx, &["proxima_example"], &[FOLLOWS_TABLE])
        .await
        .expect("the host syncs its Follows table");
    let synced = policies(&mut tx).await;
    assert_follows_rule(&synced);

    let mut as_owner_id = OWNER_ID_TABLES.to_vec();
    as_owner_id.push("stamp_v1");
    for (label, owner_id, memory_owner) in [
        ("in owner_id_tables", as_owner_id.as_slice(), &[][..]),
        (
            "in memory_owner_tables",
            OWNER_ID_TABLES,
            MEMORY_OWNER_TABLES,
        ),
        ("left out", OWNER_ID_TABLES, &[][..]),
    ] {
        install(
            &mut tx,
            owner_id,
            FK_PARENT_TABLES,
            OWNERLESS_TABLES,
            memory_owner,
        )
        .await
        .unwrap_or_else(|error| panic!("stamp_v1 {label} installs: {error}"));
        assert_eq!(
            policies(&mut tx).await,
            synced,
            "stamp_v1 {label} keeps the follows rule"
        );
    }
    sqlx::query(
        "SELECT proxima_core.install_owner_rls_table('proxima_example', 'stamp_v1', 'owner_id')",
    )
    .execute(&mut *tx)
    .await
    .expect("the per-table installer accepts a Follows table");
    assert_eq!(
        policies(&mut tx).await,
        synced,
        "install_owner_rls_table keeps the follows rule"
    );

    for (label, fk_parent, ownerless) in [
        (
            "fk_parent_tables",
            ["note_v1", "digest_v1", "stamp_v1"].as_slice(),
            OWNERLESS_TABLES,
        ),
        (
            "ownerless_tables",
            FK_PARENT_TABLES,
            ["settings", "stamp_v1"].as_slice(),
        ),
    ] {
        let mut savepoint = tx.begin().await.expect("savepoint");
        let message = install(&mut savepoint, OWNER_ID_TABLES, fk_parent, ownerless, &[])
            .await
            .expect_err("a table with owner_id takes no parent's or ownerless policy")
            .to_string();
        savepoint.rollback().await.expect("rollback to savepoint");
        assert!(
            message.contains(
                "proxima_example.stamp_v1 has an owner_id column but is not in owner_id_tables"
            ),
            "stamp_v1 in {label}: {message}"
        );
    }
    tx.rollback().await.expect("rollback");
    conn.close().await.expect("close");
}

/// How many stamp rows for memory `t` the `owner` scope sees, on the runtime
/// role under owner RLS.
async fn stamps_seen_by(runtime: &sqlx::PgPool, owner: OwnerId, t: Uuid) -> i64 {
    let mut tx =
        proxima_storage_pg::begin_owner_transaction(runtime, &owner_scope_for_tests(&[owner]))
            .await
            .expect("owner transaction");
    let seen: i64 =
        sqlx::query_scalar("SELECT count(*) FROM proxima_example.stamp_v1 WHERE t = $1")
            .bind(t)
            .fetch_one(&mut *tx)
            .await
            .expect("stamp count under owner RLS");
    tx.rollback().await.expect("rollback");
    seen
}

/// The r3 leak (F1): a flavor migration that classifies its Follows table as
/// `owner_id` after the host marked it must not let a forgotten memory's row
/// through an owner read.
#[tokio::test]
async fn forgotten_follows_row_hidden_after_flavor_installs_owner_id_class() {
    let harness = ExampleEngine::new("proxima_owner_follows_forgotten", |_| {}).await;
    let runtime = sqlx::PgPool::connect(harness.db.runtime_url())
        .await
        .expect("runtime connection");
    let mut conn = PgConnection::connect(harness.db.platform_url())
        .await
        .expect("platform connection");
    let mut tx = conn.begin().await.expect("install transaction");
    platform_scope(&mut tx).await;
    sync(&mut tx, &["proxima_example"], &[FOLLOWS_TABLE])
        .await
        .expect("the host syncs its Follows table");
    let mut as_owner_id = OWNER_ID_TABLES.to_vec();
    as_owner_id.push("stamp_v1");
    install(
        &mut tx,
        &as_owner_id,
        FK_PARENT_TABLES,
        OWNERLESS_TABLES,
        &[],
    )
    .await
    .expect("the flavor classifies stamp_v1 as owner_id");
    tx.commit().await.expect("commit");
    conn.close().await.expect("close");

    let user = UserId::new(Uuid::now_v7());
    let owner = OwnerId::from(user);
    let authz = authenticated_context(AuthzContext::for_subject(user, AuthPath::HostBearer));
    let stamp = StampV1 {
        mark: "follows-forgotten".to_owned(),
    };
    let memory_id = harness
        .engine
        .ingest_fact(&authz, FactWrite::new(owner, StampV1::SCHEMA_ID, &stamp))
        .await
        .expect("stamp is remembered")
        .memory_id;
    let t = memory_id.into_inner();
    assert_eq!(
        stamps_seen_by(&runtime, owner, t).await,
        1,
        "the owner sees its live stamp"
    );
    harness
        .engine
        .forget_memory(&authz, owner, memory_id)
        .await
        .expect("owner forgets the memory");
    assert_eq!(
        stamps_seen_by(&runtime, owner, t).await,
        0,
        "the forgotten memory's Follows row is hidden from its owner"
    );
}

#[tokio::test]
async fn refuses_sync_outside_composed_schemas() {
    let db = example_engine::migrated_db("proxima_owner_follows_refusals").await;
    let mut conn = PgConnection::connect(db.platform_url())
        .await
        .expect("platform connection");
    for (schemas, tables, refusal) in [
        (
            &["proxima_example"][..],
            &["proxima_core.memory"][..],
            "outside the composed schemas",
        ),
        (
            &["proxima_core"][..],
            &["proxima_core.memory"][..],
            "proxima_core is not a flavor schema",
        ),
        (
            &["proxima_example"][..],
            &["proxima_other.thing_v1"][..],
            "outside the composed schemas",
        ),
        (
            &["proxima_example"][..],
            &["stamp_v1"][..],
            "is not a lowercase schema.table name",
        ),
    ] {
        let mut tx = conn.begin().await.expect("refusal transaction");
        platform_scope(&mut tx).await;
        let message = sync(&mut tx, schemas, tables)
            .await
            .expect_err(refusal)
            .to_string();
        tx.rollback().await.expect("rollback");
        assert!(message.contains(refusal), "{refusal}: {message}");
    }
    let message = sync(&mut conn, &["proxima_example"], &[FOLLOWS_TABLE])
        .await
        .expect_err("outside platform scope")
        .to_string();
    assert!(message.contains("runs in platform scope only"), "{message}");
    conn.close().await.expect("close");
}

#[path = "fixtures/split_core_db.rs"]
mod split_core_db;

#[tokio::test]
async fn automatic_follows_installation_precedes_flavor_lanes_and_serves_skip_boot() {
    use std::borrow::Cow;

    use proxima::flavor::FlavorBundle;
    use proxima::{NamedMigrator, PgMigrationStorage, PgPoolConfig, PgTuning, Proxima};
    use proxima_example::ExampleFlavor;
    use sqlx::migrate::{Migration, MigrationType, Migrator};

    let db = split_core_db::clone_split_core_db("proxima_follows_order")
        .await
        .expect("owned test database");
    let pg = PgMigrationStorage::connect_with_config(
        db.platform_url(),
        PgPoolConfig::default(),
        PgTuning::default(),
    )
    .await
    .expect("migration storage");
    let sidecars = example_engine::frozen_sidecars();
    let probe = Migration::new(
        474,
        Cow::Borrowed("Follows registration precedes table creation"),
        MigrationType::Simple,
        "DO $$ BEGIN
             IF NOT EXISTS (SELECT 1 FROM proxima_core.owner_follows
                            WHERE table_name = 'proxima_example.stamp_v1') THEN
                 RAISE EXCEPTION 'Follows registration must precede flavor migrations';
             END IF;
         END $$"
            .into_sql_str(),
        false,
    );
    let mut sources = vec![NamedMigrator::flavor(
        "follows-order",
        Migrator {
            migrations: Cow::Owned(vec![probe]),
            ..Migrator::DEFAULT
        },
    )];
    sources.extend(ExampleFlavor::migrators());
    proxima::run_core_and_flavor_migrations(&pg, sources, &sidecars)
        .await
        .expect("runner registers Follows before flavor lanes and installs it afterwards");
    let pool = pg.clone_pool_for_backend();
    let mut connection = pool.acquire().await.expect("connection");
    let mut transaction = proxima::begin_migration_transaction(&mut connection)
        .await
        .expect("platform transaction");
    sidecars
        .ensure_owner_follows(&mut transaction)
        .await
        .expect("post-lane owner index and marks exist");
    transaction.rollback().await.expect("rollback");
    drop(connection);
    pg.clone_pool_for_backend().close().await;

    let owner = proxima::OwnerId::from(proxima::UserId::new(uuid::Uuid::now_v7()));
    let built = Proxima::<ExampleFlavor>::app()
        .database_url(db.runtime_url())
        .platform_database_url(db.platform_url())
        .store_number(0)
        .owner(owner)
        .allow_insecure_single_owner()
        .served_surface(proxima::ServedSurface::All)
        .skip_migrations()
        .build()
        .await
        .expect("skip boot validates generated Follows state without installing it");
    built.shutdown().await;
}

#[tokio::test]
async fn preflight_refuses_missing_follows_state_and_empty_sql_run_repairs_it() {
    use proxima::flavor::FlavorBundle;
    use proxima::{MigrationError, PgMigrationStorage, PgPoolConfig, PgTuning};
    use proxima_example::ExampleFlavor;

    let db = example_engine::migrated_db("proxima_follows_preflight").await;
    let pg = PgMigrationStorage::connect_with_config(
        db.platform_url(),
        PgPoolConfig::default(),
        PgTuning::default(),
    )
    .await
    .expect("migration storage");
    let sidecars = example_engine::frozen_sidecars();
    for mutation in [
        sqlx::query(
            "DELETE FROM proxima_core.owner_follows WHERE table_name = 'proxima_example.stamp_v1'",
        ),
        sqlx::query("INSERT INTO proxima_core.owner_follows VALUES ('proxima_example.extra_v1')"),
        sqlx::query("DROP INDEX proxima_example.stamp_v1_owner_follows_idx"),
    ] {
        let pool = pg.clone_pool_for_backend();
        let mut connection = pool.acquire().await.expect("connection");
        let mut transaction = proxima::begin_migration_transaction(&mut connection)
            .await
            .expect("platform transaction");
        mutation
            .execute(&mut *transaction)
            .await
            .expect("change deployed Follows state");
        transaction.commit().await.expect("commit");
        drop(connection);
        let refused =
            proxima::preflight_without_migrations(&pg, ExampleFlavor::migrators(), &sidecars).await;
        assert!(
            matches!(refused, Err(MigrationError::SidecarOwnerRls(_))),
            "preflight refuses mismatched deployment: {refused:?}"
        );
        let pending =
            proxima::pending_migrations(&pg.clone_pool_for_backend(), &ExampleFlavor::migrators())
                .await
                .expect("SQL migration plan");
        assert!(
            pending.is_empty(),
            "registry state is separate from the SQL ledger"
        );
        proxima::run_core_and_flavor_migrations(&pg, ExampleFlavor::migrators(), &sidecars)
            .await
            .expect("an empty SQL run restores generated state");
        proxima::preflight_without_migrations(&pg, ExampleFlavor::migrators(), &sidecars)
            .await
            .expect("read-only preflight sees platform-owned Follows marks");
    }
    pg.clone_pool_for_backend().close().await;
}

#[tokio::test]
async fn a_registered_follows_table_missing_after_migrations_refuses_the_run() {
    let db = split_core_db::clone_split_core_db("proxima_follows_missing")
        .await
        .expect("owned test database");
    let pg = proxima::PgMigrationStorage::connect_with_config(
        db.platform_url(),
        proxima::PgPoolConfig::default(),
        proxima::PgTuning::default(),
    )
    .await
    .expect("migration storage");
    let refused = proxima::run_core_and_flavor_migrations(
        &pg,
        Vec::new(),
        &example_engine::frozen_sidecars(),
    )
    .await;
    assert!(
        matches!(refused, Err(proxima::MigrationError::SidecarOwnerRls(_))),
        "registered sidecars must exist when all migration lanes finish: {refused:?}"
    );
    pg.clone_pool_for_backend().close().await;
}

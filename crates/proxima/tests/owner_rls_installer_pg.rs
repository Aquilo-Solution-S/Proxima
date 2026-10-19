//! `proxima_core.install_owner_rls` on a flavor schema, the example
//! flavor's: every classification that does not name one owner per table is
//! refused.

#[path = "fixtures/example_engine.rs"]
#[allow(dead_code, reason = "each suite uses part of the shared fixture")]
mod example_engine;

use sqlx::{Connection, PgConnection};

/// The example schema's classification, as the example migrator states it (baseline plus `stamp_follows`).
const OWNER_ID_TABLES: &[&str] = &["notebooks", "stamp_v1"];
const FK_PARENT_TABLES: &[&str] = &["note_v1", "digest_v1"];
const OWNERLESS_TABLES: &[&str] = &["settings"];

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
    let message = refusal(conn, &[], FK_PARENT_TABLES, OWNERLESS_TABLES, &[]).await;
    assert!(
        message.contains("owner RLS classification missing for proxima_example.notebooks"),
        "an unclassified table refuses: {message}"
    );

    let mut with_stale = FK_PARENT_TABLES.to_vec();
    with_stale.push("dropped_long_ago_v1");
    let message = refusal(conn, OWNER_ID_TABLES, &with_stale, OWNERLESS_TABLES, &[]).await;
    assert!(
        message.contains("classified table proxima_example.dropped_long_ago_v1 does not exist"),
        "a stale name refuses: {message}"
    );

    let mut twice = FK_PARENT_TABLES.to_vec();
    twice.push("notebooks");
    let message = refusal(conn, OWNER_ID_TABLES, &twice, OWNERLESS_TABLES, &[]).await;
    assert!(
        message.contains("proxima_example.notebooks is classified 2 times"),
        "a double classification refuses: {message}"
    );

    let mut owner_as_fk = FK_PARENT_TABLES.to_vec();
    owner_as_fk.push("notebooks");
    let message = refusal(conn, &[], &owner_as_fk, OWNERLESS_TABLES, &[]).await;
    assert!(
        message.contains(
            "proxima_example.notebooks has an owner_id column but is not in owner_id_tables"
        ),
        "an owner_id table cannot take a parent's policy: {message}"
    );

    let message = refusal(conn, OWNER_ID_TABLES, FK_PARENT_TABLES, &[], &["settings"]).await;
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
        &[],
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
        3 * (OWNER_ID_TABLES.len() + FK_PARENT_TABLES.len() + OWNERLESS_TABLES.len()),
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

    assert_refusals(&mut conn).await;
    assert_parent_refusals(&mut conn).await;
    assert_memory_owner_refusals(&mut conn).await;
    conn.close().await.expect("close");
}

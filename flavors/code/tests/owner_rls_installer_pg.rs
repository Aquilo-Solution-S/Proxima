//! `proxima_core.install_owner_rls` against this flavor's hand-written v0.0.15
//! owner-RLS block: identical policies on every table but one, where the
//! v0.0.15 block keyed an Abstraction sidecar on a Fact reference; the v0.0.20
//! code migration is exactly the installer call; and every classification
//! that does not name one owner per table is refused.

mod common;

use common::{TestDb, apply_current_migrations};
use sqlx::{Connection, PgConnection};

/// The code migration that replaces the v0.0.15 block with the installer.
const INSTALLER_MIGRATION: i64 = 20_260_924_000_020;

/// Core and the code lane up to (not including) the installer migration:
/// the v0.0.15 hand-written policies.
async fn migrate_through_v015(database: &str, pg: &proxima_storage_pg::PgStorage) {
    let _ = pg;
    let (_, platform_url) = proxima_pg_testkit::split_role_urls(database)
        .await
        .expect("split roles");
    let platform = proxima_storage_pg::PgStorage::connect_for_migrations_with_config(
        &platform_url,
        proxima_storage_pg::PgPoolConfig::default(),
        proxima_storage_pg::PgTuning::default(),
    )
    .await
    .expect("platform pool");
    let mut v015 = proxima_code::migrator();
    v015.migrations = std::borrow::Cow::Owned(
        v015.iter()
            .filter(|migration| migration.version < INSTALLER_MIGRATION)
            .cloned()
            .collect(),
    );
    proxima::run_core_and_flavor_migrations(
        &platform,
        [proxima::NamedMigrator::flavor("proxima-code", v015)],
    )
    .await
    .expect("core and the v0.0.15 code lane");
}

/// The code schema's classification, as a flavor calling the installer
/// states it.
const OWNER_ID_TABLES: &[&str] = &["projection", "repo_ingestion_runs", "repos"];
const FK_PARENT_TABLES: &[&str] = &[
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
const MEMORY_OWNER_TABLES: &[&str] = &["commit_summarizer_self_v1", "engineer_self_v1"];

const POLICIES: &str = "SELECT c.relname::text, p.polname::text, p.polcmd::text,
        p.polpermissive, p.polroles::text,
        COALESCE(pg_get_expr(p.polqual, p.polrelid), ''),
        COALESCE(pg_get_expr(p.polwithcheck, p.polrelid), ''),
        c.relrowsecurity, c.relforcerowsecurity
   FROM pg_policy AS p
   JOIN pg_class AS c ON c.oid = p.polrelid
   JOIN pg_namespace AS n ON n.oid = c.relnamespace
  WHERE n.nspname = 'proxima_code'
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
    sqlx::query("SELECT proxima_core.install_owner_rls('proxima_code', $1, $2, $3, $4)")
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
    memory_owner: &[&str],
) -> String {
    let mut tx = conn.begin().await.expect("refusal transaction");
    let error = install(&mut tx, owner_id, fk_parent, &[], memory_owner)
        .await
        .expect_err("the installer must refuse this classification");
    tx.rollback().await.expect("rollback");
    error.to_string()
}

/// Every classification the installer must refuse, each in a rolled-back
/// transaction.
async fn assert_refusals(conn: &mut PgConnection) {
    let without_repos: Vec<&str> = OWNER_ID_TABLES
        .iter()
        .copied()
        .filter(|table| *table != "repos")
        .collect();
    let message = refusal(conn, &without_repos, FK_PARENT_TABLES, MEMORY_OWNER_TABLES).await;
    assert!(
        message.contains("owner RLS classification missing for proxima_code.repos"),
        "an unclassified table refuses: {message}"
    );

    let mut with_stale = FK_PARENT_TABLES.to_vec();
    with_stale.push("dropped_long_ago_v1");
    let message = refusal(conn, OWNER_ID_TABLES, &with_stale, MEMORY_OWNER_TABLES).await;
    assert!(
        message.contains("classified table proxima_code.dropped_long_ago_v1 does not exist"),
        "a stale name refuses: {message}"
    );

    let mut twice = FK_PARENT_TABLES.to_vec();
    twice.push("repos");
    let message = refusal(conn, OWNER_ID_TABLES, &twice, MEMORY_OWNER_TABLES).await;
    assert!(
        message.contains("proxima_code.repos is classified 2 times"),
        "a double classification refuses: {message}"
    );

    let mut owner_as_fk = FK_PARENT_TABLES.to_vec();
    owner_as_fk.push("repos");
    let message = refusal(conn, &without_repos, &owner_as_fk, MEMORY_OWNER_TABLES).await;
    assert!(
        message.contains("proxima_code.repos has an owner_id column but is not in owner_id_tables"),
        "an owner_id table cannot take a parent's policy: {message}"
    );

    let fk_without_plan_item: Vec<&str> = FK_PARENT_TABLES
        .iter()
        .copied()
        .filter(|table| *table != "execution_plan_item_v1")
        .collect();
    let mut keyed = MEMORY_OWNER_TABLES.to_vec();
    keyed.push("execution_plan_item_v1");
    let message = refusal(conn, OWNER_ID_TABLES, &fk_without_plan_item, &keyed).await;
    assert!(
        message
            .contains("execution_plan_item_v1 is in memory_owner_tables but has no uuid t column"),
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

#[tokio::test]
async fn installer_rekeys_one_v015_policy_and_refuses_gaps() {
    let db = TestDb::fresh().await;
    migrate_through_v015(&db.name, &db.pg).await;
    let (_, platform_url) = proxima_pg_testkit::split_role_urls(&db.name)
        .await
        .expect("split roles");
    let mut conn = PgConnection::connect(&platform_url)
        .await
        .expect("platform connection");

    let hand_written: Vec<PolicyRow> = sqlx::query_as(POLICIES)
        .fetch_all(&mut conn)
        .await
        .expect("hand-written policies");
    assert_eq!(
        hand_written.len(),
        3 * (OWNER_ID_TABLES.len() + FK_PARENT_TABLES.len() + MEMORY_OWNER_TABLES.len()),
        "every code table carries the three owner-RLS policies"
    );

    let mut tx = conn.begin().await.expect("install transaction");
    install(
        &mut tx,
        OWNER_ID_TABLES,
        FK_PARENT_TABLES,
        &[],
        MEMORY_OWNER_TABLES,
    )
    .await
    .expect("the code classification installs");
    let installed: Vec<PolicyRow> = sqlx::query_as(POLICIES)
        .fetch_all(&mut *tx)
        .await
        .expect("installed policies");
    tx.rollback().await.expect("rollback");
    assert_eq!(hand_written.len(), installed.len());
    let changed: Vec<(&PolicyRow, &PolicyRow)> = hand_written
        .iter()
        .zip(&installed)
        .filter(|(left, right)| left != right)
        .collect();
    assert_eq!(
        changed
            .iter()
            .map(|(left, _)| (left.0.as_str(), left.1.as_str()))
            .collect::<Vec<_>>(),
        [
            ("execution_plan_v1", "proxima_owner_read"),
            ("execution_plan_v1", "proxima_owner_write"),
        ],
        "every other policy is the v0.0.15 file's"
    );
    for (v015, v020) in changed {
        assert!(
            v015.5
                .contains("execution_plan_v1.goal_activated_memory_id"),
            "{}",
            v015.5
        );
        assert!(
            v020.5.contains("parent.t = execution_plan_v1.t"),
            "{}",
            v020.5
        );
        assert!(!v020.5.contains("goal_activated_memory_id"), "{}", v020.5);
    }

    apply_current_migrations(&db.pg)
        .await
        .expect("the v0.0.20 code migration applies");
    let live: Vec<PolicyRow> = sqlx::query_as(POLICIES)
        .fetch_all(&mut conn)
        .await
        .expect("live policies");
    assert_eq!(
        live, installed,
        "the v0.0.20 migration is the installer call"
    );

    assert_refusals(&mut conn).await;
    assert_parent_refusals(&mut conn).await;

    conn.close().await.expect("close");
}

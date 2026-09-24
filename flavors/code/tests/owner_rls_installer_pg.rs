//! `proxima_core.install_owner_rls` reproduces this flavor's hand-written
//! v0.0.15 owner-RLS policies exactly, and refuses a classification that does
//! not cover the schema.

mod common;

use common::{TestDb, apply_current_migrations};
use sqlx::{Connection, PgConnection};

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

#[tokio::test]
async fn installer_reproduces_the_code_flavor_policies_and_refuses_gaps() {
    let db = TestDb::fresh().await;
    apply_current_migrations(&db.pg)
        .await
        .expect("current core and code migrations");
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
    for (left, right) in hand_written.iter().zip(&installed) {
        assert_eq!(
            left, right,
            "installer policy differs from the v0.0.15 file"
        );
    }
    assert_eq!(hand_written.len(), installed.len());

    assert_refusals(&mut conn).await;

    conn.close().await.expect("close");
}

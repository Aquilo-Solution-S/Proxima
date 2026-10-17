//! `proxima_core.install_owner_rls` for this flavor: every classification that
//! does not name one owner per table is refused.

mod common;

use common::{HistoricalDb, apply_current_migrations_pool};
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

/// v0.0.29: `projection` holds Fact and Abstraction rows, so it follows its
/// memory's kind.
const KIND_OWNER_ID_TABLES: &[&str] = &["repo_ingestion_runs", "repos"];
const KIND_MEMORY_OWNER_TABLES: &[&str] = &[
    "commit_summarizer_self_v1",
    "engineer_self_v1",
    "projection",
];

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
    let db = HistoricalDb::fresh().await;
    apply_current_migrations_pool(&db.pool)
        .await
        .expect("current migrations");
    let (_, platform_url) = proxima_pg_testkit::split_role_urls(&db.name)
        .await
        .expect("split roles");
    let mut conn = PgConnection::connect(&platform_url)
        .await
        .expect("platform connection");

    let mut tx = conn.begin().await.expect("install transaction");
    install(
        &mut tx,
        KIND_OWNER_ID_TABLES,
        FK_PARENT_TABLES,
        &[],
        KIND_MEMORY_OWNER_TABLES,
    )
    .await
    .expect("the kind-rule classification installs");
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
        3 * (KIND_OWNER_ID_TABLES.len() + FK_PARENT_TABLES.len() + KIND_MEMORY_OWNER_TABLES.len()),
        "every code table carries the three owner-RLS policies"
    );
    assert_eq!(live, expected, "live policies are the installer result");

    let execution_plan_read = named_policy(&live, "execution_plan_v1", "proxima_owner_read");
    let execution_plan_write = named_policy(&live, "execution_plan_v1", "proxima_owner_write");
    for policy in [execution_plan_read, execution_plan_write] {
        assert!(
            policy.5.contains("parent.t = execution_plan_v1.t"),
            "{}: {}",
            policy.1,
            policy.5
        );
        assert!(
            !policy.5.contains("goal_activated_memory_id"),
            "{}",
            policy.5
        );
        assert!(
            !policy.6.contains("goal_activated_memory_id"),
            "{}",
            policy.6
        );
    }

    for table in ["commit_summarizer_self_v1", "engineer_self_v1"] {
        let read = named_policy(&live, table, "proxima_owner_read");
        let write = named_policy(&live, table, "proxima_owner_write");
        assert!(read.5.contains("app.read_perspective"), "{}", read.5);
        assert!(write.5.contains("app.write_perspective"), "{}", write.5);
    }

    assert_refusals(&mut conn).await;
    assert_parent_refusals(&mut conn).await;
    assert_memory_owner_refusals(&mut conn).await;
    conn.close().await.expect("close");
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "the policy scenario keeps seed and scope assertions together"
)]
async fn projection_rows_follow_their_memory_kind() {
    use proxima_core::{AccessCeiling, AuthPath, AuthzContext, GroupId, OwnerId, Role, UserId};

    let db = HistoricalDb::fresh().await;
    apply_current_migrations_pool(&db.pool)
        .await
        .expect("current migrations");
    let admin = &db.pool;
    let owner = uuid::Uuid::now_v7();
    let (fact, abstraction) = (uuid::Uuid::now_v7(), uuid::Uuid::now_v7());
    let mut seed = admin.begin().await.expect("seed transaction");
    sqlx::query("INSERT INTO proxima_core.owners (owner_id, kind) VALUES ($1, 'group')")
        .bind(owner)
        .execute(&mut *seed)
        .await
        .expect("owner");
    let content = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxima_core.content (content_id, owner_id, schema_id, content_hash)
         VALUES ($1, $2, 'code/test', decode(repeat('00', 32), 'hex'))",
    )
    .bind(content)
    .bind(owner)
    .execute(&mut *seed)
    .await
    .expect("content");
    for (t, kind, origins, content_id) in [
        (fact, "fact", Vec::new(), None),
        (abstraction, "abstraction", vec![fact], Some(content)),
    ] {
        let handle = uuid::Uuid::now_v7();
        sqlx::query(
            "INSERT INTO proxima_core.memory_head (handle, schema_id, kind, owner_id, t)
             VALUES ($1, 'code/test', $2::proxima_core.memory_kind, $3, $4)",
        )
        .bind(handle)
        .bind(kind)
        .bind(owner)
        .bind(t)
        .execute(&mut *seed)
        .await
        .expect("memory head");
        sqlx::query(
            "INSERT INTO proxima_core.memory (handle, t, kind, owner_id, schema_id, content_id, origins, occurred_at, \"by\", via)
             VALUES ($1, $2, $3::proxima_core.memory_kind, $4, 'code/test', $5, $6, CASE WHEN $3::proxima_core.memory_kind = 'fact' THEN uuid_extract_timestamp($2) END, $4, NULL)",
        )
        .bind(handle)
        .bind(t)
        .bind(kind)
        .bind(owner)
        .bind(content_id)
        .bind(&origins)
        .execute(&mut *seed)
        .await
        .expect("memory");
        sqlx::query(
            "INSERT INTO proxima_code.projection (memory_id, schema_id, owner_id, search_tsv)
             VALUES ($1, 'code/test', $2, to_tsvector('simple', 'kind rule'))",
        )
        .bind(t)
        .bind(owner)
        .execute(&mut *seed)
        .await
        .expect("projection");
    }
    seed.commit().await.expect("seed");

    let (runtime_url, _) = proxima_pg_testkit::split_role_urls(&db.name)
        .await
        .expect("split roles");
    let runtime = sqlx::PgPool::connect(&runtime_url)
        .await
        .expect("runtime pool");
    let group = OwnerId::from(GroupId::new(owner));
    let mut both = vec![fact, abstraction];
    both.sort();
    for (role, readable) in [
        (Role::ingest(), vec![fact]),
        (
            Role::new(
                AccessCeiling::Abstraction,
                AccessCeiling::Abstraction,
                false,
            )
            .expect("role"),
            both,
        ),
    ] {
        let authz = proxima_core::test_fixtures::authenticated_context(
            AuthzContext::for_subject_with_role(
                UserId::new(uuid::Uuid::now_v7()),
                [(group, role)],
                AuthPath::HostBearer,
            ),
        );
        let scope = authz.owner_scope().expect("authenticated scope");
        let mut tx = proxima_storage_pg::begin_owner_transaction(&runtime, scope)
            .await
            .expect("owner scope");
        let visible: Vec<uuid::Uuid> =
            sqlx::query_scalar("SELECT memory_id FROM proxima_code.projection ORDER BY memory_id")
                .fetch_all(&mut *tx)
                .await
                .expect("projection read");
        assert_eq!(visible, readable, "{role:?} reads");
        let written =
            sqlx::query("UPDATE proxima_code.projection SET tag = tag WHERE memory_id = ANY($1)")
                .bind(vec![fact, abstraction])
                .execute(&mut *tx)
                .await
                .expect("projection write");
        assert_eq!(
            written.rows_affected(),
            readable.len() as u64,
            "{role:?} writes"
        );
        tx.rollback().await.expect("rollback");
    }
    runtime.close().await;
}

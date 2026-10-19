//! The narrowly scoped candidate authority is the only runtime definer exception.

use proxima_storage_pg::assert_runtime_rls;
use proxima_storage_pg::search_candidates::ARGUMENT_TYPES;
use proxima_storage_pg::test_fixtures::fresh_pg;
use sqlx::{AssertSqlSafe, PgPool};

use super::{assert_rls_refusal, execute, quoted_identifier};

async fn assert_refused(runtime: &PgPool, reason: &str) {
    let error = assert_runtime_rls(runtime, &["proxima_core"])
        .await
        .expect_err("unsafe candidate authority must fail boot")
        .to_string();
    assert!(error.contains(reason), "{error}");
}

#[tokio::test]
async fn runtime_cannot_assume_or_replace_candidate_authority() {
    let (pg, _database) = fresh_pg("candidate_runtime_authority").await;
    let admin = pg.pool_for_tests();
    let runtime = pg.clone_pool_for_backend();
    assert_runtime_rls(&runtime, &["proxima_core"])
        .await
        .unwrap();
    execute(admin, "CREATE FUNCTION proxima_core.search_lexical_candidates(integer) RETURNS integer LANGUAGE sql SECURITY DEFINER SET search_path = pg_catalog, pg_temp AS 'SELECT $1'").await;
    execute(
        admin,
        "REVOKE ALL ON FUNCTION proxima_core.search_lexical_candidates(integer) FROM PUBLIC",
    )
    .await;
    execute(
        admin,
        "ALTER FUNCTION proxima_core.search_lexical_candidates(integer) OWNER TO proxima_search",
    )
    .await;
    assert_refused(&runtime, "search_lexical_candidates").await;
    execute(
        admin,
        "DROP FUNCTION proxima_core.search_lexical_candidates(integer)",
    )
    .await;
    let error = sqlx::query("SET ROLE proxima_search")
        .execute(&runtime)
        .await
        .expect_err("runtime cannot assume the candidate role");
    assert_rls_refusal(&error);
    // SQL-POLICY: fixed-fragment — the signature is the fixed candidate contract.
    let replacement = format!(
        "CREATE OR REPLACE FUNCTION proxima_core.search_lexical_candidates({ARGUMENT_TYPES}) \
         RETURNS TABLE(t uuid, schema_id text, lexical_score real) \
         LANGUAGE sql AS 'SELECT NULL::uuid, NULL::text, NULL::real WHERE false'"
    );
    // SQL-POLICY: fixed-fragment — the replacement above has no caller-controlled SQL.
    let error = sqlx::query(AssertSqlSafe(replacement))
        .execute(&runtime)
        .await
        .expect_err("runtime cannot replace a candidate function");
    assert_rls_refusal(&error);
}

#[tokio::test]
async fn candidate_definers_reject_public_execution_and_hijacked_paths() {
    let (pg, _database) = fresh_pg("candidate_definer_catalog").await;
    let admin = pg.pool_for_tests();
    let runtime = pg.clone_pool_for_backend();
    let signature = format!("proxima_core.search_lexical_candidates({ARGUMENT_TYPES})");
    // SQL-POLICY: fixed-fragment — fixed function name and identity arguments.
    execute(
        admin,
        &format!("GRANT EXECUTE ON FUNCTION {signature} TO PUBLIC"),
    )
    .await;
    assert_refused(&runtime, "PUBLIC EXECUTE").await;
    // SQL-POLICY: fixed-fragment — fixed function name and identity arguments.
    execute(
        admin,
        &format!("REVOKE EXECUTE ON FUNCTION {signature} FROM PUBLIC"),
    )
    .await;
    // SQL-POLICY: fixed-fragment — fixed function identity and path values.
    execute(
        admin,
        &format!("ALTER FUNCTION {signature} SET search_path = pg_temp, pg_catalog"),
    )
    .await;
    assert_refused(&runtime, "search_path").await;
    // SQL-POLICY: fixed-fragment — restores the generated function's exact path.
    execute(
        admin,
        &format!("ALTER FUNCTION {signature} SET search_path = pg_catalog, pg_temp"),
    )
    .await;
    assert_runtime_rls(&runtime, &["proxima_core"])
        .await
        .unwrap();
}

#[tokio::test]
async fn candidate_policy_is_read_only_and_requires_a_projection_shape() {
    let (pg, _database) = fresh_pg("candidate_policy_catalog").await;
    let admin = pg.pool_for_tests();
    let runtime = pg.clone_pool_for_backend();
    let platform: String = sqlx::query_scalar(
        "SELECT pg_get_userbyid(relowner) FROM pg_class WHERE oid = 'proxima_core.memory'::regclass",
    ).fetch_one(admin).await.unwrap();
    execute(admin, "CREATE TABLE proxima_core.document_search(memory_id uuid PRIMARY KEY REFERENCES proxima_core.memory(t), search_tsv tsvector NOT NULL)").await;
    // SQL-POLICY: fixed-fragment — quoted catalog role name is the existing table owner.
    execute(
        admin,
        &format!(
            "ALTER TABLE proxima_core.document_search OWNER TO {}",
            quoted_identifier(&platform)
        ),
    )
    .await;
    execute(admin, "SELECT proxima_core.owner_rls_apply_class('test', 'memory_owner', 'proxima_core', 'document_search', 'memory_owner', NULL)").await;
    execute(admin, "CREATE POLICY proxima_search_candidates ON proxima_core.document_search FOR SELECT TO proxima_search USING (true)").await;
    assert_runtime_rls(&runtime, &["proxima_core"])
        .await
        .unwrap();
    execute(admin, "CREATE POLICY proxima_search_candidates ON proxima_core.memory FOR SELECT TO proxima_search USING (true)").await;
    assert_refused(
        &runtime,
        "proxima_core.memory has an invalid proxima_search_candidates",
    )
    .await;
    execute(
        admin,
        "DROP POLICY proxima_search_candidates ON proxima_core.memory",
    )
    .await;
    execute(
        admin,
        "DROP POLICY proxima_search_candidates ON proxima_core.document_search",
    )
    .await;
    execute(admin, "CREATE POLICY proxima_search_candidates ON proxima_core.document_search FOR ALL TO proxima_search USING (true) WITH CHECK (true)").await;
    assert_refused(
        &runtime,
        "document_search has an invalid proxima_search_candidates",
    )
    .await;
    execute(
        admin,
        "DROP POLICY proxima_search_candidates ON proxima_core.document_search",
    )
    .await;
    execute(
        admin,
        "GRANT UPDATE ON proxima_core.memory TO proxima_search",
    )
    .await;
    assert_refused(&runtime, "read-only role").await;
    execute(
        admin,
        "REVOKE UPDATE ON proxima_core.memory FROM proxima_search",
    )
    .await;
    execute(
        admin,
        "GRANT CREATE ON SCHEMA proxima_core TO proxima_search",
    )
    .await;
    assert_refused(&runtime, "read-only role").await;
    execute(
        admin,
        "REVOKE CREATE ON SCHEMA proxima_core FROM proxima_search",
    )
    .await;
    assert_runtime_rls(&runtime, &["proxima_core"])
        .await
        .unwrap();
}

//! Apply core + flavor migrations to a fresh DB and verify the
//! current `proxima_code` schema shape.

use proxima_pg_testkit::{create_db, db_url, drop_db, unique_db_name};
use proxima_storage_pg::PgStorage;

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn flavor_migrations_apply_to_fresh_db() {
    let db_name = unique_db_name("proxima_test");
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?; // core
        proxima_code::migrator().run(pg.pool_for_tests()).await?; // flavor

        // Verify the flavor sidecar tables exist.
        for table in [
            "commit_v1",
            "file_revision_v1",
            "code_chunk_v1",
            "commit_summary_v1",
            "work_requested_v1",
            "test_requested_v1",
            "test_requested_criterion_v1",
            "acceptance_criteria_v1",
            "acceptance_criterion_v1",
            "execution_plan_v1",
            "execution_plan_item_v1",
            "execution_result_v1",
            "test_result_v1",
            "acceptance_verification_v1",
            "acceptance_summary_v1",
            // Retired: call sites live on the chunk payload; work
            // assignment is a node.
            "code_chunk_call_v1",
            "work_assignment_v1",
        ] {
            let row = sqlx::query(
                "SELECT 1 AS ok FROM information_schema.tables
                 WHERE table_schema = 'proxima_code' AND table_name = $1",
            )
            .bind(table)
            .fetch_optional(pg.pool_for_tests())
            .await?;
            assert!(row.is_some(), "expected table proxima_code.{table}");
        }
        // The workspace-runner subsystem is gone (drop_workspace_mode /
        // drop_workspace_review); its tables must not survive a fresh apply.
        for dropped in [
            "workspace_run_v1",
            "workspace_decision_v1",
            "workspace_review_v1",
            // The typed edge sidecar. A flavor cannot declare one any more,
            // and this one could never hold a second call site anyway.
            "code_calls_v1",
        ] {
            let row = sqlx::query(
                "SELECT 1 AS ok FROM information_schema.tables
                 WHERE table_schema = 'proxima_code' AND table_name = $1",
            )
            .bind(dropped)
            .fetch_optional(pg.pool_for_tests())
            .await?;
            assert!(row.is_none(), "proxima_code.{dropped} should be dropped");
        }

        // Verify the M5 core tables exist.
        for table in ["memory", "memory_head", "announce"] {
            let row = sqlx::query(
                "SELECT 1 AS ok FROM information_schema.tables
                 WHERE table_schema = 'proxima_core' AND table_name = $1",
            )
            .bind(table)
            .fetch_optional(pg.pool_for_tests())
            .await?;
            assert!(row.is_some(), "expected table proxima_core.{table}");
        }

        for (table, column) in [
            ("repos", "owner_kind"),
            ("repo_ingestion_runs", "owner_kind"),
            ("repo_ingestion_runs", "status"),
            ("repo_ingestion_runs", "stage"),
            ("file_revision_v1", "state"),
            ("code_chunk_v1", "state"),
            ("execution_plan_item_v1", "kind"),
            ("execution_result_v1", "status"),
            ("test_result_v1", "status"),
            ("acceptance_verification_v1", "status"),
        ] {
            assert_enum_column(pg.pool_for_tests(), "proxima_code", table, column).await?;
        }

        assert_owner_ref_constraints(pg.pool_for_tests()).await?;

        for table in [
            "code_chunk_v1",
            "file_revision_v1",
            "commit_v1",
            "commit_summary_v1",
        ] {
            let present: bool = sqlx::query_scalar(
                "SELECT EXISTS (
                     SELECT 1 FROM information_schema.columns
                      WHERE table_schema = 'proxima_code'
                        AND table_name = $1
                        AND column_name = 'embed_text'
                 )",
            )
            .bind(table)
            .fetch_one(pg.pool_for_tests())
            .await?;
            assert!(present, "W6: {table}.embed_text must exist");
        }

        // After the Owner = OwnerRef collapse, the full-collapse decision
        // removes the legacy owner org column from proxima_code too. Keystone gate for the
        // flavor DDL-drop migration — a missed column would silently keep org
        // in the flavor schema and pass every check above.
        let org_cols: (i64,) = sqlx::query_as(
            "SELECT count(*)::bigint FROM information_schema.columns \
             WHERE table_schema = 'proxima_code' AND column_name = ('owner_' || 'org_id')",
        )
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(
            org_cols.0, 0,
            "legacy owner org column must be absent from proxima_code after Owner=OwnerRef collapse; found {}",
            org_cols.0
        );

        // Idempotency — a second run must not error.
        proxima_code::migrator().run(pg.pool_for_tests()).await?;

        let handle = uuid::Uuid::now_v7();
        let memory_id = uuid::Uuid::now_v7();
        let repo_id = uuid::Uuid::now_v7();
        let owner_id = uuid::Uuid::now_v7();
        sqlx::query(
            "INSERT INTO proxima_core.owners (owner_id, kind)
             VALUES ($1, 'personal') ON CONFLICT DO NOTHING",
        )
        .bind(owner_id)
        .execute(pg.pool_for_tests())
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
             VALUES ($1, 'abstraction', 'proxima-code/code-chunk-v1', $2, $3)",
        )
        .bind(handle)
        .bind(owner_id)
        .bind(memory_id)
        .execute(pg.pool_for_tests())
        .await?;
        let mut hash = [0_u8; 32];
        hash[..16].copy_from_slice(memory_id.as_bytes());
        hash[16..].copy_from_slice(memory_id.as_bytes());
        let content_id: uuid::Uuid = sqlx::query_scalar(
            "INSERT INTO proxima_core.content (owner_id, schema_id, content_hash)
             VALUES ($1, 'proxima-code/code-chunk-v1', $2)
             RETURNING content_id",
        )
        .bind(owner_id)
        .bind(hash.as_slice())
        .fetch_one(pg.pool_for_tests())
        .await?;
        let fact_handle = uuid::Uuid::now_v7();
        let fact_t = uuid::Uuid::now_v7();
        sqlx::query(
            "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
             VALUES ($1, 'fact', 'core/test-fact-v1', $2, $3)",
        )
        .bind(fact_handle)
        .bind(owner_id)
        .bind(fact_t)
        .execute(pg.pool_for_tests())
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.memory (handle, t, kind, owner_id, schema_id)
             VALUES ($1, $2, 'fact', $3, 'core/test-fact-v1')",
        )
        .bind(fact_handle)
        .bind(fact_t)
        .bind(owner_id)
        .execute(pg.pool_for_tests())
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.memory
                (handle, t, kind, owner_id, schema_id, origins, content_id)
             VALUES ($1, $2, 'abstraction', $3, 'proxima-code/code-chunk-v1', ARRAY[$4]::uuid[], $5)",
        )
        .bind(handle)
        .bind(memory_id)
        .bind(owner_id)
        .bind(fact_t)
        .bind(content_id)
        .execute(pg.pool_for_tests())
        .await?;
        sqlx::query(
            "INSERT INTO proxima_code.code_chunk_v1
                 (t, repo_id, file_path, chunk_index, text, chunk_type,
                  byte_range_start, byte_range_end, line_range_start, line_range_end, state)
             VALUES ($1, $2, 'src/lib.rs', 0, 'fn main() {}', 'file',
                     0, 12, 1, 1, 'Present')",
        )
        .bind(memory_id)
        .bind(repo_id)
        .execute(pg.pool_for_tests())
        .await?;
        let err = sqlx::query(
            "UPDATE proxima_code.code_chunk_v1 SET text = 'rewritten' WHERE t = $1",
        )
        .bind(memory_id)
        .execute(pg.pool_for_tests())
        .await
        .expect_err("code_chunk_v1 content rewrite must be rejected");
        assert!(
            err.to_string().contains("append-only"),
            "expected append-only rejection, got: {err}"
        );

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("flavor_migrations_apply_to_fresh_db failed");
}

/// `OwnerRef` has no id-less kind, so the flavor's owner columns carry the
/// invariant as plain NOT NULL rather than the old shape/world CHECK pair.
async fn assert_owner_ref_constraints(
    pool: &sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    for table in ["repos", "repo_ingestion_runs"] {
        for column in ["owner_kind", "owner_id"] {
            let nullable: String = sqlx::query_scalar(
                "SELECT is_nullable
                   FROM information_schema.columns
                  WHERE table_schema = 'proxima_code'
                    AND table_name = $1
                    AND column_name = $2",
            )
            .bind(table)
            .bind(column)
            .fetch_one(pool)
            .await?;
            assert_eq!(
                nullable, "NO",
                "proxima_code.{table}.{column} must be NOT NULL"
            );
        }
        let dead: Vec<String> = sqlx::query_scalar(
            "SELECT constraint_name::text
               FROM information_schema.table_constraints
              WHERE table_schema = 'proxima_code'
                AND table_name = $1
                AND constraint_type = 'CHECK'
                AND constraint_name LIKE '%world%'",
        )
        .bind(table)
        .fetch_all(pool)
        .await?;
        assert!(
            dead.is_empty(),
            "proxima_code.{table} must carry no World CHECK: {dead:?}"
        );
    }
    Ok(())
}

async fn assert_enum_column(
    pool: &sqlx::PgPool,
    schema: &str,
    table: &str,
    column: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let typtype: Option<String> = sqlx::query_scalar(
        "SELECT t.typtype::text
           FROM pg_attribute a
           JOIN pg_class c ON c.oid = a.attrelid
           JOIN pg_namespace n ON n.oid = c.relnamespace
           JOIN pg_type t ON t.oid = a.atttypid
          WHERE n.nspname = $1
            AND c.relname = $2
            AND a.attname = $3
            AND NOT a.attisdropped",
    )
    .bind(schema)
    .bind(table)
    .bind(column)
    .fetch_optional(pool)
    .await?;

    assert_eq!(
        typtype.as_deref(),
        Some("e"),
        "expected {schema}.{table}.{column} to be a SQL enum"
    );
    Ok(())
}

/// The code baseline carries the generator's output verbatim.
///
/// Same pin as core's `generator_output_is_the_migration_text`, on the
/// other side of the flavor boundary: `projection_artifacts` is run over
/// the code flavor's OWN contract, and the baseline has to contain what it
/// emits, character for character.
#[test]
fn generator_output_is_the_code_baseline_text() {
    let artifacts = proxima_storage_pg::projection::projection_artifacts(
        &proxima_code::contract::CODE_FLAVOR_CONTRACT,
    )
    .expect("code artifacts")
    .expect("the code flavor declares a projection");
    let baseline = include_str!("../migrations/20260818000020_v008_baseline.sql");
    for statement in artifacts.forward() {
        assert!(
            baseline.contains(statement),
            "the code baseline does not carry the generator's output verbatim:\n{statement}"
        );
    }
}

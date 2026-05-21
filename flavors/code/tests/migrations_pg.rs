//! Apply core + flavor migrations to a fresh DB and verify the
//! current `proxima_code` schema shape.

use proxima_storage_pg::PgStorage;
use sqlx::{Connection, Executor, PgConnection};
use uuid::Uuid;

const ADMIN_URL: &str = "postgres://proxima:proxima@localhost/proxima";

async fn create_db(name: &str) -> Result<(), sqlx::Error> {
    let mut conn = PgConnection::connect(ADMIN_URL).await?;
    conn.execute(format!("CREATE DATABASE \"{name}\"").as_str())
        .await?;
    conn.close().await?;
    Ok(())
}

async fn drop_db(name: &str) -> Result<(), sqlx::Error> {
    let mut conn = PgConnection::connect(ADMIN_URL).await?;
    conn.execute(format!("DROP DATABASE IF EXISTS \"{name}\"").as_str())
        .await?;
    conn.close().await?;
    Ok(())
}

#[tokio::test]
async fn flavor_migrations_apply_to_fresh_db() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = format!("postgres://proxima:proxima@localhost/{db_name}");

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?; // core
        proxima_code::migrator().run(pg.pool()).await?; // flavor

        // Verify the flavor sidecar tables exist.
        for table in [
            "commit_v1",
            "file_revision_v1",
            "code_chunk_v1",
            "commit_summary_v1",
            "workspace_decision_v1",
            "execution_request_v1",
            "test_request_v1",
            "workspace_review_v1",
        ] {
            let row = sqlx::query(
                "SELECT 1 AS ok FROM information_schema.tables
                 WHERE table_schema = 'proxima_code' AND table_name = $1",
            )
            .bind(table)
            .fetch_optional(pg.pool())
            .await?;
            assert!(row.is_some(), "expected table proxima_code.{table}");
        }
        let old_run = sqlx::query(
            "SELECT 1 AS ok FROM information_schema.tables
             WHERE table_schema = 'proxima_code' AND table_name = 'workspace_run_v1'",
        )
        .fetch_optional(pg.pool())
        .await?;
        assert!(
            old_run.is_none(),
            "proxima_code.workspace_run_v1 should be dropped"
        );

        // Verify the M5 core tables exist.
        for table in [
            "source_batch_f2a",
            "edges",
            "embeddings",
            "workspace_run_v1",
        ] {
            let row = sqlx::query(
                "SELECT 1 AS ok FROM information_schema.tables
                 WHERE table_schema = 'proxima_core' AND table_name = $1",
            )
            .bind(table)
            .fetch_optional(pg.pool())
            .await?;
            assert!(row.is_some(), "expected table proxima_core.{table}");
        }

        for (table, column) in [
            ("repos", "owner_principal_kind"),
            ("repo_ingestion_runs", "owner_principal_kind"),
            ("repo_ingestion_runs", "status"),
            ("repo_ingestion_runs", "stage"),
            ("file_revision_v1", "state"),
            ("code_chunk_v1", "state"),
            ("workspace_decision_v1", "decision"),
            ("workspace_review_v1", "verdict"),
        ] {
            assert_enum_column(pg.pool(), "proxima_code", table, column).await?;
        }

        // Idempotency — a second run must not error.
        proxima_code::migrator().run(pg.pool()).await?;

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("flavor_migrations_apply_to_fresh_db failed");
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

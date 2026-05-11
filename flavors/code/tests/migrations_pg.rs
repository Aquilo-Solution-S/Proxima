//! Apply core + flavor migrations to a fresh DB and verify the
//! `proxima_code` schema and tables exist.

use proxima_storage_pg::PgStorage;
use sqlx::{Connection, Executor, PgConnection};
use uuid::Uuid;

const ADMIN_URL: &str = "postgres://postgres@localhost/postgres";

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
    if create_db(&db_name).await.is_err() {
        eprintln!("skipping (no admin PG)");
        return;
    }
    let url = format!("postgres://postgres@localhost/{db_name}");

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
            "workspace_run_v1",
            "workspace_decision_v1",
            "execution_request_v1",
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

        // Verify the M5 core tables exist.
        for table in ["source_batch_f2a", "edges", "embeddings"] {
            let row = sqlx::query(
                "SELECT 1 AS ok FROM information_schema.tables
                 WHERE table_schema = 'proxima_core' AND table_name = $1",
            )
            .bind(table)
            .fetch_optional(pg.pool())
            .await?;
            assert!(row.is_some(), "expected table proxima_core.{table}");
        }

        // Idempotency — a second run must not error.
        proxima_code::migrator().run(pg.pool()).await?;

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("flavor_migrations_apply_to_fresh_db failed");
}

//! Boot a fresh transient DB, apply migrations, assert
//! tables exist, drop the DB. Requires admin access to a
//! local PG cluster (<postgres://postgres@localhost>).

mod common;

use common::{create_db, db_url, drop_db};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

#[tokio::test]
async fn migrations_apply_to_fresh_db() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());

    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }

    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;

        let row: (i64,) = sqlx::query_as(
            "SELECT count(*)::bigint FROM information_schema.tables \
             WHERE table_schema = 'proxima_core'",
        )
        .fetch_one(pg.pool())
        .await?;
        assert!(
            row.0 >= 7,
            "expected >=7 tables in proxima_core, got {}",
            row.0
        );
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("migrations integration test failed");
}

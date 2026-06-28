//! Boot a fresh transient DB, apply migrations, assert
//! tables exist, drop the DB. Requires admin access to a
//! local PG cluster (<postgres://postgres@localhost>).

use crate::common::{create_db, db_url, drop_db};
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

        // S0 (Owner = OwnerRef collapse, Track B): owner_org_id must be GONE
        // from every proxima_core table. This is the keystone gate for the
        // DDL-drop migration — a single missed column would silently keep org
        // in storage and pass the table-count check above.
        let org_cols: (i64,) = sqlx::query_as(
            "SELECT count(*)::bigint FROM information_schema.columns \
             WHERE table_schema = 'proxima_core' AND column_name = 'owner_org_id'",
        )
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(
            org_cols.0, 0,
            "owner_org_id must be absent from proxima_core after S0; found {} column(s)",
            org_cols.0
        );
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("migrations integration test failed");
}

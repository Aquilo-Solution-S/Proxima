//! The shipped store-wide list against the migrated core schema (#478).
//!
//! Every surface enumerator (erase partition, export bundle, transfer sweep)
//! walks DECLARED surfaces, so a forgotten table is invisible to all of
//! them. This starts at the catalog instead: every `proxima_core` base table
//! is a flavor #0 surface or a `STORE_WIDE_TABLES` entry with a reason, and
//! the list names nothing stale and nothing declared.

use std::collections::BTreeSet;

use proxima_pg_testkit::{db_url, drop_db};
use proxima_storage_pg::test_fixtures::create_core_db;
use proxima_storage_pg::{PgStorage, STORE_WIDE_TABLES};
use uuid::Uuid;

#[tokio::test]
async fn every_core_base_table_is_declared_or_store_wide() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let surfaces = pg.surfaces();
        let declared: BTreeSet<&str> = surfaces
            .surfaces()
            .iter()
            .map(|surface| surface.table)
            .collect();
        let listed: BTreeSet<&str> = STORE_WIDE_TABLES.iter().map(|(table, _)| *table).collect();
        assert_eq!(
            listed.len(),
            STORE_WIDE_TABLES.len(),
            "STORE_WIDE_TABLES names a table twice"
        );
        for (table, why) in STORE_WIDE_TABLES {
            assert!(
                why.len() > 40,
                "{table}'s entry needs a reason, not a placeholder"
            );
            assert!(
                !declared.contains(table),
                "{table} is a declared surface; drop it from STORE_WIDE_TABLES"
            );
        }

        let present: Vec<String> = sqlx::query_scalar(
            "SELECT table_schema || '.' || table_name
               FROM information_schema.tables
              WHERE table_schema = 'proxima_core'
                AND table_type = 'BASE TABLE'
                AND table_name <> '_sqlx_migrations'
              ORDER BY 1",
        )
        .fetch_all(pg.pool_for_tests())
        .await?;
        assert!(
            present.len() > 20,
            "core schema too small: {}",
            present.len()
        );

        let undeclared: Vec<&String> = present
            .iter()
            .filter(|table| !declared.contains(table.as_str()) && !listed.contains(table.as_str()))
            .collect();
        assert!(
            undeclared.is_empty(),
            "no erase, export or transfer can see these core tables: {undeclared:?}. \
             Declare a flavor #0 Surface, or add a STORE_WIDE_TABLES entry saying why not."
        );
        let stale: Vec<&&str> = listed
            .iter()
            .filter(|table| !present.iter().any(|row| row == *table))
            .collect();
        assert!(
            stale.is_empty(),
            "STORE_WIDE_TABLES names relations the schema does not have: {stale:?}"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("every_core_base_table_is_declared_or_store_wide failed");
}

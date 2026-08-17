//! Commit/summary search_tsv is stored and GIN-indexed.
#![allow(clippy::doc_markdown)]

use proxima_code::payloads::{CommitSummaryV1, CommitV1};
use proxima_core::{AbstractionPayload, FactPayload};
use proxima_pg_testkit::{create_db, db_url, drop_db, unique_db_name};
use proxima_storage_pg::PgStorage;

#[tokio::test]
async fn commit_search_tsv_is_stored_and_indexed() {
    let db_name = unique_db_name("proxima_test");
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        proxima_code::migrator().run(pg.pool_for_tests()).await?;

        let commit = CommitV1::search_projection().expect("commits project for search");
        assert_eq!(commit.tsv_column, Some("search_tsv"));
        let summary = CommitSummaryV1::search_projection().expect("summaries project for search");
        assert_eq!(summary.tsv_column, Some("search_tsv"));

        for table in ["commit_v1", "commit_summary_v1"] {
            let generated: bool = sqlx::query_scalar(
                "SELECT is_generated = 'ALWAYS'
                   FROM information_schema.columns
                  WHERE table_schema = 'proxima_code'
                    AND table_name = $1
                    AND column_name = 'search_tsv'",
            )
            .bind(table)
            .fetch_one(pg.pool_for_tests())
            .await?;
            assert!(generated, "{table}.search_tsv must be generated");
            let gin: bool = sqlx::query_scalar(
                "SELECT EXISTS (
                     SELECT 1
                       FROM pg_indexes
                      WHERE schemaname = 'proxima_code'
                        AND tablename = $1
                        AND indexdef ILIKE '%gin%search_tsv%'
                 )",
            )
            .bind(table)
            .fetch_one(pg.pool_for_tests())
            .await?;
            assert!(gin, "{table} needs a GIN index on search_tsv");
        }
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("commit search_tsv migration failed");
}

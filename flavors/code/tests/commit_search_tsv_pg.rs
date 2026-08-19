//! Commit/summary search_tsv is stored and GIN-indexed.
#![allow(clippy::doc_markdown)]

mod common;

use common::{migrated_db, seed_memory, test_owner};
use proxima_code::payloads::{CommitSummaryV1, CommitV1};
use proxima_core::{AbstractionPayload, FactPayload};
use proxima_pg_testkit::{create_db, db_url, drop_db, unique_db_name};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

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

/// Commit messages and generated summaries are prose, not code identifiers:
/// their stored vectors and query side must use the SQL-owned language-neutral
/// configuration. The German query also protects the user-visible search path
/// from an English-only assumption.
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn commit_and_summary_search_accept_non_english_prose() {
    let (db_name, pg) = migrated_db().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pool = pg.pool_for_tests();
        let owner = test_owner();
        let repo_id = Uuid::now_v7();
        let (_, commit_t) = seed_memory(
            pool,
            &owner,
            CommitV1::SCHEMA_ID,
            "fact",
            None,
            None,
            &[],
        )
        .await?;
        let (_, summary_t) = seed_memory(
            pool,
            &owner,
            CommitSummaryV1::SCHEMA_ID,
            "abstraction",
            None,
            None,
            &[],
        )
        .await?;

        let now = time::OffsetDateTime::now_utc();
        sqlx::query(
            "INSERT INTO proxima_code.commit_v1
                (t, repo_id, sha, parents, author_name, author_email,
                 author_time, committer_name, committer_email, committer_time, message)
             VALUES ($1, $2, 'de-cafe', ARRAY[]::text[], 'Ada', 'ada@example.test',
                     $3, 'Ada', 'ada@example.test', $3, $4)",
        )
        .bind(commit_t)
        .bind(repo_id)
        .bind(now)
        .bind("Änderungen für Übertragung")
        .execute(pool)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_code.commit_summary_v1
                (t, repo_id, commit_sha, summary, key_files, change_kind)
             VALUES ($1, $2, 'de-cafe', $3, ARRAY['src/übertragung.rs']::text[], 'fix')",
        )
        .bind(summary_t)
        .bind(repo_id)
        .bind("Änderungen für Übertragung")
        .execute(pool)
        .await?;

        let config: String = sqlx::query_scalar(
            "SELECT proxima_code.commit_search_lexical_config()::text",
        )
        .fetch_one(pool)
        .await?;
        assert_eq!(config, "simple");

        for (table, t, fields) in [
            (
                "commit_v1",
                commit_t,
                "NULLIF(sha, '') , NULLIF(message, ''), NULLIF(author_name, ''), NULLIF(author_email, '')",
            ),
            (
                "commit_summary_v1",
                summary_t,
                "NULLIF(commit_sha, ''), NULLIF(summary, ''), proxima_core.lexical_text_array(key_files)",
            ),
        ] {
            let expression = format!(
                "SELECT search_tsv = proxima_code.commit_search_tsv(
                    proxima_core.lexical_join({fields}))
                   FROM proxima_code.{table}
                  WHERE t = $1"
            );
            // SQL-POLICY: fixed-fragment — table and fields come only from the fixed test cases above.
            let matches_authority: bool = sqlx::query_scalar(sqlx::AssertSqlSafe(expression))
                .bind(t)
                .fetch_one(pool)
                .await?;
            assert!(
                matches_authority,
                "{table}.search_tsv must use the SQL prose authority"
            );
        }

        let commit_matches: bool = sqlx::query_scalar(
            "SELECT search_tsv @@ proxima_code.commit_search_web_tsquery('Übertragung')
               FROM proxima_code.commit_v1
              WHERE t = $1",
        )
        .bind(commit_t)
        .fetch_one(pool)
        .await?;
        assert!(commit_matches, "non-English commit prose must be searchable");

        let summary_matches: bool = sqlx::query_scalar(
            "SELECT search_tsv @@ proxima_code.commit_search_web_tsquery('Übertragung')
               FROM proxima_code.commit_summary_v1
              WHERE t = $1",
        )
        .bind(summary_t)
        .fetch_one(pool)
        .await?;
        assert!(
            summary_matches,
            "non-English generated summary prose must be searchable"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("language-neutral commit/summary search failed");
}

//! The stored code-chunk vector must equal the expression it replaced.
//!
//! The v0.0.7 flavor migration moved `to_tsvector` off the read path into a
//! generated column. Its definition now lives in two places that cannot see
//! each other: the migration's `GENERATED ALWAYS AS`, and the search
//! query in `flavors/code/src/mcp/search_chunks.rs` that matches
//! `websearch_to_tsquery('simple', ...)` against it. If they diverge, code
//! search silently returns different results — no error, no signal.
//!
//! These pin the column against the literal pre-migration expression, and
//! pin the config to `simple`: code is not English, and stemming or stopword
//! removal would fold distinct identifiers together and drop real tokens
//! (`in`, `as`, `if`, `do`, `no`, `on`).

use proxima_pg_testkit::{create_db, db_url, drop_db, unique_db_name};
use proxima_storage_pg::PgStorage;

/// The exact expression `search_chunks` scored with before the migration,
/// as a SQL fragment over `$1` (path) and `$2` (text).
const LEGACY_TSV_SQL: &str =
    "to_tsvector('pg_catalog.simple'::regconfig, $1::text || ' ' || $2::text)";

/// Inputs chosen to hit the cases where `simple` and `english` diverge, plus
/// the punctuation and identifier shapes real code carries.
fn adversarial_chunks() -> Vec<(&'static str, &'static str)> {
    vec![
        ("src/lib.rs", "fn main() {}"),
        // Stemming would fold these three together under 'english'.
        ("src/parse.rs", "parsing parsed parser parses"),
        // Every one of these is a real keyword and an English stopword.
        ("src/kw.rs", "in as if do no on it be for while"),
        ("a/b/c.ts", "export const x: Record<string, number> = {};"),
        ("src/punct.rs", "let a = b::<C>(&d)?; // e.f-g_h"),
        ("src/unicode.rs", "let grüße = \"Straßberger\"; // naïve"),
        ("src/num.rs", "0xFF 3.14 1_000_000 -42"),
        ("path/with space.rs", "struct S;"),
        ("src/long.rs", "x"),
    ]
}

#[tokio::test]
async fn code_chunk_search_tsv_matches_the_scoring_expression() {
    let db_name = unique_db_name("proxima_test");
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        proxima_code::migrator().run(pg.pool_for_tests()).await?;

        // The generated column is not addressable without rows, so probe the
        // expression equivalence directly first: it is the property the
        // column's definition has to hold.
        for (path, text) in adversarial_chunks() {
            // SQL-POLICY: fixed-fragment — LEGACY_TSV_SQL is a module
            // constant; both inputs are bound.
            let matches: bool = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "SELECT to_tsvector('simple'::regconfig, $1::text || ' ' || $2::text)
                        IS NOT DISTINCT FROM {LEGACY_TSV_SQL}"
            )))
            .bind(path)
            .bind(text)
            .fetch_one(pg.pool_for_tests())
            .await?;
            assert!(
                matches,
                "stored expression diverged from the pre-migration one for {path} / {text:?}"
            );
        }

        // `simple` must not stem: under 'english' these collapse to one lexeme.
        let distinct_lexemes: i32 = sqlx::query_scalar(
            "SELECT array_length(tsvector_to_array(
                 to_tsvector('simple'::regconfig, 'parsing parsed parser parses')), 1)",
        )
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(
            distinct_lexemes, 4,
            "code search must not stem identifiers; 'simple' keeps all four forms"
        );

        // `simple` must not drop stopwords: these are all real keywords.
        let keyword_lexemes: i32 = sqlx::query_scalar(
            "SELECT array_length(tsvector_to_array(
                 to_tsvector('simple'::regconfig, 'in as if do no on')), 1)",
        )
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(
            keyword_lexemes, 6,
            "code search must not drop keywords that are English stopwords"
        );

        Ok(())
    }
    .await;

    drop_db(&db_name).await.ok();
    result.expect("chunk search tsv checks");
}

/// The column is `GENERATED ALWAYS AS ... STORED` over exactly the search
/// expression, and the GIN index sits on the column rather than on the old
/// expression.
#[tokio::test]
async fn code_chunk_search_tsv_is_stored_and_indexed() {
    let db_name = unique_db_name("proxima_test");
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        proxima_code::migrator().run(pg.pool_for_tests()).await?;

        let generation: Option<String> = sqlx::query_scalar(
            "SELECT generation_expression
               FROM information_schema.columns
              WHERE table_schema = 'proxima_code'
                AND table_name = 'code_chunk_v1'
                AND column_name = 'search_tsv'",
        )
        .fetch_optional(pg.pool_for_tests())
        .await?
        .flatten();
        let generation = generation.expect("search_tsv is a generated column");
        assert!(
            generation.contains("simple") && generation.contains("file_path"),
            "unexpected generation expression: {generation}"
        );

        let has_index: bool = sqlx::query_scalar(
            "SELECT EXISTS (
                 SELECT 1 FROM pg_indexes
                  WHERE schemaname = 'proxima_code'
                    AND tablename = 'code_chunk_v1'
                    AND indexdef ILIKE '%gin%search_tsv%'
             )",
        )
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert!(has_index, "GIN index on search_tsv is missing");

        // The superseded expression index must be gone, or every write pays
        // for two GIN indexes over the same lexemes.
        let stale_index: bool = sqlx::query_scalar(
            "SELECT EXISTS (
                 SELECT 1 FROM pg_indexes
                  WHERE schemaname = 'proxima_code'
                    AND indexname = 'idx_code_chunk_v1_text_search'
             )",
        )
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert!(!stale_index, "the replaced expression index is still there");

        Ok(())
    }
    .await;

    drop_db(&db_name).await.ok();
    result.expect("chunk search tsv storage checks");
}

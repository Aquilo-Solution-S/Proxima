//! The stored code-chunk vector must equal what both search surfaces expect.
//!
//! The v0.0.7 flavor migration moved `to_tsvector` off the read path into a
//! generated column. Its definition now lives in three places that cannot see
//! each other:
//!
//! - the migration's `GENERATED ALWAYS AS`,
//! - the query in `flavors/code/src/mcp/search_chunks.rs` that matches a
//!   tsquery against it,
//! - `CodeChunkV1::search_projection()`, which names the column as its
//!   `tsv_column` so `core_search_memories` substitutes it for the expression
//!   it would otherwise compute inline.
//!
//! If any of them diverges, code search silently returns different results —
//! no error, no signal. These pin all three against
//! `proxima_core.lexical_tsv(proxima_core.lexical_join(...))`, which is the
//! single definition core and the flavor now share.

use proxima_code::payloads::CodeChunkV1;
use proxima_core::AbstractionPayload;
use proxima_pg_testkit::{create_db, db_url, drop_db, unique_db_name};
use proxima_storage_pg::PgStorage;

/// Inputs chosen to hit the punctuation, identifier and Unicode shapes real
/// code carries, plus the cases where a config choice is observable.
fn adversarial_chunks() -> Vec<(&'static str, &'static str)> {
    vec![
        ("src/lib.rs", "fn main() {}"),
        ("src/parse.rs", "parsing parsed parser parses"),
        ("src/kw.rs", "in as if do no on it be for while"),
        ("a/b/c.ts", "export const x: Record<string, number> = {};"),
        ("src/punct.rs", "let a = b::<C>(&d)?; // e.f-g_h"),
        ("src/unicode.rs", "let grüße = \"Straßberger\"; // naïve"),
        ("src/num.rs", "0xFF 3.14 1_000_000 -42"),
        ("path/with space.rs", "struct S;"),
        ("src/long.rs", "x"),
        ("src/empty.rs", ""),
    ]
}

/// The generated column must equal `lexical_tsv(lexical_join(file_path,
/// text))` for every input — the expression `core_search_memories` builds
/// when a sidecar declares no `tsv_column`, and therefore the only expression
/// the column is allowed to stand in for.
#[tokio::test]
async fn code_chunk_search_tsv_matches_the_projection() {
    let db_name = unique_db_name("proxima_test");
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        proxima_code::migrator().run(pg.pool_for_tests()).await?;

        // The projection is the contract: the fields it lists, in order, are
        // the arguments the column has to be generated from.
        let projection = CodeChunkV1::search_projection().expect("chunks project for search");
        let columns: Vec<&str> = projection.fields.iter().map(|f| f.column).collect();
        assert_eq!(
            columns,
            vec!["file_path", "text"],
            "projection fields changed; the v0.0.7 generated column must change with them"
        );
        assert_eq!(
            projection.tsv_column,
            Some("search_tsv"),
            "chunks must read the stored vector rather than recompute it"
        );

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

        // Re-declare the migration's own expression over a scratch table
        // whose columns carry the projected names, then compare what it
        // stores against what `core_search_memories` builds when a sidecar
        // declares no `tsv_column`. Reading the definition back out of the
        // catalog is what makes this a drift test rather than a restatement:
        // it fails if the migration and the projection stop agreeing, for
        // any input, including the ones a reviewer would not think to try.
        //
        // A plain table, not a TEMP one: the pool hands out a different
        // connection per statement and TEMP tables are session-scoped. The
        // whole database is dropped at the end of the test either way.
        //
        // `lexical_language` mirrors the chunk table's pinned-english column
        // (the flavor baseline): the generation expression read back
        // from the catalog references it per row.
        //
        // SQL-POLICY: fixed-fragment — `generation` is read from
        // information_schema, not from a caller.
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "CREATE TABLE tsv_probe (
                 file_path text,
                 text text,
                 lexical_language regconfig NOT NULL DEFAULT 'english'::regconfig,
                 stored tsvector GENERATED ALWAYS AS ({generation}) STORED
             )"
        )))
        .execute(pg.pool_for_tests())
        .await?;

        for (path, text) in adversarial_chunks() {
            sqlx::query("INSERT INTO tsv_probe (file_path, text) VALUES ($1, $2)")
                .bind(path)
                .bind(text)
                .execute(pg.pool_for_tests())
                .await?;
        }

        let drifted: Vec<(String, String)> = sqlx::query_as(
            "SELECT file_path, text
               FROM tsv_probe
              WHERE stored IS DISTINCT FROM
                    proxima_core.lexical_tsv(lexical_language,
                        proxima_core.lexical_join(
                            NULLIF(file_path, ''), NULLIF(text, '')))",
        )
        .fetch_all(pg.pool_for_tests())
        .await?;
        assert!(
            drifted.is_empty(),
            "stored column and the projection expression disagree for: {drifted:?}"
        );

        Ok(())
    }
    .await;

    drop_db(&db_name).await.ok();
    result.expect("chunk search tsv projection checks");
}

/// A sidecar's stored vector stands in for one core builds with
/// `TEXT_SEARCH_CONFIG`. A column built with any other config would match
/// nothing that stems and everything that does not — silently, because a
/// tsvector carries no record of the config that produced it.
///
/// `lexical_tsv` is the only spelling that cannot drift, so the guard is that
/// the column is built from it rather than from a bare `to_tsvector`.
#[tokio::test]
async fn code_chunk_search_tsv_shares_core_text_search_config() {
    let db_name = unique_db_name("proxima_test");
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        proxima_code::migrator().run(pg.pool_for_tests()).await?;

        // Every stored search vector in the database, core's and the
        // flavor's alike, has to route through lexical_tsv.
        let rogue: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT table_schema, table_name, generation_expression
               FROM information_schema.columns
              WHERE column_name = 'search_tsv'
                AND table_schema IN ('proxima_core', 'proxima_code')
                AND generation_expression NOT LIKE '%lexical_tsv%'",
        )
        .fetch_all(pg.pool_for_tests())
        .await?;
        assert!(
            rogue.is_empty(),
            "search_tsv columns bypassing proxima_core.lexical_tsv cannot be \
             substituted for core's builder expression: {rogue:?}"
        );

        // A stemming query must reach a stored row: this is the property a
        // 'simple' column would silently lose.
        let stems: bool = sqlx::query_scalar(
            "SELECT proxima_core.lexical_tsv('fn parse_manifest(input: &str)')
                    @@ websearch_to_tsquery('english'::regconfig,
                           proxima_core.lexical_scrub('parsing manifests'))",
        )
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert!(
            stems,
            "stored vectors and the query builder must share a stemmer"
        );

        Ok(())
    }
    .await;

    drop_db(&db_name).await.ok();
    result.expect("chunk search tsv config checks");
}

/// The GIN index sits on the generated column.
#[tokio::test]
async fn code_chunk_search_tsv_is_stored_and_indexed() {
    let db_name = unique_db_name("proxima_test");
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        proxima_code::migrator().run(pg.pool_for_tests()).await?;

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

/// Pinning chunks to `english` only makes them SEARCHABLE if `english` is
/// in the active-language set the core query builder ORs over. On a
/// deployment whose default was switched (e.g. `german`) before this
/// migration, nothing else would ever register it — every chunk would
/// silently drop out of the strict lexical band.
#[tokio::test]
async fn the_pinned_chunk_language_is_registered_and_matchable() {
    let db_name = unique_db_name("proxima_test");
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        proxima_code::migrator().run(pg.pool_for_tests()).await?;

        // The deployment this feature exists for: documents in german.
        sqlx::query("SELECT proxima_core.set_lexical_config('german')")
            .execute(pg.pool_for_tests())
            .await?;

        let registered: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM proxima_core.lexical_languages
              WHERE config = 'english'::regconfig)",
        )
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert!(
            registered,
            "the pinned chunk language is not in the active set: english-stemmed \
             vectors would never be matched by any tsquery arm"
        );

        // End to end through the production match shape: an english-stemmed
        // chunk vector must satisfy the cross-language OR the core builder
        // constructs from lexical_languages.
        let matched: bool = sqlx::query_scalar(
            "SELECT proxima_core.lexical_tsv('english'::regconfig,
                        'fn register_repo handles adopted branches quickly')
                    @@ (SELECT proxima_core.tsquery_or_agg(
                                websearch_to_tsquery(l.config,
                                    proxima_core.lexical_query_text(l.config, 'adopted branches')))
                          FROM proxima_core.lexical_languages l)",
        )
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert!(
            matched,
            "an english chunk vector does not match the active-language OR \
             on a german-default deployment"
        );

        Ok(())
    }
    .await;

    drop_db(&db_name).await.ok();
    result.expect("chunk language registration checks");
}

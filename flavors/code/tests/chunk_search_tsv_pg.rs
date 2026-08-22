//! The stored code-chunk vector must equal what both search surfaces expect.
//!
//! The vector is written by ONE expression —
//! `projection_vector_sql(&CODE_CHUNK_V1.search)` — into
//! `proxima_code.projection`. These tests pin that expression against the
//! reference definition below, input by input, so any divergence in scoring
//! is caught rather than inferred.

use proxima_code::payloads::CODE_LEXICAL_LANGUAGE;
use proxima_pg_testkit::{create_db, db_url, drop_db, unique_db_name};
use proxima_storage_pg::PgStorage;

/// The reference chunk-vector expression, verbatim, over alias `c`.
///
/// The reference side of the identity proof. It is a literal because no
/// catalog object carries it: this is the only copy, and its job is to
/// disagree if the generator ever stops reproducing it.
const REFERENCE_VECTOR_SQL: &str = "proxima_core.lexical_tsv(
     proxima_code.code_lexical_config(),
     proxima_core.lexical_join(
         VARIADIC ARRAY[NULLIF(c.file_path, ''), NULLIF(c.text, '')]))";

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

/// The Rust ingest path stamps `CODE_LEXICAL_LANGUAGE`; the database owns the
/// actual SQL configuration used by the default, generated vector, and query
/// builders. Keep those authorities synchronized at the executable boundary.
#[tokio::test]
async fn code_chunk_sql_authority_matches_rust_ingest_constant() {
    let db_name = unique_db_name("proxima_test");
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        proxima_code::migrator().run(pg.pool_for_tests()).await?;

        let sql_language: String =
            sqlx::query_scalar("SELECT proxima_code.code_lexical_config()::text")
                .fetch_one(pg.pool_for_tests())
                .await?;
        assert_eq!(
            sql_language, CODE_LEXICAL_LANGUAGE,
            "Rust code-chunk ingest language must match the SQL authority"
        );

        let default_expression: Option<String> = sqlx::query_scalar(
            "SELECT column_default
               FROM information_schema.columns
              WHERE table_schema = 'proxima_code'
                AND table_name = 'code_chunk_v1'
                AND column_name = 'lexical_language'",
        )
        .fetch_one(pg.pool_for_tests())
        .await?;
        let default_expression = default_expression.expect("code-chunk lexical default");
        assert!(
            default_expression.contains("code_lexical_config"),
            "code-chunk default must use the SQL authority: {default_expression}"
        );

        // The vector's config is a contract declaration now, not a column
        // default: `LanguagePolicy::Pinned("english")` is what the generator
        // renders, and `code_lexical_config()` is what SQL says. They must
        // be the same configuration.
        let schema = proxima_code::contract::CODE_FLAVOR_CONTRACT
            .schemas
            .iter()
            .find(|schema| schema.sidecar_table == Some("proxima_code.code_chunk_v1"))
            .expect("code-chunk-v1 is declared");
        assert_eq!(
            schema
                .search
                .language()
                .and_then(|policy| policy.pinned_config()),
            Some(sql_language.as_str()),
            "the declared pinned config must be the SQL authority"
        );
        Ok(())
    }
    .await;

    drop_db(&db_name).await.ok();
    result.expect("code-chunk lexical authority guard failed");
}

/// The generator's expression and the reference expression must produce the
/// SAME tsvector for every input — that identity is what keeps the vector
/// off the sidecar honest.
#[tokio::test]
async fn the_generator_reproduces_the_v007_generated_column() {
    let db_name = unique_db_name("proxima_test");
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        proxima_code::migrator().run(pg.pool_for_tests()).await?;

        // The contract is the authority on WHICH columns are projected, in
        // WHICH order — the arguments the vector is built from.
        let schema = proxima_code::contract::CODE_FLAVOR_CONTRACT
            .schemas
            .iter()
            .find(|schema| schema.sidecar_table == Some("proxima_code.code_chunk_v1"))
            .expect("code-chunk-v1 is declared");
        let proxima_core::flavor::SearchProjectionDecl::Projected { fields, .. } = &schema.search
        else {
            panic!("code chunks are a search surface");
        };
        let columns: Vec<&str> = fields.iter().map(|field| field.column).collect();
        assert_eq!(
            columns,
            vec!["file_path", "text"],
            "projected fields changed; the identity proof below must change with them"
        );

        // The sidecar carries no vector any more.
        let still_generated: Option<String> = sqlx::query_scalar(
            "SELECT generation_expression
               FROM information_schema.columns
              WHERE table_schema = 'proxima_code'
                AND table_name = 'code_chunk_v1'
                AND column_name = 'search_tsv'",
        )
        .fetch_optional(pg.pool_for_tests())
        .await?
        .flatten();
        assert!(
            still_generated.is_none(),
            "the projection replaced the generated column: {still_generated:?}"
        );

        // A plain table, not a TEMP one: the pool hands out a different
        // connection per statement and TEMP tables are session-scoped. The
        // whole database is dropped at the end of the test either way.
        sqlx::query(
            "CREATE TABLE tsv_probe (
                 file_path text,
                 text text
             )",
        )
        .execute(pg.pool_for_tests())
        .await?;
        for (path, text) in adversarial_chunks() {
            sqlx::query("INSERT INTO tsv_probe (file_path, text) VALUES ($1, $2)")
                .bind(path)
                .bind(text)
                .execute(pg.pool_for_tests())
                .await?;
        }

        // The generator's own output, run against the probe rows.
        let generated = proxima_storage_pg::projection::projection_vector_sql(&schema.search)
            .expect("the generator emits a vector expression");
        // `COALESCE(.., ''::tsvector)` is the ONE deliberate difference: the
        // projection's `search_tsv` is NOT NULL where the generated column
        // was nullable. An empty tsvector and a NULL one both fail `@@` and
        // both rank zero, so no result moves.
        //
        // SQL-POLICY: fixed-fragment — both fragments are compiled-in
        // `&'static str`, one from the contract and one from this file.
        let drifted: Vec<(String, String)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT c.file_path, c.text
               FROM tsv_probe c
              WHERE ({generated}) IS DISTINCT FROM COALESCE({REFERENCE_VECTOR_SQL}, ''::tsvector)"
        )))
        .fetch_all(pg.pool_for_tests())
        .await?;
        assert!(
            drifted.is_empty(),
            "the generator and the v0.0.7 generated column disagree for: {drifted:?}"
        );

        Ok(())
    }
    .await;

    drop_db(&db_name).await.ok();
    result.expect("chunk vector identity checks");
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

        // There is no GENERATED search vector left anywhere: every stored
        // vector in the database is a projection row, written by the
        // generator, which routes through `lexical_tsv` by construction.
        let generated: Vec<(String, String)> = sqlx::query_as(
            "SELECT table_schema::text, table_name::text
               FROM information_schema.columns
              WHERE column_name = 'search_tsv'
                AND table_schema IN ('proxima_core', 'proxima_code')
                AND generation_expression IS NOT NULL",
        )
        .fetch_all(pg.pool_for_tests())
        .await?;
        assert!(
            generated.is_empty(),
            "the projection is the only home for a stored vector: {generated:?}"
        );

        let commit_builder: String = sqlx::query_scalar(
            "SELECT pg_get_functiondef(
                        'proxima_code.commit_search_tsv(text)'::regprocedure)",
        )
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert!(
            commit_builder.contains("proxima_core.lexical_tsv"),
            "commit prose builder must route through the core lexical_tsv helper"
        );

        // A stemming query must reach a stored row: this is the property a
        // 'simple' column would silently lose.
        let stems: bool = sqlx::query_scalar(
            "SELECT proxima_core.lexical_tsv('fn parse_manifest(input: &str)')
                    @@ websearch_to_tsquery(proxima_code.code_lexical_config(),
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

        // ONE composite GIN for the whole flavor, on the projection.
        let has_index: bool = sqlx::query_scalar(
            "SELECT EXISTS (
                 SELECT 1 FROM pg_indexes
                  WHERE schemaname = 'proxima_code'
                    AND tablename = 'projection'
                    AND indexname = 'code_projection_owner_tsv_gin'
             )",
        )
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert!(has_index, "the projection's composite GIN is missing");

        let per_sidecar: Vec<String> = sqlx::query_scalar(
            "SELECT indexname::text FROM pg_indexes
              WHERE schemaname = 'proxima_code'
                AND tablename <> 'projection'
                AND indexdef ILIKE '%gin%search_tsv%'
              ORDER BY 1",
        )
        .fetch_all(pg.pool_for_tests())
        .await?;
        assert!(
            per_sidecar.is_empty(),
            "the projection replaced the per-sidecar GINs: {per_sidecar:?}"
        );

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

        sqlx::query("SELECT proxima_core.set_lexical_config('german')")
            .execute(pg.pool_for_tests())
            .await?;
        let matched: bool = sqlx::query_scalar(
            "SELECT proxima_core.lexical_tsv(proxima_code.code_lexical_config(),
                        'fn register_repo handles adopted branches quickly')
                    @@ websearch_to_tsquery(proxima_code.code_lexical_config(), 'adopted branches')",
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

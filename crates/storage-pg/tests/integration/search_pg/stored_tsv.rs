//! The stored lexical vectors must equal the expression they replaced.
//!
//! Migration 0011 moved `to_tsvector` off the read path and into generated
//! columns. The definition now lives in two places that cannot see each
//! other: SQL (the generated column) and the Rust query builder (the
//! fallback for sidecars with no stored column). If they ever disagree, a
//! memory silently scores differently depending on which table it lives
//! in — no error, just wrong results. These tests pin both against the
//! literal expression the builder computed before 0011.
//!
//! 0012 added the third place they can disagree: *which* text-search
//! configuration both sides use. The last two tests pin that the switch
//! moves every stored vector and the query side together.

use crate::common::{drop_db, fresh_pg, owner_fixture};
use proxima_core::storage_ports::*;
use proxima_core::verbs::query::EntityKind;

use super::{insert_search_abstraction, insert_text_memory, lexical_request};

/// The exact tsvector expression the lexical branch inlined before 0011,
/// as a SQL fragment over `$1`.
const LEGACY_TSV_SQL: &str = "to_tsvector(
     'english',
     regexp_replace(
         regexp_replace($1, '[[:punct:]]+', ' ', 'g'),
         '\\m[[:alnum:]]{255}[[:alnum:]]+\\M',
         ' ',
         'g'
     )
 )";

/// Inputs chosen to hit every branch of the scrub: punctuation runs, the
/// over-long-token cut, stopwords, stemming, unicode, and the empty and
/// whitespace-only cases that decide NULL-vs-empty-vector.
fn adversarial_texts() -> Vec<String> {
    vec![
        String::new(),
        "   ".to_string(),
        "plain text".to_string(),
        "Hello, World! -- it's a test...".to_string(),
        "adopted adopting adopts".to_string(),
        "what is my the a of".to_string(),
        format!("prefix {} suffix", "x".repeat(300)),
        format!("edge {} edge", "y".repeat(255)),
        "e-mail user@example.com http://host/path?q=1".to_string(),
        "Grüße Straßberger naïve".to_string(),
        "tabs\tand\nnewlines".to_string(),
        "127.0.0.1 3.14 -42".to_string(),
    ]
}

#[tokio::test]
async fn lexical_tsv_function_matches_the_inlined_expression()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;

    for text in adversarial_texts() {
        // SQL-POLICY: fixed-fragment
        let matches: bool = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT proxima_core.lexical_tsv($1) IS NOT DISTINCT FROM {LEGACY_TSV_SQL}"
        )))
        .bind(&text)
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert!(
            matches,
            "lexical_tsv diverged from the pre-0011 expression for {text:?}"
        );
    }

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn stored_memory_tsv_matches_the_projected_search_text()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;

    let owner = owner_fixture();
    for text in adversarial_texts() {
        if text.trim().is_empty() {
            // The base branch filters empty text out before it can be a
            // candidate; the column is still generated, just unreachable.
            continue;
        }
        insert_text_memory(&pg, &owner, &text).await?;
        insert_search_abstraction(&pg, &owner, &text, None).await?;
    }

    // memories.search_tsv is generated from COALESCE(text, ''), which is
    // exactly what the base candidate branch projects as search_text.
    let mismatches: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM proxima_core.memories m
          WHERE m.search_tsv IS DISTINCT FROM
                proxima_core.lexical_tsv(COALESCE(m.text, ''))",
    )
    .fetch_one(pg.pool_for_tests())
    .await?;
    assert_eq!(mismatches, 0, "stored memories.search_tsv drifted");

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn stored_sidecar_tsv_matches_the_projection_concatenation()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;

    let owner = owner_fixture();
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(&owner);
    for (idx, text) in adversarial_texts().into_iter().enumerate() {
        let memory_id = uuid::Uuid::now_v7();
        sqlx::query(
            "INSERT INTO proxima_core.memories
                (memory_id, owner_kind, owner_id, schema_id, schema_version, kind, text,
                 operator_kind, operator_id, input_contract_id, source_batch_id,
                 model_id, prompt_version)
             VALUES ($1, $2, $3, 'proxima/agent-derivation-v1', 1, 'Abstraction', $4, 'AtoA',
                     '00000000-0000-0000-0000-000000000331'::uuid,
                     '00000000-0000-0000-0000-000000000332'::uuid, NULL,
                     'test-model', 'test-v1')",
        )
        .bind(memory_id)
        .bind(owner_kind)
        .bind(owner_id)
        .bind(format!("body {idx}"))
        .execute(pg.pool_for_tests())
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.agent_derivation_v1
                (memory_id, title, body, tags, source_memory_ids, model_id,
                 client_name, client_version)
             VALUES ($1, $2, $3, $4, ARRAY[]::uuid[], 'test-model', 'test', '1')",
        )
        .bind(memory_id)
        .bind(format!("title {idx}"))
        // body carries the adversarial text; title and tags stay well-formed
        // so the row still satisfies the nonempty check constraints.
        .bind(format!("body {idx} {text}"))
        .bind(vec![format!("tag{idx}"), "shared, tag".to_string()])
        .execute(pg.pool_for_tests())
        .await?;
    }

    // The generated column must equal lexical_tsv over the same
    // concat_ws the sidecar candidate branch emits as search_text.
    let mismatches: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM proxima_core.agent_derivation_v1 d
          WHERE d.search_tsv IS DISTINCT FROM proxima_core.lexical_tsv(
                NULLIF(concat_ws(' ',
                    NULLIF(d.title, ''),
                    NULLIF(d.body, ''),
                    NULLIF(array_to_string(d.tags, ' '), '')), ''))",
    )
    .fetch_one(pg.pool_for_tests())
    .await?;
    assert_eq!(
        mismatches, 0,
        "stored agent_derivation_v1.search_tsv drifted"
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

/// Switching the configuration must rewrite every stored vector generated
/// from it — including sidecars in schemas core has never heard of.
///
/// Redefining `lexical_config()` by hand does not: `PostgreSQL` permits the
/// replacement and leaves stored generated columns untouched, so rows
/// written before the switch keep their old tokenisation while rows written
/// after get the new one. Nothing raises an error, and half the corpus stops
/// being reachable by the other half's queries. `set_lexical_config` exists
/// to make that impossible, and it finds its work by introspection because
/// core cannot enumerate flavor tables.
#[tokio::test]
async fn switching_the_lexical_config_rebuilds_every_stored_vector()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;

    // Stand in for a flavor sidecar: a generated column in a schema the core
    // migration does not know exists.
    sqlx::query("CREATE SCHEMA proxima_flavorish")
        .execute(pg.pool_for_tests())
        .await?;
    sqlx::query(
        "CREATE TABLE proxima_flavorish.chunk_v1 (
             id integer PRIMARY KEY,
             body text NOT NULL,
             search_tsv tsvector
                 GENERATED ALWAYS AS (proxima_core.lexical_tsv(body)) STORED
         )",
    )
    .execute(pg.pool_for_tests())
    .await?;
    sqlx::query("INSERT INTO proxima_flavorish.chunk_v1 (id, body) VALUES (1, $1)")
        .bind("Die Bauleitung überwacht die Ausführung")
        .execute(pg.pool_for_tests())
        .await?;

    let owner = owner_fixture();
    insert_text_memory(&pg, &owner, "Die Bauleitung überwacht die Ausführung").await?;

    let english: String =
        sqlx::query_scalar("SELECT search_tsv::text FROM proxima_flavorish.chunk_v1 WHERE id = 1")
            .fetch_one(pg.pool_for_tests())
            .await?;
    assert!(
        english.contains("'die'"),
        "english indexes the German stopword `die` as a content word: {english}"
    );

    let rebuilt: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT rebuilt_schema, rebuilt_table, rebuilt_column
           FROM proxima_core.set_lexical_config('german')",
    )
    .fetch_all(pg.pool_for_tests())
    .await?;
    assert!(
        rebuilt
            .iter()
            .any(|(s, t, _)| s == "proxima_flavorish" && t == "chunk_v1"),
        "the switch skipped a generated column outside proxima_core: {rebuilt:#?}"
    );
    assert!(
        rebuilt
            .iter()
            .any(|(s, t, _)| s == "proxima_core" && t == "memories"),
        "the switch skipped memories.search_tsv: {rebuilt:#?}"
    );

    let german: String =
        sqlx::query_scalar("SELECT search_tsv::text FROM proxima_flavorish.chunk_v1 WHERE id = 1")
            .fetch_one(pg.pool_for_tests())
            .await?;
    assert!(
        !german.contains("'die'") && german.contains("'bauleit'"),
        "the pre-switch row kept its english vector: {german}"
    );

    // No column may disagree with the function after the switch — that is
    // exactly the split-brain state a manual redefinition produces.
    let drifted: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM proxima_core.memories m
          WHERE m.search_tsv IS DISTINCT FROM
                proxima_core.lexical_tsv(COALESCE(m.text, ''))",
    )
    .fetch_one(pg.pool_for_tests())
    .await?;
    assert_eq!(
        drifted, 0,
        "stored memories.search_tsv drifted after the switch"
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

/// The query side must follow the switch in the same step.
///
/// This is the property that makes the configuration safe to change at all:
/// the builder emits `proxima_core.lexical_config()` rather than a literal,
/// so the tsquery is always built in the configuration the stored vectors
/// were built in. Before 0012 the two were independent literals in a
/// migration and a Rust constant.
#[tokio::test]
async fn the_query_side_follows_a_switched_lexical_config() -> Result<(), Box<dyn std::error::Error>>
{
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;

    let owner = owner_fixture();
    let memory_id =
        insert_text_memory(&pg, &owner, "Die Bauleitung überwacht die Ausführung").await?;

    // `Bauleitungen` shares no literal token with the text; only a German
    // stemmer conflates it with `Bauleitung`. The substring arm cannot
    // rescue it either — `bauleitungen` is not a substring of the body.
    let mut request = lexical_request(&owner, "Bauleitungen");
    request.kind = Some(EntityKind::Abstraction);

    let before = pg.search_memories(&request, &[]).await?.results;
    assert!(
        before.is_empty(),
        "english matched an inflected German form it cannot stem: {before:#?}"
    );

    sqlx::query("SELECT proxima_core.set_lexical_config('german')")
        .execute(pg.pool_for_tests())
        .await?;

    let after = pg.search_memories(&request, &[]).await?.results;
    assert_eq!(
        after.first().map(|row| row.memory_id.into_inner()),
        Some(memory_id),
        "german did not match the inflected form after the switch: {after:#?}"
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

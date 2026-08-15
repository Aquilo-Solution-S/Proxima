//! Lexical-branch search behaviour: projection scope, stemming, and the OR rescue arm.

use super::{
    code_chunk_projection, drop_db, fresh_pg, ingest_fact_memory, insert_search_abstraction,
    lexical_request, owner_fixture,
};

use proxima_core::storage_ports::*;
use proxima_core::verbs::query::EntityKind;

#[tokio::test]
async fn lexical_search_ignores_unprojected_code_chunk_text()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;
    sqlx::query("CREATE SCHEMA IF NOT EXISTS proxima_code")
        .execute(pg.pool_for_tests())
        .await?;
    sqlx::query(
        "CREATE TABLE proxima_code.code_chunk_v1 (
             memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
             file_path text NOT NULL,
             language text,
             chunk_type text NOT NULL,
             text text NOT NULL
         )",
    )
    .execute(pg.pool_for_tests())
    .await?;

    let owner = owner_fixture();
    let chunk_id =
        ingest_fact_memory(&pg, &owner, "proxima-code/code-chunk-v1", b"code-chunk").await?;
    let long_token = format!("{}rawonlyneedle", "a".repeat(300));
    sqlx::query(
        "INSERT INTO proxima_code.code_chunk_v1
            (memory_id, file_path, language, chunk_type, text)
         VALUES ($1, 'src/search.rs', 'rust', 'function', $2)",
    )
    .bind(chunk_id.into_inner())
    .bind(long_token)
    .execute(pg.pool_for_tests())
    .await?;

    let projections = vec![code_chunk_projection()];
    let rows = pg
        .search_memories(&lexical_request(&owner, "rawonlyneedle"), &projections)
        .await?
        .results;
    assert!(rows.is_empty(), "{rows:#?}");

    let rows = pg
        .search_memories(&lexical_request(&owner, "src search"), &projections)
        .await?
        .results;
    assert_eq!(rows.first().map(|row| row.memory_id), Some(chunk_id));
    assert!(!rows[0].snippet.contains("rawonlyneedle"));

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn lexical_search_ignores_sidecar_without_projection()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;
    sqlx::query("CREATE SCHEMA IF NOT EXISTS proxima_test")
        .execute(pg.pool_for_tests())
        .await?;
    sqlx::query(
        "CREATE TABLE proxima_test.unprojected_v1 (
             memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
             secret text NOT NULL
         )",
    )
    .execute(pg.pool_for_tests())
    .await?;

    let owner = owner_fixture();
    let memory_id =
        ingest_fact_memory(&pg, &owner, "proxima-test/unprojected-v1", b"unprojected").await?;
    sqlx::query("INSERT INTO proxima_test.unprojected_v1 (memory_id, secret) VALUES ($1, $2)")
        .bind(memory_id.into_inner())
        .bind("hidden payload phrase")
        .execute(pg.pool_for_tests())
        .await?;

    let rows = pg
        .search_memories(&lexical_request(&owner, "hidden payload"), &[])
        .await?
        .results;
    assert!(rows.is_empty(), "{rows:#?}");

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn lexical_search_stems_and_ignores_stopwords() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;
    let owner = owner_fixture();

    let adopted =
        insert_search_abstraction(&pg, &owner, "we adopted two kittens yesterday", None).await?;

    // Stemming ('english' config): the query's inflection must not matter —
    // "adopting a kitten" reaches "adopted two kittens".
    let mut inflected = lexical_request(&owner, "adopting a kitten");
    inflected.kind = Some(EntityKind::Abstraction);
    let page = pg.search_memories(&inflected, &[]).await?;
    assert!(
        page.results
            .iter()
            .any(|row| row.memory_id.into_inner() == adopted),
        "stemmed query must match the inflected document"
    );

    // Stopword removal: a natural-language question matches on its content
    // words; its function words ("when", "were", "the") must not be
    // required to appear literally under websearch AND semantics. Under
    // the previous 'simple' config this exact shape returned nothing.
    let mut question = lexical_request(&owner, "when were the kittens adopted?");
    question.kind = Some(EntityKind::Abstraction);
    let page = pg.search_memories(&question, &[]).await?;
    assert!(
        page.results
            .iter()
            .any(|row| row.memory_id.into_inner() == adopted),
        "natural-language question must match via its content words"
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn lexical_or_rescue_matches_partial_content_words() -> Result<(), Box<dyn std::error::Error>>
{
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;
    let owner = owner_fixture();

    let full =
        insert_search_abstraction(&pg, &owner, "we adopted two kittens from the shelter", None)
            .await?;
    let partial =
        insert_search_abstraction(&pg, &owner, "the kittens slept all afternoon", None).await?;

    // "adopted kittens shelter": the partial document holds only one of the
    // three content lexemes, so strict websearch AND semantics exclude it.
    // The OR-rescue arm must surface it — scored below the full match, above
    // the 0.25 substring floor.
    let mut req = lexical_request(&owner, "adopted kittens from a shelter");
    req.kind = Some(EntityKind::Abstraction);
    let page = pg.search_memories(&req, &[]).await?;
    let score_of = |id: uuid::Uuid| {
        page.results
            .iter()
            .find(|row| row.memory_id.into_inner() == id)
            .map(|row| row.score)
    };
    let full_score = score_of(full).expect("full AND match present");
    let partial_score = score_of(partial).expect("partial match rescued by OR arm");
    // Disjoint score bands: strict [0.5, 1.0] > rescue (0.25, 0.45] > 0.25.
    // Match tier dominates cover-density rank by construction, so a strict
    // match can never rank below a rescue no matter how ts_rank_cd falls.
    assert!(
        full_score >= 0.5,
        "strict match must land in the strict band: {full_score}"
    );
    assert!(
        full_score > partial_score,
        "strict match must outrank rescue: {full_score} vs {partial_score}"
    );
    assert!(
        partial_score > 0.25 && partial_score <= 0.45 + 1.0e-6,
        "rescue score sits between substring floor and strict band: {partial_score}"
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

/// Inside the rescue band, a document that repeats one query word must not
/// outrank one that contains more distinct query words.
///
/// Cover density rewards a short span holding several query terms, and
/// repetitive structured data wins that trivially. Length normalisation
/// separates the two, so the rescue arm ranks with `ts_rank(v, q, 1|32)`.
#[tokio::test]
async fn rescue_ranks_distinct_terms_above_one_word_repeated()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;
    let owner = owner_fixture();

    // Two of the three content lexemes, in one short sentence.
    let precise =
        insert_search_abstraction(&pg, &owner, "the kittens found a shelter", None).await?;
    // One lexeme, sixty times, in a document twenty times longer — the
    // schema.json shape.
    let repetitive = insert_search_abstraction(
        &pg,
        &owner,
        &format!("kittens {}", "kittens ".repeat(60)),
        None,
    )
    .await?;

    let mut req = lexical_request(&owner, "adopted kittens from a shelter");
    req.kind = Some(EntityKind::Abstraction);
    let page = pg.search_memories(&req, &[]).await?;
    let score_of = |id: uuid::Uuid| {
        page.results
            .iter()
            .find(|row| row.memory_id.into_inner() == id)
            .map(|row| row.score)
    };
    let precise_score = score_of(precise).expect("precise match present");
    let repetitive_score = score_of(repetitive).expect("repetitive match present");
    assert!(
        precise_score > repetitive_score,
        "two distinct query words must outrank one word repeated sixty times: \
         {precise_score} vs {repetitive_score}"
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

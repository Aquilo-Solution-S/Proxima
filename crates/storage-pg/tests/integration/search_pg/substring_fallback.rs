//! The substring band after the lexical branch was split in two.
//!
//! v0.0.8 moved the tsquery gate onto the base tables so migration 0019's
//! GIN indexes can serve it, which meant taking `LIKE '%…%'` out of the
//! same disjunction — no core index can serve that arm, and one unservable
//! arm in an `OR` costs the whole statement its index path. The band now
//! has three homes, and this file is about the seam between them:
//!
//! - rows that also match a tsquery keep it implicitly, because the flat
//!   `0.25` can never beat the band they already scored in;
//! - rows the semantic leg returns get it from that leg's own statement,
//!   evaluated over rows it was reading anyway;
//! - rows that are *only* substring matches need the fallback statement,
//!   which runs only when the candidates in hand cannot already fill the
//!   page above the band.
//!
//! The risk the split introduces is silent: a page that is one row short
//! of what it used to hold, on a query nobody runs in a test. So these
//! tests are all about the third case, and each one is built so the skip
//! decision actually fires rather than being trivially true on three rows.

use super::{
    drop_db, fresh_pg, hybrid_request, insert_embedded_memory, insert_search_abstraction,
    lexical_request, owner_fixture, padded_embedding,
};

use proxima_core::storage_ports::*;
use proxima_core::verbs::query::{EntityKind, SearchOrder};

/// Seeds `count` rows that all match the tsquery `term` strongly, so the
/// page can be filled from the strict band alone.
async fn seed_strict_matches(
    pg: &proxima_storage_pg::PgStorage,
    owner: &proxima_core::OwnerRef,
    term: &str,
    count: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    for idx in 0..count {
        insert_search_abstraction(pg, owner, &format!("{term} record number {idx}"), None).await?;
    }
    Ok(())
}

/// A substring-only row that cannot outrank the page is absent from it —
/// which is what the skipped fallback has to reproduce, not merely what it
/// happens to do.
///
/// The row matches `LIKE '%loorgrai%'` and no tsquery, so it scores the
/// flat substring band. The corpus around it holds far more strict-band
/// rows than the page is wide, so under relevance order the band cannot
/// reach the page however the query is executed. Before the split the
/// statement read it and the top-N sort dropped it; now the statement is
/// never issued. Same page, and this pins that it is the same page.
#[tokio::test]
async fn a_substring_only_row_that_cannot_rank_stays_off_the_page()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;
    let owner = owner_fixture();

    seed_strict_matches(&pg, &owner, "loorgrai", 40).await?;
    let substring_only =
        insert_search_abstraction(&pg, &owner, "prefix xloorgraix suffix", None).await?;

    let mut req = lexical_request(&owner, "loorgrai");
    req.kind = Some(EntityKind::Abstraction);
    req.limit = 10;
    let page = pg.search_memories(&req, &[]).await?;

    assert_eq!(page.results.len(), 10);
    assert!(
        page.results
            .iter()
            .all(|row| row.memory_id.into_inner() != substring_only),
        "a substring-only row reached a page already full of strict-band rows"
    );
    assert!(
        page.results.iter().all(|row| row.score > 0.25),
        "every row on this page should have outranked the substring band"
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

/// The same row on the same corpus, once the page can no longer be filled
/// without it.
///
/// This is the case the fallback exists for, and the one an "index-first,
/// drop the substring arm" design would lose. `xloorgraix` contains the
/// query as an infix and produces no matching lexeme, so nothing but the
/// substring predicate can find it.
#[tokio::test]
async fn a_substring_only_row_is_read_when_the_page_needs_it()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;
    let owner = owner_fixture();

    // Fewer strict matches than the page is wide, so the fallback must run.
    seed_strict_matches(&pg, &owner, "loorgrai", 3).await?;
    let substring_only =
        insert_search_abstraction(&pg, &owner, "prefix xloorgraix suffix", None).await?;

    let mut req = lexical_request(&owner, "loorgrai");
    req.kind = Some(EntityKind::Abstraction);
    req.limit = 10;
    let page = pg.search_memories(&req, &[]).await?;

    let found = page
        .results
        .iter()
        .find(|row| row.memory_id.into_inner() == substring_only)
        .expect("the infix match is the only thing the substring band can find");
    assert!(
        (found.score - 0.25).abs() <= 1.0e-6,
        "substring-only rows score the flat band: {}",
        found.score
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

/// A query with no lexemes at all leaves the substring predicate as the
/// only arm there is, however full the corpus looks.
///
/// `websearch_to_tsquery` returns an empty tsquery for a query of pure
/// stopwords, so the gate admits nothing and the fallback carries the
/// whole search. The builder's own doc comment cites this case as the
/// reason the substring arm exists; it would be the first thing an
/// empty-tsquery-only gate broke, and the first thing a skip rule that
/// counted rows rather than scores broke.
#[tokio::test]
async fn an_all_stopword_query_is_answered_by_the_substring_band()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;
    let owner = owner_fixture();

    seed_strict_matches(&pg, &owner, "loorgrai", 40).await?;
    let phrase = insert_search_abstraction(&pg, &owner, "it is what it is", None).await?;

    let mut req = lexical_request(&owner, "what it");
    req.kind = Some(EntityKind::Abstraction);
    req.limit = 10;
    let page = pg.search_memories(&req, &[]).await?;

    assert!(
        page.results
            .iter()
            .any(|row| row.memory_id.into_inner() == phrase),
        "an all-stopword query has nothing but the substring band to match with"
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

/// Under recency order the skip argument does not hold, and the code must
/// know that.
///
/// The rule that lets the fallback be skipped is a statement about score
/// ordering: nothing scoring at the band can displace a page of rows above
/// it. Order by `created_at` instead and the newest row wins regardless of
/// score, so the newest row being a substring-only one is exactly the case
/// a score-based skip would drop. The corpus is the same one the first
/// test uses to justify skipping.
#[tokio::test]
async fn recency_order_never_skips_the_substring_band() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;
    let owner = owner_fixture();

    seed_strict_matches(&pg, &owner, "loorgrai", 40).await?;
    // Seeded last, so it is the newest row in the corpus.
    let newest = insert_search_abstraction(&pg, &owner, "prefix xloorgraix suffix", None).await?;

    let mut req = lexical_request(&owner, "loorgrai");
    req.kind = Some(EntityKind::Abstraction);
    req.order = SearchOrder::Recency;
    req.limit = 10;
    let page = pg.search_memories(&req, &[]).await?;

    assert_eq!(
        page.results.first().map(|row| row.memory_id.into_inner()),
        Some(newest),
        "recency order puts the newest match first, and it is a substring-only match"
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

/// Hybrid's substring band rides the semantic statement, so a window row
/// that the tsquery gate excluded still fuses with the 0.25 it earns.
///
/// This is the hole the split would otherwise open. The row is a semantic
/// hit and a substring match and not a tsquery match, so before the split
/// the lexical statement supplied its 0.25 and fusion scored it
/// `0.6 * sim + 0.4 * 0.25`. With the band gone from that statement and
/// the fallback skipped — the page here is full of strict-band rows, so it
/// is skipped — the only thing that can still supply the 0.25 is the
/// semantic leg itself.
#[tokio::test]
async fn hybrid_fuses_the_substring_band_from_the_semantic_leg()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;
    let owner = owner_fixture();

    // A page's worth of strict-band lexical rows, so the fallback is
    // skipped and the semantic leg is the only remaining source of the band.
    for idx in 0..40 {
        insert_embedded_memory(
            &pg,
            &owner,
            &format!("loorgrai record number {idx}"),
            [0.0, 1.0, 0.0],
        )
        .await?;
    }
    let infix =
        insert_embedded_memory(&pg, &owner, "prefix xloorgraix suffix", [1.0, 0.0, 0.0]).await?;

    let req = hybrid_request(&owner, "loorgrai", padded_embedding([1.0, 0.0, 0.0]));
    let page = pg.search_memories(&req, &[]).await?;

    let row = page
        .results
        .iter()
        .find(|row| row.memory_id.into_inner() == infix)
        .expect("the nearest vector is on the page");
    assert!(
        (row.lexical_score - 0.25).abs() <= 1.0e-6,
        "the semantic leg must carry the substring band into fusion: {}",
        row.lexical_score
    );
    assert!(
        (row.score - (0.6 * row.similarity_score + 0.4 * 0.25)).abs() <= 1.0e-5,
        "fused score must include the band: {row:?}"
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

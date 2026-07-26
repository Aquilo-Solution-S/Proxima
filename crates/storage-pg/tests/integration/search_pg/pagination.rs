//! Cursor pagination: disjoint pages, keyset pushdown, and paging past the result cap.

use super::{drop_db, fresh_pg, insert_search_abstraction, lexical_request, owner_fixture};

use proxima_core::storage_ports::*;
use proxima_core::verbs::query::{EntityKind, SearchCursor, SearchOrder};

#[tokio::test]
async fn relevance_pagination_pages_are_disjoint_and_exhaustive()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;
    let owner = owner_fixture();

    for _ in 0..12 {
        insert_search_abstraction(&pg, &owner, "pagegrain needle", None).await?;
    }

    let mut req = lexical_request(&owner, "pagegrain");
    req.kind = Some(EntityKind::Abstraction);
    req.limit = 5;
    let full = {
        let mut one_shot = req.clone();
        one_shot.limit = 12;
        pg.search_memories(&one_shot, &[]).await?.results
    };
    assert_eq!(full.len(), 12);

    // Every score is identical here, so this exercises the pure
    // memory_id tiebreak of the relevance keyset.
    let mut paged = Vec::new();
    let mut after = None;
    let mut hops = 0;
    loop {
        let mut page_req = req.clone();
        page_req.after = after;
        let page = pg.search_memories(&page_req, &[]).await?;
        assert!(page.results.len() <= 5);
        paged.extend(page.results);
        if !page.has_more {
            break;
        }
        let last = paged.last().expect("page emitted rows");
        after = Some(SearchCursor::Relevance {
            score_bits: last.score.to_bits(),
            memory_id: last.memory_id,
            seen: u32::try_from(paged.len())?,
        });
        hops += 1;
        assert!(hops <= 4, "pagination must terminate");
    }
    assert_eq!(paged.len(), 12, "pages must be exhaustive");
    assert_eq!(
        paged
            .iter()
            .map(|row| row.memory_id.into_inner())
            .collect::<Vec<_>>(),
        full.iter()
            .map(|row| row.memory_id.into_inner())
            .collect::<Vec<_>>(),
        "concatenated pages must equal the one-shot listing: no dupes, no gaps"
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn recency_pagination_pushes_keyset_into_sql_and_rejects_mismatched_cursor()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;
    let owner = owner_fixture();

    for _ in 0..8 {
        insert_search_abstraction(&pg, &owner, "recencygrain needle", None).await?;
    }

    let mut req = lexical_request(&owner, "recencygrain");
    req.kind = Some(EntityKind::Abstraction);
    req.order = SearchOrder::Recency;
    req.limit = 3;
    let full = {
        let mut one_shot = req.clone();
        one_shot.limit = 8;
        pg.search_memories(&one_shot, &[]).await?.results
    };
    assert_eq!(full.len(), 8);

    let mut paged = Vec::new();
    let mut after = None;
    loop {
        let mut page_req = req.clone();
        page_req.after = after;
        let page = pg.search_memories(&page_req, &[]).await?;
        paged.extend(page.results);
        if !page.has_more {
            break;
        }
        let last = paged.last().expect("page emitted rows");
        after = Some(SearchCursor::Recency {
            created_at: last.created_at,
            memory_id: last.memory_id,
            seen: u32::try_from(paged.len())?,
        });
        assert!(paged.len() <= 8, "pagination must terminate");
    }
    assert_eq!(
        paged
            .iter()
            .map(|row| row.memory_id.into_inner())
            .collect::<Vec<_>>(),
        full.iter()
            .map(|row| row.memory_id.into_inner())
            .collect::<Vec<_>>(),
        "recency pages must equal the one-shot listing"
    );

    // A cursor of the wrong class for the requested order fails closed.
    let mut mismatched = req;
    mismatched.after = Some(SearchCursor::Relevance {
        score_bits: 1.0_f32.to_bits(),
        memory_id: full[0].memory_id,
        seen: 3,
    });
    let err = pg
        .search_memories(&mismatched, &[])
        .await
        .expect_err("relevance cursor with recency order must fail");
    assert!(
        err.to_string().contains("cursor order"),
        "unexpected error: {err}"
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn search_pages_past_the_fifty_result_cap() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;
    let owner = owner_fixture();

    for _ in 0..55 {
        insert_search_abstraction(&pg, &owner, "capgrain needle", None).await?;
    }

    let mut req = lexical_request(&owner, "capgrain");
    req.kind = Some(EntityKind::Abstraction);
    req.limit = 60;
    let first = pg.search_memories(&req, &[]).await?;
    assert_eq!(first.results.len(), 50, "the per-page cap holds");
    assert!(first.has_more, "matches past the cap must be reported");

    let last = first.results.last().expect("full first page");
    let mut rest_req = req.clone();
    rest_req.after = Some(SearchCursor::Relevance {
        score_bits: last.score.to_bits(),
        memory_id: last.memory_id,
        seen: 50,
    });
    let rest = pg.search_memories(&rest_req, &[]).await?;
    assert_eq!(rest.results.len(), 5, "the cursor reaches past the cap");
    assert!(!rest.has_more);
    let mut all: Vec<_> = first
        .results
        .iter()
        .chain(rest.results.iter())
        .map(|row| row.memory_id.into_inner())
        .collect();
    all.sort_unstable();
    all.dedup();
    assert_eq!(
        all.len(),
        55,
        "the two pages cover every match exactly once"
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

//! Result ordering and scoring: recency order, hybrid fusion weights, and the min-score floor.

use super::{
    TaggedAbstractionInsert, create_tagged_search_sidecars, drop_db, fresh_pg, hybrid_request,
    insert_embedded_memory, insert_search_abstraction, insert_tagged_abstraction, lexical_request,
    owner_fixture, padded_embedding, tagged_abstraction_projection, tagged_search_request,
};

use proxima_core::storage_ports::*;
use proxima_core::verbs::query::{EntityKind, SearchMode, SearchOrder};
use uuid::Uuid;

#[tokio::test]
async fn search_order_recency_sorts_matching_candidates_newest_first()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;
    create_tagged_search_sidecars(pg.pool_for_tests()).await?;

    let owner = owner_fixture();
    let base = time::OffsetDateTime::from_unix_timestamp(1_700_020_000)?;
    let newer = insert_tagged_abstraction(
        &pg,
        &owner,
        TaggedAbstractionInsert {
            memory_id: Uuid::from_u128(31),
            title: "Recency same",
            body: "recency ordering needle",
            tags: &["recency"],
            created_at: base + time::Duration::days(1),
            embedding: None,
        },
    )
    .await?;
    let older = insert_tagged_abstraction(
        &pg,
        &owner,
        TaggedAbstractionInsert {
            memory_id: Uuid::from_u128(32),
            title: "Recency same",
            body: "recency ordering needle",
            tags: &["recency"],
            created_at: base - time::Duration::days(1),
            embedding: None,
        },
    )
    .await?;
    let projection = tagged_abstraction_projection();

    let relevance_req = tagged_search_request(&owner, "recency ordering", SearchMode::Lexical);
    let rows = pg
        .search_memories(&relevance_req, std::slice::from_ref(&projection))
        .await?
        .results;
    assert_eq!(
        rows.iter().map(|row| row.memory_id).collect::<Vec<_>>(),
        vec![older, newer],
        "relevance keeps score then memory_id ordering"
    );

    let mut recency_req = tagged_search_request(&owner, "recency ordering", SearchMode::Lexical);
    recency_req.order = SearchOrder::Recency;
    let rows = pg
        .search_memories(&recency_req, &[projection])
        .await?
        .results;
    assert_eq!(
        rows.iter().map(|row| row.memory_id).collect::<Vec<_>>(),
        vec![newer, older]
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn hybrid_fusion_weight_defaults_and_overrides_rerank()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;
    let owner = owner_fixture();

    // Strong lexical match, orthogonal embedding — and vice versa.
    let lexical_hit =
        insert_embedded_memory(&pg, &owner, "fusiongrain exact needle", [0.0, 1.0, 0.0]).await?;
    let semantic_hit =
        insert_embedded_memory(&pg, &owner, "entirely unrelated prose", [1.0, 0.0, 0.0]).await?;

    let base = hybrid_request(&owner, "fusiongrain", padded_embedding([1.0, 0.0, 0.0]));
    let rows = pg.search_memories(&base, &[]).await?.results;
    assert_eq!(
        rows.first().map(|row| row.memory_id.into_inner()),
        Some(semantic_hit),
        "default 0.6 semantic weight must outrank the lexical-only hit"
    );
    assert!(
        rows.iter()
            .any(|row| row.memory_id.into_inner() == lexical_hit)
    );
    for row in &rows {
        let expected = (0.6 * row.similarity_score) + (0.4 * row.lexical_score);
        assert!(
            (row.score - expected).abs() <= 1.0e-6,
            "default fused score must be 0.6*semantic + 0.4*lexical; got {} want {expected}",
            row.score
        );
    }

    let mut lexical_only = base.clone();
    lexical_only.semantic_weight = Some(0.0);
    let rows = pg.search_memories(&lexical_only, &[]).await?.results;
    assert_eq!(
        rows.first().map(|row| row.memory_id.into_inner()),
        Some(lexical_hit),
        "semantic_weight=0.0 must rank purely lexically"
    );

    let mut semantic_only = base;
    semantic_only.semantic_weight = Some(1.0);
    let rows = pg.search_memories(&semantic_only, &[]).await?.results;
    assert_eq!(
        rows.first().map(|row| row.memory_id.into_inner()),
        Some(semantic_hit),
        "semantic_weight=1.0 must rank purely semantically"
    );
    for row in &rows {
        assert!(
            (row.score - row.similarity_score).abs() <= 1.0e-6,
            "weight 1.0 score must equal the similarity component"
        );
    }

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn min_score_floor_drops_weak_matches() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;
    let owner = owner_fixture();

    let strong = insert_search_abstraction(&pg, &owner, "floorgrain strong hit", None).await?;
    // Substring-only match: no tsquery token, so the LIKE arm scores it 0.25.
    let weak = insert_search_abstraction(&pg, &owner, "prefix xfloorgrainx suffix", None).await?;

    let mut unfloored = lexical_request(&owner, "floorgrain");
    unfloored.kind = Some(EntityKind::Abstraction);
    let page = pg.search_memories(&unfloored, &[]).await?;
    assert!(!page.has_more);
    let ids: Vec<_> = page
        .results
        .iter()
        .map(|row| row.memory_id.into_inner())
        .collect();
    assert!(ids.contains(&strong) && ids.contains(&weak));
    let strong_score = page
        .results
        .iter()
        .find(|row| row.memory_id.into_inner() == strong)
        .expect("strong row")
        .score;
    let weak_score = page
        .results
        .iter()
        .find(|row| row.memory_id.into_inner() == weak)
        .expect("weak row")
        .score;
    assert!(
        strong_score > 0.5,
        "tsquery match scores high: {strong_score}"
    );
    assert!(
        (weak_score - 0.25).abs() <= 1.0e-6,
        "LIKE-only match scores 0.25: {weak_score}"
    );

    let mut floored = unfloored;
    floored.min_score = Some(0.5);
    let page = pg.search_memories(&floored, &[]).await?;
    assert!(
        !page.has_more,
        "sub-floor rows must not count toward has_more"
    );
    assert_eq!(
        page.results
            .iter()
            .map(|row| row.memory_id.into_inner())
            .collect::<Vec<_>>(),
        vec![strong],
        "the floor must drop the 0.25 substring match"
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

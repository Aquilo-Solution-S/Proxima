use super::*;
use proxima_core::verbs::query::SearchCursor;
use proxima_core::{EmbeddingDim, EmbeddingSpace, SpaceVector};
use proxima_storage_pg::test_fixtures::fresh_pg;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn chunk_vector(dim: EmbeddingDim, x: f32, y: f32) -> pgvector::Vector {
    let mut values = vec![0.0; dim.width()];
    values[0] = x;
    values[1] = y;
    pgvector::Vector::from(values)
}

fn chunk_query(dim: EmbeddingDim) -> SpaceVector {
    let mut values = vec![0.0; dim.width()];
    values[0] = 1.0;
    SpaceVector::new(EmbeddingSpace::new("test-embed", dim), values).expect("matching query width")
}

async fn seed_chunks(
    pool: &sqlx::PgPool,
    owner: OwnerRef,
    t: Uuid,
    dim: EmbeddingDim,
    vectors: &[pgvector::Vector],
) -> Result<(), sqlx::Error> {
    let width = i16::try_from(dim.width()).expect("supported dimension fits i16");
    sqlx::query(
        "INSERT INTO proxima_core.embeddings
            (entity_id, model_id, dim, embedding_version, chunk_ordinal, vec, owner_id)
         SELECT $1, 'test-embed', $2, 1, (chunk.ordinality - 1)::integer,
                chunk.vec, $4
           FROM unnest($3::vector[]) WITH ORDINALITY AS chunk(vec, ordinality)",
    )
    .bind(t)
    .bind(width)
    .bind(vectors)
    .bind(owner.stored_owner_id())
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.embedding_heads
            (entity_id, model_id, dim, embedding_version, owner_id)
         VALUES ($1, 'test-embed', $2, 1, $3)",
    )
    .bind(t)
    .bind(width)
    .bind(owner.stored_owner_id())
    .execute(pool)
    .await?;
    Ok(())
}

/// The last chunk alone crosses the score floor. Wide lanes use halfvec
/// candidates but must report the original vector's cosine, not its rounding.
#[tokio::test]
async fn last_chunk_matches_semantic_and_hybrid_with_full_precision_score() -> TestResult {
    let (pg, _guard) = fresh_pg("last_embedding_chunk").await;
    let pool = pg.pool_for_tests();
    let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    let target = seed_note(pool, owner, "Long document", "Unrelated vocabulary").await?;
    let other = seed_note(
        pool,
        owner,
        "Other document",
        "Another unrelated observation",
    )
    .await?;
    let zero = seed_note(pool, owner, "Zero vector", "No defined cosine").await?;
    let projection = note_projection();
    for dim in [
        EmbeddingDim::D1024,
        EmbeddingDim::D2048,
        EmbeddingDim::D3072,
    ] {
        seed_chunks(
            pool,
            owner,
            target,
            dim,
            &[
                chunk_vector(dim, 0.0, 1.0),
                chunk_vector(dim, 0.6, 0.8),
                chunk_vector(dim, 0.8, 0.6),
            ],
        )
        .await?;
        seed_chunks(
            pool,
            owner,
            other,
            dim,
            &[chunk_vector(dim, 0.6, 0.8), chunk_vector(dim, 0.0, 0.0)],
        )
        .await?;
        seed_chunks(pool, owner, zero, dim, &[chunk_vector(dim, 0.0, 0.0)]).await?;
        for mode in [SearchMode::Semantic, SearchMode::Hybrid] {
            let mut req = search_req(owner, "absent-query-phrase");
            req.mode = mode;
            req.semantic = Some(chunk_query(dim));
            req.semantic_weight = Some(0.75);
            req.min_score = Some(0.75);
            let page = pg
                .search_memories(None, &req, std::slice::from_ref(&projection))
                .await?;
            assert_eq!(page.results.len(), 1, "{dim:?} {mode:?}: {page:?}");
            let hit = &page.results[0];
            assert_eq!(hit.memory_id.into_inner(), target);
            assert!(
                (hit.similarity_score - 0.8).abs() < 0.000_001,
                "{dim:?}: {hit:?}"
            );
            let expected_score = if mode == SearchMode::Semantic {
                0.8
            } else {
                0.75 / 61.0
            };
            assert!((hit.score - expected_score).abs() < 0.000_001, "{hit:?}");
            assert!(!page.has_more);
        }
    }
    Ok(())
}

/// 130 top chunks from one memory exceed the initial 40-row window and
/// its first expansion. Distinct results and a second page must survive.
#[tokio::test]
async fn chunk_clusters_do_not_spend_distinct_memory_limit_or_cursor() -> TestResult {
    let (pg, _guard) = fresh_pg("clustered_embedding_chunks").await;
    let pool = pg.pool_for_tests();
    let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    let dim = EmbeddingDim::D1024;
    let first = seed_note(pool, owner, "Dominant", "Many embedding fragments").await?;
    seed_chunks(
        pool,
        owner,
        first,
        dim,
        &vec![chunk_vector(dim, 1.0, 0.0); 130],
    )
    .await?;
    let mut expected = vec![first];
    for (title, x, y) in [
        ("Second", 0.8, 0.6),
        ("Third", 0.6, 0.8),
        ("Fourth", 0.0, 1.0),
    ] {
        let t = seed_note(pool, owner, title, "Independent memory").await?;
        seed_chunks(pool, owner, t, dim, &[chunk_vector(dim, x, y)]).await?;
        expected.push(t);
    }
    let projection = note_projection();
    for mode in [SearchMode::Semantic, SearchMode::Hybrid] {
        let mut req = search_req(owner, "absent-query-phrase");
        req.mode = mode;
        req.limit = 2;
        req.semantic = Some(chunk_query(dim));
        let first_page = pg
            .search_memories(None, &req, std::slice::from_ref(&projection))
            .await?;
        assert_eq!(
            first_page
                .results
                .iter()
                .map(|hit| hit.memory_id.into_inner())
                .collect::<Vec<_>>(),
            expected[..2]
        );
        assert!(first_page.has_more);
        let last = first_page.results.last().expect("full first page");
        req.after = Some(SearchCursor::Relevance {
            score_bits: last.score.to_bits(),
            memory_id: last.memory_id,
            seen: 2,
        });
        let second_page = pg
            .search_memories(None, &req, std::slice::from_ref(&projection))
            .await?;
        assert_eq!(
            second_page
                .results
                .iter()
                .map(|hit| hit.memory_id.into_inner())
                .collect::<Vec<_>>(),
            expected[2..]
        );
        assert!(!second_page.has_more);
    }
    Ok(())
}

#[tokio::test]
async fn chunk_scan_stops_at_existing_cap_when_one_memory_dominates() -> TestResult {
    let (pg, _guard) = fresh_pg("embedding_chunk_cap").await;
    let pool = pg.pool_for_tests();
    let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    let dim = EmbeddingDim::D1024;
    let first = seed_note(pool, owner, "Dominant", "One thousand matching fragments").await?;
    seed_chunks(
        pool,
        owner,
        first,
        dim,
        &vec![chunk_vector(dim, 1.0, 0.0); 1_000],
    )
    .await?;
    let beyond = seed_note(pool, owner, "Beyond cap", "More distant memory").await?;
    seed_chunks(pool, owner, beyond, dim, &[chunk_vector(dim, 0.8, 0.6)]).await?;
    let mut req = search_req(owner, "absent-query-phrase");
    req.mode = SearchMode::Semantic;
    req.limit = 2;
    req.semantic = Some(chunk_query(dim));
    let page = pg.search_memories(None, &req, &[note_projection()]).await?;
    assert_eq!(page.results.len(), 1);
    assert_eq!(page.results[0].memory_id.into_inner(), first);
    assert!(!page.has_more);
    Ok(())
}

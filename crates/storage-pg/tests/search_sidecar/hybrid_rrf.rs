use super::*;
use proxima_core::verbs::query::{MemorySearchPage, SearchCursor};
use proxima_storage_pg::test_fixtures::fresh_pg;
use std::collections::{BTreeMap, BTreeSet};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn assert_score(actual: f32, expected: f32) {
    assert_eq!(
        actual.to_bits(),
        expected.to_bits(),
        "expected {expected}, got {actual}"
    );
}

fn hybrid_request(owner: OwnerRef, query: &str) -> MemorySearchRequest {
    let mut req = search_req(owner, query);
    req.mode = SearchMode::Hybrid;
    let mut values = vec![0.0; 1024];
    values[0] = 1.0;
    req.semantic = Some(test_semantic(values));
    req
}

fn hits_by_id(
    page: &MemorySearchPage,
) -> BTreeMap<Uuid, &proxima_core::verbs::query::MemorySearchResult> {
    page.results
        .iter()
        .map(|hit| (hit.memory_id.into_inner(), hit))
        .collect()
}

#[tokio::test]
async fn hybrid_uses_rescue_and_ranks_real_single_leg_membership() -> TestResult {
    let (pg, _guard) = fresh_pg("hybrid_rrf_legs").await;
    let pool = pg.pool_for_tests();
    let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    let rescue = seed_note(pool, owner, "Partial result", "quartz").await?;
    let semantic = seed_note(pool, owner, "Concept result", "Unrelated vocabulary").await?;
    let both = seed_note(pool, owner, "Exact result", "quartz nebula").await?;
    let zero = seed_note(pool, owner, "Orthogonal result", "Separate vocabulary").await?;
    seed_embedding(pool, owner, semantic, &embed_literal()).await?;
    seed_embedding(pool, owner, both, &embed_literal_xy("0.8", "0.6")).await?;
    seed_embedding(pool, owner, zero, &embed_literal_xy("0", "1")).await?;
    let other_owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    let foreign = seed_note(pool, other_owner, "quartz nebula", "quartz nebula").await?;
    seed_embedding(pool, other_owner, foreign, &embed_literal()).await?;
    let projection = note_projection();
    let mut req = hybrid_request(owner, "quartz nebula");
    let hybrid = pg
        .search_memories(None, &req, std::slice::from_ref(&projection))
        .await?;
    let hits = hits_by_id(&hybrid);
    assert_eq!(hits.len(), 4);
    assert!(
        !hits.contains_key(&foreign),
        "foreign hits cannot spend RRF ranks"
    );
    assert!(hits[&rescue].lexical_score > 0.25 && hits[&rescue].lexical_score <= 0.45);
    assert_score(hits[&rescue].similarity_score, 0.0);
    assert_score(hits[&rescue].score, 0.5 / 62.0);
    assert_score(hits[&semantic].lexical_score, 0.0);
    assert_score(hits[&semantic].score, 0.5 / 61.0);
    assert_score(hits[&both].score, 0.5 / 62.0 + 0.5 / 61.0);
    assert_eq!(hybrid.results[0].memory_id.into_inner(), both);
    assert_score(hits[&zero].similarity_score, 0.0);
    assert_score(hits[&zero].score, 0.5 / 63.0);
    assert!(!hybrid.has_more);
    for mode in [SearchMode::Lexical, SearchMode::Semantic] {
        req.mode = mode;
        let pure = pg
            .search_memories(None, &req, std::slice::from_ref(&projection))
            .await?;
        for hit in pure.results {
            let fused = hits[&hit.memory_id.into_inner()];
            let raw = if mode == SearchMode::Lexical {
                hit.lexical_score
            } else {
                hit.similarity_score
            };
            assert_score(hit.score, raw);
            assert_score(
                hit.lexical_score,
                if mode == SearchMode::Lexical {
                    fused.lexical_score
                } else {
                    0.0
                },
            );
            assert_score(
                hit.similarity_score,
                if mode == SearchMode::Semantic {
                    fused.similarity_score
                } else {
                    0.0
                },
            );
        }
    }
    Ok(())
}

#[tokio::test]
async fn hybrid_min_score_keeps_either_raw_leg_and_drops_both_below_floor() -> TestResult {
    let (pg, _guard) = fresh_pg("hybrid_rrf_floor").await;
    let pool = pg.pool_for_tests();
    let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    let lexical = seed_note(pool, owner, "Lexical winner", "quartz nebula").await?;
    let semantic = seed_note(pool, owner, "Semantic winner", "Unrelated vocabulary").await?;
    let both_low = seed_note(pool, owner, "Weak winner", "quartz").await?;
    let lexical_high = seed_note(pool, owner, "Strong phrase", "quartz nebula").await?;
    seed_embedding(pool, owner, semantic, &embed_literal_xy("0.8", "0.6")).await?;
    seed_embedding(pool, owner, both_low, &embed_literal_xy("0.4", "0.9165151")).await?;
    seed_embedding(
        pool,
        owner,
        lexical_high,
        &embed_literal_xy("0.4", "0.9165151"),
    )
    .await?;
    let mut req = hybrid_request(owner, "quartz nebula");
    req.min_score = Some(0.5);
    let page = pg.search_memories(None, &req, &[note_projection()]).await?;
    let hits = hits_by_id(&page);
    assert_eq!(hits.len(), 3);
    assert!(!hits.contains_key(&both_low));
    assert!(hits[&lexical].lexical_score >= 0.5);
    assert!(hits[&semantic].similarity_score >= 0.5);
    assert_score(hits[&semantic].score, 0.5 / 61.0);
    assert!(hits[&lexical_high].similarity_score < 0.5);
    assert!(
        hits[&lexical_high].similarity_score > 0.0,
        "keep raw score even when its leg fails the floor"
    );
    let mut lexical_hits = [hits[&lexical], hits[&lexical_high]];
    lexical_hits.sort_by(|a, b| {
        b.lexical_score
            .total_cmp(&a.lexical_score)
            .then_with(|| b.memory_id.cmp(&a.memory_id))
    });
    assert_score(lexical_hits[0].score, 0.5 / 61.0);
    assert_score(lexical_hits[1].score, 0.5 / 62.0);
    assert!(
        page.results.iter().all(|hit| hit.score < 0.5),
        "the floor does not apply to RRF values"
    );
    Ok(())
}

async fn seed_window_notes(pool: &sqlx::PgPool, owner: OwnerRef) -> Result<Vec<Uuid>, sqlx::Error> {
    let ids: Vec<_> = (0..1_001).map(|_| Uuid::now_v7()).collect();
    sqlx::query("INSERT INTO proxima_core.owners (owner_id, kind) VALUES ($1, 'personal')")
        .bind(owner.stored_owner_id())
        .execute(pool)
        .await?;
    let mut stamped = pool.begin().await?;
    sqlx::query(
        "INSERT INTO proxima_core.memory_head (handle, t, kind, schema_id, owner_id)
         SELECT t, t, 'fact', 'core/agent-note-v1', $2 FROM unnest($1::uuid[]) AS t",
    )
    .bind(&ids)
    .bind(owner.stored_owner_id())
    .execute(&mut *stamped)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.memory (handle, t, kind, schema_id, owner_id, sidecar_tables)
         SELECT t, t, 'fact', 'core/agent-note-v1', $2,
                ARRAY['proxima_core.agent_note_v1'] FROM unnest($1::uuid[]) AS t",
    )
    .bind(&ids)
    .bind(owner.stored_owner_id())
    .execute(&mut *stamped)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.agent_note_v1 (t, note_id, title, body, tags)
         SELECT t, t, 'quartz', 'Window membership', '{}'::text[] FROM unnest($1::uuid[]) AS t",
    )
    .bind(&ids)
    .execute(&mut *stamped)
    .await?;
    stamped.commit().await?;
    for &t in &ids {
        project(pool, t, "core/agent-note-v1", None).await?;
    }
    Ok(ids)
}

/// More than one leg's fixed window. Both orders must exhaust exactly the
/// same eligible union, retaining ranks when page sizes and cursors change.
#[tokio::test]
async fn hybrid_cursor_pages_end_at_fixed_window_in_both_orders() -> TestResult {
    let (pg, _guard) = fresh_pg("hybrid_rrf_window").await;
    let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    let mut ids = seed_window_notes(pg.pool_for_tests(), owner).await?;
    ids.sort_unstable_by(|a, b| b.cmp(a));
    let expected: BTreeSet<_> = ids[..1_000].iter().copied().collect();
    let projection = note_projection();
    let mut req = hybrid_request(owner, "quartz");
    for order in [SearchOrder::Relevance, SearchOrder::Recency] {
        req.order = order;
        req.after = None;
        req.limit = 1;
        let mut seen = BTreeSet::new();
        loop {
            let page = pg
                .search_memories(None, &req, std::slice::from_ref(&projection))
                .await?;
            assert!(!page.results.is_empty());
            for hit in &page.results {
                let t = hit.memory_id.into_inner();
                assert!(seen.insert(t), "cursor pages must be disjoint");
                let rank = ids.iter().position(|id| *id == t).expect("seeded result") + 1;
                let rank = u16::try_from(rank).expect("fixed window rank");
                assert_score(hit.score, 0.5 / (60.0 + f32::from(rank)));
            }
            let last = page.results.last().expect("nonempty page");
            let count = u32::try_from(seen.len()).expect("fixed window count");
            req.after = Some(match order {
                SearchOrder::Relevance => SearchCursor::Relevance {
                    score_bits: last.score.to_bits(),
                    memory_id: last.memory_id,
                    seen: count,
                },
                SearchOrder::Recency => SearchCursor::Recency {
                    created_at: last.created_at,
                    memory_id: last.memory_id,
                    seen: count,
                },
            });
            if !page.has_more {
                break;
            }
            req.limit = 50;
        }
        assert_eq!(
            seen, expected,
            "the fixed window is complete and excludes its 1,001st hit"
        );
        let exhausted = pg
            .search_memories(None, &req, std::slice::from_ref(&projection))
            .await?;
        assert!(exhausted.results.is_empty());
        assert!(!exhausted.has_more);
    }
    Ok(())
}

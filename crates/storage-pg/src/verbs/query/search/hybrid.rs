//! Hybrid's fixed candidate windows and weighted reciprocal rank fusion.

use std::collections::BTreeMap;

use proxima_core::verbs::query::{
    DEFAULT_HYBRID_SEMANTIC_WEIGHT, MemorySearchRequest, MemorySearchResult, SearchMode,
};

use super::Hit;

/// Each leg contributes at most 1,000 eligible distinct memories. Flavor
/// declarations may impose a smaller lexical cap; the semantic chunk scan
/// retains its shared 1,000-row cap. The windows never depend on page size,
/// output order or cursor depth. Paging ends at their fused union.
pub(super) const HYBRID_CANDIDATE_WINDOW: u32 = 1_000;
const RRF_K: f32 = 60.0;

pub(super) fn uses_hybrid_fusion(req: &MemorySearchRequest) -> bool {
    req.mode == SearchMode::Hybrid && req.semantic.is_some()
}

/// Assign one-based ranks after admission and the raw per-leg floor. A hit
/// outside a leg's window, or absent from that leg, receives no term from it.
/// Keep raw scores in the output even when only its other leg passes the floor.
pub(super) fn apply_hybrid_fusion(
    req: &MemorySearchRequest,
    results: &mut Vec<MemorySearchResult>,
    hits: &BTreeMap<uuid::Uuid, Hit>,
) {
    let weight = req
        .semantic_weight
        .unwrap_or(DEFAULT_HYBRID_SEMANTIC_WEIGHT)
        .clamp(0.0, 1.0);
    let mut fused = BTreeMap::<uuid::Uuid, f32>::new();
    for (semantic, leg_weight) in [(true, weight), (false, 1.0 - weight)] {
        let mut leg: Vec<(uuid::Uuid, f32)> = results
            .iter()
            .filter_map(|result| {
                let t = result.memory_id.into_inner();
                let hit = hits.get(&t)?;
                let score = if semantic {
                    hit.similarity_score?
                } else {
                    hit.lexical_score?
                };
                req.min_score
                    .is_none_or(|floor| score >= floor)
                    .then_some((t, score))
            })
            .collect();
        leg.sort_by(|(a_t, a_score), (b_t, b_score)| {
            b_score.total_cmp(a_score).then_with(|| b_t.cmp(a_t))
        });
        leg.truncate(usize::try_from(HYBRID_CANDIDATE_WINDOW).unwrap_or(usize::MAX));
        for (index, (t, _)) in leg.into_iter().enumerate() {
            let rank = u16::try_from(index + 1).expect("fixed hybrid window fits u16");
            *fused.entry(t).or_default() += leg_weight / (RRF_K + f32::from(rank));
        }
    }
    results.retain_mut(|result| {
        if let Some(score) = fused.get(&result.memory_id.into_inner()) {
            result.score = *score;
            true
        } else {
            false
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verbs::query::search::tests::request_with_tags;
    use proxima_core::verbs::query::{EntityKind, SearchCursor, SearchOrder};
    use proxima_core::{MemoryId, SchemaId};

    fn assert_score(actual: f32, expected: f32) {
        assert_eq!(
            actual.to_bits(),
            expected.to_bits(),
            "expected {expected}, got {actual}"
        );
    }

    fn result(t: u128, lexical: Option<f32>, semantic: Option<f32>) -> (MemorySearchResult, Hit) {
        let t = uuid::Uuid::from_u128(t);
        (
            MemorySearchResult {
                memory_id: MemoryId::new(t),
                kind: EntityKind::Fact,
                schema_id: SchemaId::new("core/agent-note-v1".into()),
                created_at: time::OffsetDateTime::UNIX_EPOCH,
                snippet: String::new(),
                score: 0.0,
                lexical_score: lexical.unwrap_or_default(),
                similarity_score: semantic.unwrap_or_default(),
            },
            Hit {
                t,
                lexical_score: lexical,
                similarity_score: semantic,
            },
        )
    }

    fn fuse(
        examples: &[(u128, Option<f32>, Option<f32>)],
        weight: Option<f32>,
        floor: Option<f32>,
    ) -> Vec<MemorySearchResult> {
        let mut req = request_with_tags();
        req.semantic_weight = weight;
        req.min_score = floor;
        let (mut results, hits): (Vec<_>, Vec<_>) = examples
            .iter()
            .map(|&(t, lexical, semantic)| result(t, lexical, semantic))
            .unzip();
        let hits = hits.into_iter().map(|hit| (hit.t, hit)).collect();
        apply_hybrid_fusion(&req, &mut results, &hits);
        results
    }

    #[test]
    fn reciprocal_ranks_ignore_raw_scale_and_include_single_leg_hits() {
        let results = fuse(
            &[
                (1, Some(0.01), Some(0.9)),
                (2, Some(0.02), None),
                (3, None, Some(0.8)),
            ],
            None,
            None,
        );
        assert_score(results[0].score, 0.5 / 61.0 + 0.5 / 62.0);
        assert_score(results[1].score, 0.5 / 61.0);
        assert_score(results[2].score, 0.5 / 62.0);
        assert_score(results[0].lexical_score, 0.01);
        assert_score(results[0].similarity_score, 0.9);
    }

    #[test]
    fn floor_applies_to_each_leg_before_ranking() {
        let results = fuse(
            &[
                (1, Some(0.4), Some(0.4)),
                (2, Some(0.7), Some(0.4)),
                (3, Some(0.4), Some(0.8)),
            ],
            Some(0.75),
            Some(0.5),
        );
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].memory_id.into_inner(), uuid::Uuid::from_u128(2));
        assert_score(results[0].score, 0.25 / 61.0);
        assert_score(results[0].similarity_score, 0.4);
        assert_score(results[1].score, 0.75 / 61.0);
        assert_score(results[1].lexical_score, 0.4);
    }

    #[test]
    fn zero_score_membership_and_zero_weight_are_preserved() {
        let results = fuse(
            &[(1, Some(0.0), None), (2, None, Some(0.0))],
            None,
            Some(0.0),
        );
        assert_eq!(results.len(), 2);
        assert_score(results[0].score, 0.5 / 61.0);
        assert_score(results[1].score, 0.5 / 61.0);
        let results = fuse(
            &[(1, Some(0.1), None), (2, None, Some(0.2))],
            Some(1.0),
            None,
        );
        assert_eq!(results.len(), 2, "membership survives a zero fusion weight");
        assert_score(results[0].score, 0.0);
        assert_score(results[1].score, 1.0 / 61.0);
    }

    #[test]
    fn global_windows_are_bounded_per_leg_before_union() {
        let (mut results, hits): (Vec<_>, Vec<_>) = (1..=1_001)
            .map(|t| result(t, Some(0.5), None))
            .chain((2_001..=3_001).map(|t| result(t, None, Some(0.5))))
            .unzip();
        let hits = hits.into_iter().map(|hit| (hit.t, hit)).collect();
        apply_hybrid_fusion(&request_with_tags(), &mut results, &hits);
        assert_eq!(results.len(), 2_000);
        for excluded in [1, 2_001] {
            assert!(
                !results
                    .iter()
                    .any(|row| row.memory_id.into_inner() == uuid::Uuid::from_u128(excluded))
            );
        }
    }

    #[test]
    fn only_admitted_results_spend_ranks() {
        let (mut results, hits): (Vec<_>, Vec<_>) = [(1, 0.1), (2, 0.9)]
            .into_iter()
            .map(|(t, score)| result(t, Some(score), None))
            .unzip();
        let hits = hits.into_iter().map(|hit| (hit.t, hit)).collect();
        results.retain(|row| row.memory_id.into_inner() == uuid::Uuid::from_u128(1));
        apply_hybrid_fusion(&request_with_tags(), &mut results, &hits);
        assert_eq!(results.len(), 1);
        assert_score(results[0].score, 0.5 / 61.0);
    }

    #[test]
    fn fixed_window_ranks_and_pages_ignore_depth_and_page_size() {
        let (results, hits): (Vec<_>, Vec<_>) =
            (1..=1_001).map(|t| result(t, Some(0.5), None)).unzip();
        let hits = hits.into_iter().map(|hit| (hit.t, hit)).collect();
        let mut req = request_with_tags();
        req.mode = SearchMode::Hybrid;
        let mut vector = vec![0.0; proxima_core::EmbeddingDim::D1024.width()];
        vector[0] = 1.0;
        req.semantic = Some(
            proxima_core::SpaceVector::new(
                proxima_core::EmbeddingSpace::new("test", proxima_core::EmbeddingDim::D1024),
                vector,
            )
            .expect("test vector"),
        );
        for order in [SearchOrder::Relevance, SearchOrder::Recency] {
            req.order = order;
            req.after = None;
            let mut seen = Vec::new();
            loop {
                let limit = if seen.len().is_multiple_of(2) { 7 } else { 13 };
                assert_eq!(super::super::candidate_overfetch(&req, limit, 5_000), 1_000);
                assert_eq!(super::super::candidate_order(&req), SearchOrder::Relevance);
                let mut fused = results.clone();
                apply_hybrid_fusion(&req, &mut fused, &hits);
                let page = super::super::page_hits(&req, limit, fused);
                seen.extend(page.results.iter().map(|row| row.memory_id));
                if !page.has_more {
                    break;
                }
                let last = page.results.last().expect("nonempty page");
                let count = u32::try_from(seen.len()).expect("bounded window");
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
            }
            assert_eq!(seen.len(), 1_000);
            assert_eq!(
                seen.iter().collect::<std::collections::BTreeSet<_>>().len(),
                1_000
            );
            assert!(!seen.contains(&MemoryId::new(uuid::Uuid::from_u128(1))));
        }
    }
}

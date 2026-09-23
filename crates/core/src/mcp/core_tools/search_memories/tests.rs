use super::{
    DEFAULT_BODY_MAX_CHARS, LaneArm, ListPosition, MergeKind, NeighborEdge, OwnerLane, PageCursor,
    RankedList, RankedMemoryOutput, SEMANTIC_SEARCH_UNAVAILABLE, SearchMemoriesArgs,
    SearchMemoriesKind, SearchMemoriesMode, SearchMemoriesSupersession, SearchMemoryOutput,
    SearchRanking, cursor_positions, decode_cursor, degraded_to_lexical, effective_body_max_chars,
    encode_cursor, paginate_rank_fused, ranking_for, retain_surviving_neighbor_edges,
    truncate_body, validate_body_max_chars, validate_list_caps, validate_score_args,
    weight_for_effective_mode,
};
use crate::MemoryId;
use crate::mcp::McpToolError;
use crate::verbs::query::{SearchCursor, SearchMode, SearchOrder, TagMatch};

fn memory_output(handle: &str) -> SearchMemoryOutput {
    SearchMemoryOutput {
        memory_id: uuid::Uuid::nil(),
        memory: handle.to_string(),
        space: "current".into(),
        kind: "Fact".into(),
        schema_id: "core/agent-note".into(),
        created_at: "2026-07-05T00:00:00Z".into(),
        snippet: String::new(),
        score: 1.0,
        lexical_score: 1.0,
        similarity_score: 0.0,
        tags: Vec::new(),
        body: None,
        body_truncated: None,
    }
}

fn neighbor_edge(source: &str, target: &str) -> NeighborEdge {
    NeighborEdge {
        source: source.to_string(),
        target: target.to_string(),
        kind: "origin".into(),
    }
}

#[test]
fn search_cursor_schema_allows_presentation_to_vary() {
    let schema = serde_json::to_string(&schemars::schema_for!(SearchMemoriesArgs))
        .expect("search args schema");
    assert!(
        schema.contains("include_body") && schema.contains("include_neighbor_edges"),
        "cursor contract must name the presentation flags that may vary"
    );
    assert!(
        !schema.contains("every argument except limit"),
        "schema must not contradict the fingerprint (limit/body/neighbors may vary)"
    );
}

#[test]
fn omitted_include_neighbor_edges_deserializes_false() {
    let args: SearchMemoriesArgs = serde_json::from_value(serde_json::json!({ "query": "needle" }))
        .expect("minimal search args");
    assert!(
        !args.include_neighbor_edges,
        "neighbors default off; omitted field must be false"
    );
}

fn args(mode: SearchMemoriesMode) -> SearchMemoriesArgs {
    SearchMemoriesArgs {
        query: "needle".into(),
        mode,
        limit: 8,
        supersession: SearchMemoriesSupersession::HeadsOnly,
        kind: None,
        schema_id: None,
        tags: Vec::new(),
        tag_match: TagMatch::Any,
        since: None,
        until: None,
        order: SearchOrder::Relevance,
        min_score: None,
        semantic_weight: None,
        include_neighbor_edges: false,
        include_body: false,
        body_max_chars: None,
        spaces: Vec::new(),
        cursor: None,
    }
}

#[test]
fn score_args_reject_out_of_range_and_non_hybrid_weight() {
    let mut floor_too_high = args(SearchMemoriesMode::Hybrid);
    floor_too_high.min_score = Some(1.01);
    assert!(matches!(
        validate_score_args(&floor_too_high),
        Err(McpToolError::InvalidInput(message)) if message.contains("min_score")
    ));

    let mut nan_floor = args(SearchMemoriesMode::Hybrid);
    nan_floor.min_score = Some(f32::NAN);
    assert!(validate_score_args(&nan_floor).is_err());

    let mut weight_out_of_range = args(SearchMemoriesMode::Hybrid);
    weight_out_of_range.semantic_weight = Some(-0.1);
    assert!(matches!(
        validate_score_args(&weight_out_of_range),
        Err(McpToolError::InvalidInput(message)) if message.contains("semantic_weight")
    ));

    // An explicit weight outside hybrid mode is a contradiction, not a
    // silently ignored knob.
    let mut lexical_weight = args(SearchMemoriesMode::Lexical);
    lexical_weight.semantic_weight = Some(0.5);
    assert!(matches!(
        validate_score_args(&lexical_weight),
        Err(McpToolError::InvalidInput(message)) if message.contains("mode=hybrid")
    ));

    let mut valid = args(SearchMemoriesMode::Hybrid);
    valid.min_score = Some(0.25);
    valid.semantic_weight = Some(1.0);
    assert!(validate_score_args(&valid).is_ok());
}

#[test]
fn cursor_round_trips_and_rejects_foreign_or_garbled_tokens() {
    let cursor = SearchCursor::Relevance {
        score_bits: 0.75_f32.to_bits(),
        memory_id: MemoryId::new(uuid::Uuid::now_v7()),
        seen: 42,
    };
    let token = encode_cursor(&PageCursor::Keyset(cursor), "fp-aaaa");
    assert_eq!(
        decode_cursor(&token, "fp-aaaa").unwrap(),
        PageCursor::Keyset(cursor)
    );

    // Same token replayed against a different query shape.
    match decode_cursor(&token, "fp-bbbb") {
        Err(McpToolError::InvalidInput(message)) => {
            assert!(message.contains("does not match this query"), "{message}");
        }
        other => panic!("expected fingerprint mismatch, got {other:?}"),
    }

    // Not base64 / not our envelope.
    for garbage in ["%%%", "bm90LWpzb24"] {
        match decode_cursor(garbage, "fp-aaaa") {
            Err(McpToolError::InvalidInput(message)) => {
                assert!(message.contains("malformed cursor"), "{message}");
            }
            other => panic!("expected malformed cursor error, got {other:?}"),
        }
    }
}

#[test]
fn semantic_unavailable_message_is_provider_neutral() {
    assert!(
        !SEMANTIC_SEARCH_UNAVAILABLE.contains("_API_KEY"),
        "the actionable message must not hardcode a provider env var: {SEMANTIC_SEARCH_UNAVAILABLE}",
    );
    assert!(SEMANTIC_SEARCH_UNAVAILABLE.contains("no embedding client is configured"));
}

#[test]
fn search_mode_and_supersession_accept_mixed_case() {
    assert!(matches!(
        serde_json::from_value::<SearchMemoriesMode>(serde_json::json!("Hybrid")).unwrap(),
        SearchMemoriesMode::Hybrid
    ));
    assert!(matches!(
        serde_json::from_value::<SearchMemoriesMode>(serde_json::json!("SEMANTIC")).unwrap(),
        SearchMemoriesMode::Semantic
    ));
    assert!(matches!(
        serde_json::from_value::<SearchMemoriesMode>(serde_json::json!("lexical")).unwrap(),
        SearchMemoriesMode::Lexical
    ));
    assert!(matches!(
        serde_json::from_value::<SearchMemoriesSupersession>(serde_json::json!("HeadsOnly"))
            .unwrap(),
        SearchMemoriesSupersession::HeadsOnly
    ));
    assert!(matches!(
        serde_json::from_value::<SearchMemoriesSupersession>(serde_json::json!("all")).unwrap(),
        SearchMemoriesSupersession::All
    ));
}

#[test]
fn neighbor_edges_to_truncated_hits_are_dropped_and_deduped() {
    let memories = [memory_output("F:1"), memory_output("A:2")];
    let mut edges = vec![
        // Touches a surviving hit via source.
        neighbor_edge("A:2", "F:99"),
        // Both endpoints truncated out — dropped.
        neighbor_edge("F:98", "F:97"),
        // Same content as the first — deduped, because content IS
        // the edge's identity.
        neighbor_edge("A:2", "F:99"),
        // Touches a surviving hit via target.
        neighbor_edge("F:96", "F:1"),
    ];
    retain_surviving_neighbor_edges(&memories, &mut edges);
    let kept: Vec<_> = edges
        .iter()
        .map(|edge| (edge.source.as_str(), edge.target.as_str()))
        .collect();
    assert_eq!(kept, [("A:2", "F:99"), ("F:96", "F:1")]);
}

#[test]
fn degraded_flag_only_fires_for_hybrid_with_results_and_no_semantic() {
    // Hybrid returned rows but none carried a semantic score → degraded.
    assert!(degraded_to_lexical(SearchMode::Hybrid, false, false));
    // Hybrid with a real semantic score → healthy.
    assert!(!degraded_to_lexical(SearchMode::Hybrid, false, true));
    // Hybrid with no results at all → a genuine no-match, not degradation.
    assert!(!degraded_to_lexical(SearchMode::Hybrid, true, false));
    // Pure Semantic never reports lexical degradation (no lexical branch runs).
    assert!(!degraded_to_lexical(SearchMode::Semantic, false, false));
    // Lexical is never degraded.
    assert!(!degraded_to_lexical(SearchMode::Lexical, false, false));
}

/// The verb rejects a fusion weight paired with a mode that would
/// discard it. A hybrid request that degrades to lexical because the
/// deployment has no embeddings must stay servable, so the weight is
/// dropped with the semantic component it was weighting — otherwise
/// every `semantic_weight` search on such a deployment would start
/// failing on a rule the caller did not break.
#[test]
fn a_degraded_run_drops_the_weight_it_can_no_longer_honor() {
    assert_eq!(
        weight_for_effective_mode(Some(0.7), SearchMode::Hybrid),
        Some(0.7),
        "a hybrid run still fuses, so it keeps the caller's weight"
    );
    assert_eq!(
        weight_for_effective_mode(Some(0.7), SearchMode::Lexical),
        None,
        "degrading to lexical leaves no semantic component to weight"
    );
    assert_eq!(weight_for_effective_mode(None, SearchMode::Hybrid), None);
}

fn lane(owner: u128, space: Option<&str>) -> OwnerLane {
    let owner = crate::OwnerRef::Personal(crate::UserId::new(uuid::Uuid::from_u128(owner)));
    OwnerLane {
        space: crate::mcp::core_tools::memory_spaces::ResolvedMemorySpace {
            key: owner.external_key(),
            label: owner.external_key(),
            owner,
        },
        arm: space.map_or(LaneArm::Lexical, |model| {
            LaneArm::Semantic(
                crate::SpaceVector::new(
                    crate::EmbeddingSpace::new(model, crate::EmbeddingDim::D1024),
                    vec![0.0; 1024],
                )
                .expect("1024 wide"),
            )
        }),
    }
}

fn ranked(memory_id: u128, score: f32) -> RankedMemoryOutput {
    let mut output = memory_output(&format!("F:{memory_id}"));
    output.memory_id = uuid::Uuid::from_u128(memory_id);
    output.score = score;
    RankedMemoryOutput {
        memory_id: uuid::Uuid::from_u128(memory_id),
        created_at: time::OffsetDateTime::UNIX_EPOCH,
        output,
    }
}

#[test]
fn one_scoring_lane_ranks_by_score_and_any_mix_ranks_by_rank() {
    assert_eq!(
        ranking_for(&[lane(1, Some("m")), lane(2, Some("m"))]),
        SearchRanking::Score
    );
    assert_eq!(
        ranking_for(&[lane(1, None), lane(2, None)]),
        SearchRanking::Score
    );
    assert_eq!(
        ranking_for(&[lane(1, Some("m")), lane(2, Some("other"))]),
        SearchRanking::Rank
    );
    // A degraded Owner's lexical score is not on its peer's scale.
    assert_eq!(
        ranking_for(&[lane(1, Some("m")), lane(2, None)]),
        SearchRanking::Rank
    );
    assert_eq!(
        MergeKind::of(SearchRanking::Rank, SearchOrder::Relevance),
        MergeKind::Rank
    );
    assert_eq!(
        MergeKind::of(SearchRanking::Rank, SearchOrder::Recency),
        MergeKind::Keyset,
        "recency compares across any scoring lanes"
    );
}

/// Two Owners on different models: scores are incomparable, so the page
/// interleaves by rank, and paging through per-list keysets visits every
/// row exactly once, in one stable order.
#[test]
fn rank_fusion_interleaves_and_pages_without_gaps_or_repeats() {
    let lanes = [lane(1, Some("m")), lane(2, Some("other"))];
    // Owner 1's scores all beat owner 2's; a score merge would bury
    // owner 2 entirely.
    let a: Vec<(u128, f32)> = vec![(10, 0.99), (11, 0.98), (12, 0.97)];
    let b: Vec<(u128, f32)> = vec![(20, 0.30), (21, 0.20)];
    let fetch = |rows: &[(u128, f32)], position: ListPosition, limit: usize| {
        let start = usize::try_from(position.consumed).unwrap();
        let slice = &rows[start.min(rows.len())..];
        let has_more = slice.len() > limit;
        (
            slice
                .iter()
                .take(limit)
                .map(|(id, score)| ranked(*id, *score))
                .collect::<Vec<_>>(),
            has_more,
        )
    };
    let mut after: Option<PageCursor> = None;
    let mut seen = Vec::new();
    for _ in 0..5 {
        let positions = cursor_positions(after.as_ref(), MergeKind::Rank, &lanes).unwrap();
        let (a_rows, a_more) = fetch(&a, positions[0], 2);
        let (b_rows, b_more) = fetch(&b, positions[1], 2);
        let lists = vec![
            RankedList {
                owner: lanes[0].space.owner,
                position: positions[0],
                rows: a_rows,
            },
            RankedList {
                owner: lanes[1].space.owner,
                position: positions[1],
                rows: b_rows,
            },
        ];
        let (page, _, next) = paginate_rank_fused(lists, 2, a_more || b_more, "fp");
        seen.extend(page.iter().map(|memory| memory.memory_id.as_u128()));
        let Some(next) = next else { break };
        after = Some(decode_cursor(&next, "fp").unwrap());
    }
    assert_eq!(seen, vec![20, 10, 21, 11, 12]);
}

#[test]
fn a_cursor_of_the_other_merge_kind_is_rejected() {
    let lanes = [lane(1, Some("m"))];
    let keyset = PageCursor::Keyset(crate::verbs::query::SearchCursor::Relevance {
        score_bits: 0.5_f32.to_bits(),
        memory_id: MemoryId::new(uuid::Uuid::from_u128(1)),
        seen: 1,
    });
    assert!(matches!(
        cursor_positions(Some(&keyset), MergeKind::Rank, &lanes),
        Err(McpToolError::InvalidInput(message)) if message.contains("malformed cursor")
    ));
    assert_eq!(
        cursor_positions(Some(&keyset), MergeKind::Keyset, &lanes)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn truncate_body_applies_default_hydration_cap() {
    let body = "x".repeat(DEFAULT_BODY_MAX_CHARS + 1);
    let (text, truncated) = truncate_body(&body, DEFAULT_BODY_MAX_CHARS);
    assert_eq!(text.chars().count(), DEFAULT_BODY_MAX_CHARS);
    assert!(truncated, "a body over the cap must flag truncation");
}

#[test]
fn truncate_body_respects_smaller_caller_cap() {
    assert_eq!(truncate_body("abcdef", 3), ("abc".to_string(), true));
}

#[test]
fn truncate_body_signals_no_truncation_when_body_fits() {
    // Exactly at the cap and comfortably under it both leave the text
    // whole and report body_truncated=false — the signal only fires on a
    // real cut. Multi-byte chars count by character, not byte.
    assert_eq!(truncate_body("abc", 3), ("abc".to_string(), false));
    assert_eq!(truncate_body("ab", 3), ("ab".to_string(), false));
    assert_eq!(truncate_body("héllo", 5), ("héllo".to_string(), false));
    assert_eq!(truncate_body("héllo", 2), ("hé".to_string(), true));
}

#[test]
fn effective_body_max_chars_keeps_server_ceiling() {
    assert_eq!(effective_body_max_chars(None), DEFAULT_BODY_MAX_CHARS);
    assert_eq!(effective_body_max_chars(Some(12)), 12);
    assert_eq!(
        effective_body_max_chars(Some(DEFAULT_BODY_MAX_CHARS + 1)),
        DEFAULT_BODY_MAX_CHARS
    );
}

#[test]
fn zero_body_max_chars_is_rejected_not_hydrated_empty() {
    assert!(validate_body_max_chars(Some(0)).is_err());
    assert!(validate_body_max_chars(Some(1)).is_ok());
    assert!(validate_body_max_chars(None).is_ok());
}

#[test]
fn kind_filter_accepts_all_casings_like_sibling_enums() {
    // `mode` and `supersession` already take UPPERCASE spellings;
    // the kind filter must not be the one arg that rejects them.
    for spelling in ["\"Fact\"", "\"fact\"", "\"FACT\""] {
        let kind: SearchMemoriesKind = serde_json::from_str(spelling).expect("valid kind");
        assert!(matches!(kind, SearchMemoriesKind::Fact));
    }
    // Folding case must not widen the accepted set.
    assert!(serde_json::from_str::<SearchMemoriesKind>("\"Note\"").is_err());
}

#[test]
fn oversized_space_and_tag_lists_are_rejected() {
    let mut too_many_spaces = args(SearchMemoriesMode::Lexical);
    too_many_spaces.spaces = (0..17).map(|i| format!("group:{i}")).collect();
    assert!(matches!(
        validate_list_caps(&too_many_spaces),
        Err(McpToolError::InvalidInput(message)) if message.contains("16 spaces")
    ));

    let mut too_many_tags = args(SearchMemoriesMode::Lexical);
    too_many_tags.tags = (0..17).map(|i| format!("tag-{i}")).collect();
    assert!(matches!(
        validate_list_caps(&too_many_tags),
        Err(McpToolError::InvalidInput(message)) if message.contains("16 tags")
    ));

    // Exactly at the cap passes; the caps bound work, they are not
    // off-by-one traps.
    let mut at_cap = args(SearchMemoriesMode::Lexical);
    at_cap.spaces = (0..16).map(|i| format!("group:{i}")).collect();
    at_cap.tags = (0..16).map(|i| format!("tag-{i}")).collect();
    assert!(validate_list_caps(&at_cap).is_ok());
}

#[test]
fn spaces_dedup_by_resolved_owner_not_raw_key() {
    let subject = crate::UserId::new(uuid::Uuid::now_v7());
    let ctx = crate::mcp::core_tools::memory_spaces::test_ctx::ctx_for(subject, vec![]);
    // `current` and `personal:<own uuid>` are two spellings of the
    // same space; a page must not search it twice and return every
    // hit doubled.
    let both_spellings = vec![
        "current".to_string(),
        format!("personal:{}", subject.into_inner()),
    ];
    let deduped = super::resolve_search_spaces(&ctx, &both_spellings).expect("valid spaces");
    assert_eq!(deduped.len(), 1, "one owner, one search");
    assert_eq!(deduped[0].owner, crate::OwnerRef::Personal(subject));

    // Both spellings must also continue each other's cursors: the
    // fingerprint is computed over resolved owners, so after dedup
    // it matches the single-spelling query exactly.
    let single = super::resolve_search_spaces(&ctx, &["current".to_string()]).expect("valid space");
    let lexical = |spaces: Vec<super::ResolvedMemorySpace>| -> Vec<OwnerLane> {
        spaces
            .into_iter()
            .map(|space| OwnerLane {
                space,
                arm: LaneArm::Lexical,
            })
            .collect()
    };
    let query_args = args(SearchMemoriesMode::Lexical);
    assert_eq!(
        super::query_fingerprint("needle", &query_args, None, None, &lexical(deduped)),
        super::query_fingerprint("needle", &query_args, None, None, &lexical(single)),
    );
}

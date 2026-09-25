use super::{
    LaneArm, MergeKind, OwnerLane, PageCursor, SearchMemoriesArgs, SearchMemoriesKind,
    SearchMemoriesMode, SearchMemoriesSupersession, cursor_positions, decode_cursor,
    degraded_to_lexical, encode_cursor, truncate_body, validate_body_max_chars, validate_list_caps,
    validate_score_args,
};
use crate::MemoryId;
use crate::mcp::McpToolError;
use crate::verbs::query::{SearchCursor, SearchMode, SearchOrder, TagMatch};

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
fn truncate_body_respects_smaller_caller_cap() {
    assert_eq!(truncate_body("abcdef", 3), ("abc".to_string(), true));
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

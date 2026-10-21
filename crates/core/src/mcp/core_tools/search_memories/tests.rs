use super::{
    LaneArm, MergeKind, OwnerLane, PageCursor, SearchMemoriesArgs, SearchMemoriesKind,
    SearchMemoriesMode, SearchMemoriesSupersession, cursor_positions, decode_cursor, encode_cursor,
    truncate_body, validate_body_max_chars, validate_list_caps, validate_score_args,
};
use crate::mcp::McpToolError;
use crate::verbs::query::{
    SearchCursor, SearchMode, SearchOrder, TagMatch,
    hybrid_degraded_to_lexical as degraded_to_lexical,
};
use crate::{MemoryId, OwnerId};

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
        stores: None,
    }
}

#[test]
fn stores_require_numbers_in_the_store_number_range() {
    for stores in [
        serde_json::json!(["wiki"]),
        serde_json::json!([65536]),
        serde_json::json!([-1]),
    ] {
        assert!(
            serde_json::from_value::<SearchMemoriesArgs>(
                serde_json::json!({"query": "needle", "stores": stores})
            )
            .is_err()
        );
    }
}

#[test]
fn local_and_remote_search_outputs_keep_the_recorded_writer() {
    use crate::storage_ports::other_stores::RemoteHit;
    use crate::verbs::query::{EntityKind, MemorySearchResult};
    use crate::{SchemaId, WriterAttribution};

    let store = 1;
    for via in [None, Some(uuid::Uuid::from_u128(3))] {
        let writer = WriterAttribution {
            by: uuid::Uuid::from_u128(2),
            via,
        };
        let local = super::search_memory_output(
            "current",
            MemorySearchResult {
                memory_id: MemoryId::new(crate::new_memory_uuid(0)),
                writer,
                kind: EntityKind::Fact,
                schema_id: SchemaId::new("core/agent-note-v1".into()),
                created_at: time::OffsetDateTime::UNIX_EPOCH,
                occurred_at: Some(time::OffsetDateTime::UNIX_EPOCH),
                snippet: "stored note".into(),
                score: 1.0,
                lexical_score: 1.0,
                similarity_score: 0.0,
            },
            Vec::new(),
            None,
            None,
        );
        let remote = super::remote_hit_output(
            store,
            RemoteHit {
                handle: crate::new_memory_uuid(store),
                writer,
                kind: EntityKind::Fact,
                schema_id: "remote/note".into(),
                snippet: "stored remote note".into(),
                created_at: time::OffsetDateTime::UNIX_EPOCH,
                tags: Vec::new(),
            },
        );
        for output in [local, remote] {
            let wire = serde_json::to_value(output).expect("wire output");
            assert_eq!(wire["by"], serde_json::json!(writer.by));
            assert_eq!(wire.get("via"), Some(&serde_json::json!(writer.via)));
        }
    }
}

#[test]
fn search_output_schema_requires_the_nullable_caller() {
    let schema = crate::mcp::schema::mcp_output_schema::<super::SearchMemoryOutput>()
        .expect("search output schema");
    let required = schema["required"].as_array().expect("required fields");
    assert!(required.contains(&serde_json::json!("by")));
    assert!(required.contains(&serde_json::json!("via")));
    assert_eq!(schema["properties"]["by"]["type"], "string");
    assert_eq!(
        schema["properties"]["via"]["type"],
        serde_json::json!(["string", "null"])
    );
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
        memory_id: MemoryId::new(crate::new_memory_uuid(0)),
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
fn score_based_flavor_warning_keeps_its_existing_contract() {
    // The public flavor helper retains its score heuristic. Served memory
    // search instead observes route availability, covered by its PG test.
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
    let owner = OwnerId::from(crate::UserId::new(uuid::Uuid::from_u128(owner)));
    OwnerLane {
        space: crate::mcp::core_tools::memory_spaces::ResolvedMemorySpace {
            key: owner.to_string(),
            label: owner.to_string(),
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
    too_many_spaces.spaces = (0..17)
        .map(|i| uuid::Uuid::from_u128(i).to_string())
        .collect();
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
    at_cap.spaces = (0..16)
        .map(|i| uuid::Uuid::from_u128(i).to_string())
        .collect();
    at_cap.tags = (0..16).map(|i| format!("tag-{i}")).collect();
    assert!(validate_list_caps(&at_cap).is_ok());
}

#[test]
fn spaces_dedup_by_resolved_owner_not_raw_key() {
    let subject = crate::UserId::new(uuid::Uuid::now_v7());
    let ctx = crate::mcp::core_tools::memory_spaces::test_ctx::ctx_for(subject, vec![]);
    // `current` and the caller's own bare uuid are two spellings of the
    // same space; a page must not search it twice and return every
    // hit doubled.
    let both_spellings = vec!["current".to_string(), subject.into_inner().to_string()];
    let deduped = super::resolve_search_spaces(&ctx, &both_spellings).expect("valid spaces");
    assert_eq!(deduped.len(), 1, "one owner, one search");
    assert_eq!(deduped[0].owner, OwnerId::from(subject));

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

mod other_stores {
    use std::sync::Arc;

    use futures::future::BoxFuture;

    use super::super::{
        SearchMemoriesKind, SearchMemoriesMode, SearchMemoriesOutput, SearchMemoriesSupersession,
        SearchMemoriesTool, SearchMemoryOutput, SearchRanking, UnansweredStore,
        caller_input_schema, merge_store_hits,
    };
    use crate::McpTool;
    use crate::storage_ports::other_stores::{
        OtherStores, OtherStoresError, OtherStoresService, RemoteHit, RemoteRead, RemoteSearch,
        RemoteSearchResult, StoreCaller, StoreInfo,
    };
    use crate::verbs::query::{EntityKind, SearchMode, SearchOrder, SupersessionStatus, TagMatch};

    fn local_hit() -> SearchMemoryOutput {
        let memory_id = crate::new_memory_uuid(0);
        SearchMemoryOutput {
            memory_id,
            memory: memory_id.to_string(),
            space: "current".into(),
            by: uuid::Uuid::from_u128(10),
            via: None,
            kind: crate::EntityKind::Fact,
            schema_id: "core/agent-note-v1".into(),
            created_at: time::OffsetDateTime::UNIX_EPOCH,
            occurred_at: Some(time::OffsetDateTime::UNIX_EPOCH),
            snippet: String::new(),
            score: 1.0,
            lexical_score: 1.0,
            similarity_score: 0.0,
            tags: Vec::new(),
            body: None,
            body_truncated: None,
            store: None,
        }
    }

    fn remote_hit(store: u16) -> RemoteHit {
        RemoteHit {
            handle: crate::new_memory_uuid(store),
            writer: crate::WriterAttribution {
                by: uuid::Uuid::from_u128(20),
                via: Some(uuid::Uuid::from_u128(21)),
            },
            kind: EntityKind::Fact,
            schema_id: "remote/note".into(),
            snippet: String::new(),
            created_at: time::OffsetDateTime::UNIX_EPOCH,
            tags: Vec::new(),
        }
    }

    #[test]
    fn the_merged_page_is_cut_to_the_limit() {
        let local: Vec<_> = (0..3).map(|_| local_hit()).collect();
        let wiki: Vec<_> = (0..3).map(|_| remote_hit(1)).collect();
        let crm: Vec<_> = (0..3).map(|_| remote_hit(2)).collect();
        let expected = [
            local[0].memory.clone(),
            wiki[0].handle.to_string(),
            crm[0].handle.to_string(),
            local[1].memory.clone(),
        ];
        let mut output = SearchMemoriesOutput {
            mode: SearchMode::Lexical,
            degraded_to_lexical: false,
            ranking: SearchRanking::Score,
            memories: local,
            neighbor_edges: Vec::new(),
            next_cursor: None,
            has_more: false,
            unanswered: None,
            inventory_unanswered: None,
        };
        merge_store_hits(
            &mut output,
            vec![
                (
                    1,
                    RemoteSearchResult {
                        hits: wiki,
                        degraded_to_lexical: false,
                    },
                ),
                (
                    2,
                    RemoteSearchResult {
                        hits: crm,
                        degraded_to_lexical: false,
                    },
                ),
            ],
            vec![UnansweredStore {
                store: 3,
                why: "deadline has elapsed".into(),
            }],
            4,
            SearchOrder::Relevance,
            0,
        );
        let order: Vec<&str> = output
            .memories
            .iter()
            .map(|hit| hit.memory.as_str())
            .collect();
        // Rank 0 of every list, then rank 1 of the first: RRF, cut at 4.
        assert_eq!(order, expected);
        assert!(output.memories.iter().all(|hit| hit.store.is_some()));
        assert_eq!(output.ranking, SearchRanking::Rank);
        assert_eq!(
            output.unanswered,
            Some(vec![UnansweredStore {
                store: 3,
                why: "deadline has elapsed".into(),
            }])
        );
    }

    #[test]
    fn recency_orders_all_store_candidates_before_the_global_limit() {
        let mut local = vec![local_hit(), local_hit()];
        local[0].created_at += time::Duration::seconds(100);
        local[1].created_at += time::Duration::seconds(50);
        let mut wiki = vec![remote_hit(1), remote_hit(1)];
        wiki[0].created_at += time::Duration::seconds(300);
        wiki[1].created_at += time::Duration::seconds(100);
        let mut crm = remote_hit(2);
        crm.created_at += time::Duration::seconds(200);
        let expected = [
            wiki[0].handle.to_string(),
            crm.handle.to_string(),
            local[0].memory.clone(),
            wiki[1].handle.to_string(),
        ];
        let mut output = SearchMemoriesOutput {
            mode: SearchMode::Hybrid,
            degraded_to_lexical: true,
            ranking: SearchRanking::Score,
            memories: local,
            neighbor_edges: Vec::new(),
            next_cursor: Some("opaque-local-cursor".into()),
            has_more: true,
            unanswered: None,
            inventory_unanswered: None,
        };
        merge_store_hits(
            &mut output,
            vec![
                (
                    1,
                    RemoteSearchResult {
                        hits: wiki,
                        degraded_to_lexical: false,
                    },
                ),
                (
                    2,
                    RemoteSearchResult {
                        hits: vec![crm],
                        degraded_to_lexical: false,
                    },
                ),
            ],
            Vec::new(),
            4,
            SearchOrder::Recency,
            0,
        );
        assert_eq!(
            output
                .memories
                .iter()
                .map(|hit| hit.memory.as_str())
                .collect::<Vec<_>>(),
            expected
        );
        assert!(output.memories.iter().all(|hit| hit.score > 0.0));
        assert_eq!(output.ranking, SearchRanking::Rank);
        assert_eq!(output.next_cursor.as_deref(), Some("opaque-local-cursor"));
        assert!(output.has_more);
        assert!(output.degraded_to_lexical);
    }

    struct CapturingStore {
        listings: std::sync::atomic::AtomicUsize,
        requests: std::sync::Mutex<Vec<RemoteSearch>>,
        reply: Result<RemoteSearchResult, OtherStoresError>,
    }

    impl CapturingStore {
        fn new(reply: Result<RemoteSearchResult, OtherStoresError>) -> Self {
            Self {
                listings: std::sync::atomic::AtomicUsize::new(0),
                requests: std::sync::Mutex::new(Vec::new()),
                reply,
            }
        }
    }

    impl OtherStores for CapturingStore {
        fn stores<'a>(
            &'a self,
            _caller: StoreCaller<'a>,
        ) -> BoxFuture<'a, Result<Vec<StoreInfo>, OtherStoresError>> {
            self.listings
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Box::pin(async {
                Ok(vec![StoreInfo {
                    number: 1,
                    name: "wiki".into(),
                    description: String::new(),
                    schema_ids: vec!["remote/note".into()],
                }])
            })
        }
        fn search<'a>(
            &'a self,
            _caller: StoreCaller<'a>,
            _store: u16,
            args: &'a RemoteSearch,
        ) -> BoxFuture<'a, Result<RemoteSearchResult, OtherStoresError>> {
            self.requests.lock().unwrap().push(args.clone());
            Box::pin(async { self.reply.clone() })
        }
        fn read<'a>(
            &'a self,
            _caller: StoreCaller<'a>,
            _handle: uuid::Uuid,
            _at: crate::storage_ports::other_stores::ReadAt,
        ) -> BoxFuture<'a, Result<RemoteRead, OtherStoresError>> {
            Box::pin(async { Ok(RemoteRead::NotFound) })
        }
    }

    fn remote_ctx(port: &Arc<CapturingStore>) -> crate::mcp::McpToolCtx {
        let mut ctx = super::super::super::memory_spaces::test_ctx::ctx_for(
            crate::UserId::new(uuid::Uuid::now_v7()),
            Vec::new(),
        );
        ctx.engine = Some(Arc::new(crate::Engine::new(ctx.registry.as_ref().clone())));
        ctx.services = crate::FlavorServices::with(OtherStoresService::new(port.clone()));
        ctx
    }

    fn remote_args(mode: SearchMemoriesMode) -> super::super::SearchMemoriesArgs {
        let mut request = super::args(mode);
        request.stores = Some(vec![1]);
        request
    }

    #[tokio::test]
    async fn remote_search_receives_every_filter_and_the_requested_mode() {
        let since = time::OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap();
        let until = since + time::Duration::days(1);
        for mode in [SearchMemoriesMode::Hybrid, SearchMemoriesMode::Semantic] {
            let port = Arc::new(CapturingStore::new(Ok(RemoteSearchResult {
                hits: Vec::new(),
                degraded_to_lexical: false,
            })));
            let mut request = remote_args(mode);
            request.limit = 100;
            request.kind = Some(SearchMemoriesKind::Fact);
            request.schema_id = Some("remote/note".into());
            request.tags = vec![" Rust ".into(), "rust".into(), String::new(), "ZED".into()];
            request.tag_match = TagMatch::All;
            request.since = Some(
                since
                    .format(&time::format_description::well_known::Rfc3339)
                    .unwrap(),
            );
            request.until = Some(
                until
                    .format(&time::format_description::well_known::Rfc3339)
                    .unwrap(),
            );
            request.order = SearchOrder::Recency;
            request.supersession = SearchMemoriesSupersession::All;
            request.min_score = Some(0.4);
            request.semantic_weight = matches!(mode, SearchMemoriesMode::Hybrid).then_some(0.7);
            let semantic_weight = request.semantic_weight;
            SearchMemoriesTool::call(remote_ctx(&port), request)
                .await
                .unwrap();
            assert_eq!(
                *port.requests.lock().unwrap(),
                vec![RemoteSearch {
                    query: "needle".into(),
                    limit: 50,
                    kind: Some(EntityKind::Fact),
                    schema_id: Some("remote/note".into()),
                    tags: vec!["rust".into(), "zed".into()],
                    tag_match: TagMatch::All,
                    mode: SearchMode::from(mode),
                    since: Some(since),
                    until: Some(until),
                    order: SearchOrder::Recency,
                    supersession: SupersessionStatus::IncludeSuperseded,
                    min_score: Some(0.4),
                    semantic_weight,
                }]
            );
        }
    }

    #[tokio::test]
    async fn invalid_remote_only_timestamps_are_refused_before_any_port_call() {
        for (since, until, field) in [
            (Some("not-a-time".into()), None, "since"),
            (None, Some("not-a-time".into()), "until"),
        ] {
            let port = Arc::new(CapturingStore::new(Ok(RemoteSearchResult {
                hits: Vec::new(),
                degraded_to_lexical: false,
            })));
            let mut request = remote_args(SearchMemoriesMode::Lexical);
            request.since = since;
            request.until = until;
            let result = SearchMemoriesTool::call(remote_ctx(&port), request).await;
            assert!(
                matches!(result, Err(crate::mcp::McpToolError::InvalidInput(message)) if message.contains(field))
            );
            assert_eq!(port.listings.load(std::sync::atomic::Ordering::SeqCst), 0);
            assert!(port.requests.lock().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn remote_fallback_is_reported_with_empty_or_capped_hits() {
        let first = remote_hit(1);
        let first_id = first.handle;
        for hits in [Vec::new(), vec![first, remote_hit(1)]] {
            let expected = hits.len().min(1);
            let port = Arc::new(CapturingStore::new(Ok(RemoteSearchResult {
                hits,
                degraded_to_lexical: true,
            })));
            let mut request = remote_args(SearchMemoriesMode::Hybrid);
            request.limit = 1;
            let result = SearchMemoriesTool::call(remote_ctx(&port), request)
                .await
                .unwrap();
            assert_eq!(result.memories.len(), expected);
            if let Some(hit) = result.memories.first() {
                assert_eq!(hit.memory, first_id.to_string());
                assert_eq!(hit.memory_id, first_id);
            }
            assert!(result.degraded_to_lexical);
            assert_eq!(result.unanswered, Some(Vec::new()));
            assert_eq!(port.requests.lock().unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn unsupported_remote_search_is_unanswered_without_a_weaker_retry() {
        let port = Arc::new(CapturingStore::new(Err(
            OtherStoresError::UnsupportedSearch("all-tag matching".into()),
        )));
        let mut request = remote_args(SearchMemoriesMode::Hybrid);
        request.tags = vec!["rust".into(), "guide".into()];
        request.tag_match = TagMatch::All;
        let result = SearchMemoriesTool::call(remote_ctx(&port), request)
            .await
            .unwrap();
        assert_eq!(
            result.unanswered,
            Some(vec![UnansweredStore {
                store: 1,
                why: "store cannot honor search: all-tag matching".into(),
            }])
        );
        assert!(result.memories.is_empty());
        let requests = port.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].tag_match, TagMatch::All);
        assert_eq!(requests[0].tags, ["guide", "rust"]);
    }

    struct TwoStores;

    impl OtherStores for TwoStores {
        fn stores<'a>(
            &'a self,
            _caller: StoreCaller<'a>,
        ) -> BoxFuture<'a, Result<Vec<StoreInfo>, OtherStoresError>> {
            Box::pin(async {
                Ok(vec![
                    StoreInfo {
                        number: 1,
                        name: "wiki".into(),
                        description: "Team wiki pages".into(),
                        schema_ids: Vec::new(),
                    },
                    StoreInfo {
                        number: 2,
                        name: "crm".into(),
                        description: "Customer records".into(),
                        schema_ids: Vec::new(),
                    },
                ])
            })
        }
        fn search<'a>(
            &'a self,
            _caller: StoreCaller<'a>,
            _store: u16,
            _args: &'a RemoteSearch,
        ) -> BoxFuture<'a, Result<RemoteSearchResult, OtherStoresError>> {
            Box::pin(async {
                Ok(RemoteSearchResult {
                    hits: Vec::new(),
                    degraded_to_lexical: false,
                })
            })
        }
        fn read<'a>(
            &'a self,
            _caller: StoreCaller<'a>,
            _handle: uuid::Uuid,
            _at: crate::storage_ports::other_stores::ReadAt,
        ) -> BoxFuture<'a, Result<RemoteRead, OtherStoresError>> {
            Box::pin(async { Ok(RemoteRead::NotFound) })
        }
    }

    fn static_schema() -> serde_json::Value {
        crate::mcp::schema::mcp_tool_schema::<super::super::SearchMemoriesArgs>().schema
    }

    #[tokio::test]
    async fn without_a_port_the_schema_is_the_static_one() {
        let ctx = super::super::super::memory_spaces::test_ctx::ctx_for(
            crate::UserId::new(uuid::Uuid::now_v7()),
            Vec::new(),
        );
        let schema = static_schema();
        assert!(schema["properties"].get("stores").is_none(), "{schema}");
        assert_eq!(
            caller_input_schema(&ctx, schema.clone())
                .await
                .expect("no port"),
            schema
        );
    }

    #[tokio::test]
    async fn with_a_port_stores_is_an_enum_of_the_callers_stores() {
        let mut ctx = super::super::super::memory_spaces::test_ctx::ctx_for(
            crate::UserId::new(uuid::Uuid::now_v7()),
            Vec::new(),
        );
        ctx.engine = Some(Arc::new(crate::Engine::new(ctx.registry.as_ref().clone())));
        ctx.services = crate::FlavorServices::with(OtherStoresService::new(Arc::new(TwoStores)));
        let schema = caller_input_schema(&ctx, static_schema())
            .await
            .expect("valid inventory");
        let stores = &schema["properties"]["stores"];
        assert_eq!(stores["type"], "array");
        assert_eq!(stores["items"]["enum"], serde_json::json!([0, 1, 2]));
        assert_eq!(stores["items"]["oneOf"][1]["const"], 1);
        assert_eq!(
            stores["items"]["oneOf"][1]["description"],
            "Team wiki pages"
        );
        let prose = stores["description"].as_str().expect("description");
        assert!(prose.contains("`2` (crm): Customer records"), "{prose}");
        // Everything else is the static schema.
        let mut without = schema.clone();
        without["properties"]
            .as_object_mut()
            .expect("props")
            .remove("stores");
        assert_eq!(without, static_schema());
    }
}

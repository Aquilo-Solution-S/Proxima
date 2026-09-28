use super::{
    CHUNK_CURSOR, ChunkCandidateRow, ChunkCursorPos, ChunkMatch, ChunkMatchDetail, ChunkPage,
    ChunkSearchMode, CodeChunkV1, CodeChunkVectorCandidate, CodeChunkVectorFilters,
    CodeSearchChunksArgs, CodeSearchChunksOutput, FileClass, FileState, HashMap, MatchScores,
    MemoryId, ResolvedChunkQuery, SemanticScan, SemanticWeight, distinctive_terms, fuse_candidates,
    match_metadata, pointer_words, ranks_after_chunk_cursor, reject_unknown_language,
    render_snippet, requested_semantic_weight, same_word, select_chunk_page, semantic_scan,
    wire_cursor,
};
use crate::chunker::LANGUAGE_LABELS;

const CHUNK: &str = "fn drain(queue: &Queue) {\n    let batch = queue.take();\n    // retry a failed batch with backoff\n    send_with_retries(batch);\n}";

#[test]
fn a_full_text_match_points_at_the_line_sharing_most_query_words() {
    let (kind, line, excerpt) = match_metadata(
        "where are failed batches retried",
        "src/drain.rs",
        CHUNK,
        40,
        true,
    );
    assert_eq!(kind, "full_text");
    assert_eq!(line, Some(42), "the line with batch, failed and retry");
    assert_eq!(
        excerpt.as_deref(),
        Some("// retry a failed batch with backoff")
    );
}

#[test]
fn only_a_lexical_hit_gets_a_word_pointer() {
    let (kind, line, excerpt) = match_metadata(
        "where are failed batches retried",
        "src/drain.rs",
        CHUNK,
        40,
        false,
    );
    assert_eq!((kind.as_str(), line, excerpt), ("full_text", None, None));
    let (_, line, _) = match_metadata(
        "how does the code handle it",
        "src/drain.rs",
        CHUNK,
        40,
        true,
    );
    assert_eq!(line, None, "stopwords and short words point nowhere");
}

#[test]
fn a_line_holding_the_whole_query_still_wins() {
    let (kind, line, _) = match_metadata("queue.take()", "src/drain.rs", CHUNK, 40, true);
    assert_eq!((kind.as_str(), line), ("text_contains", Some(41)));
    let (kind, line, _) = match_metadata("drain.rs", "src/drain.rs", CHUNK, 40, true);
    assert_eq!((kind.as_str(), line), ("path_contains", None));
}

#[test]
fn words_split_at_humps_and_underscores_and_match_up_to_their_ending() {
    let words: Vec<String> =
        pointer_words("getModuleScriptSources(ASTNode, max_chunk_chars)").collect();
    assert_eq!(
        words,
        [
            "get", "module", "script", "sources", "ast", "node", "max", "chunk", "chars"
        ]
    );
    for (a, b) in [
        ("retries", "retry"),
        ("resolved", "resolution"),
        ("chunk", "chunker"),
        ("configuration", "configure"),
        ("api", "api"),
    ] {
        assert!(same_word(a, b), "{a} ~ {b}");
    }
    for (a, b) in [("module", "modal"), ("log", "logger"), ("batch", "backoff")] {
        assert!(!same_word(a, b), "{a} !~ {b}");
    }
}

#[test]
fn a_window_numbers_the_lines_around_the_pointer() {
    assert_eq!(
        render_snippet(CHUNK, 40, Some((42, 1)), 2_000),
        (
            "41:     let batch = queue.take();\n42:     // retry a failed batch with backoff\n43:     send_with_retries(batch);".to_string(),
            true
        )
    );
    assert_eq!(
        render_snippet(CHUNK, 40, Some((40, 0)), 2_000),
        ("40: fn drain(queue: &Queue) {".to_string(), true),
    );
    let (whole, truncated) = render_snippet(CHUNK, 40, Some((42, 10)), 2_000);
    assert!(whole.starts_with("40: fn drain") && whole.ends_with("44: }"));
    assert!(!truncated, "a window over the whole chunk holds all of it");
    let (cut, truncated) = render_snippet(CHUNK, 40, Some((42, 10)), 12);
    assert_eq!((cut.as_str(), truncated), ("40: fn drain", true));
}

#[test]
fn without_a_window_the_snippet_is_the_chunk_from_its_start() {
    assert_eq!(
        render_snippet(CHUNK, 40, None, 2_000),
        (CHUNK.to_string(), false)
    );
    assert_eq!(
        render_snippet(CHUNK, 40, None, 8),
        ("fn drain".to_string(), true)
    );
}

fn lean_match(detail: Option<ChunkMatchDetail>) -> ChunkMatch {
    ChunkMatch {
        handle: "A:1".into(),
        repo_handle: None,
        file_path: "src/drain.rs".into(),
        chunk_type: "function".into(),
        file_class: None,
        line_range: (40, 44),
        snippet: "fn drain".into(),
        snippet_truncated: true,
        matched_line: None,
        score: 1.5,
        detail,
    }
}

/// The default match carries the fields an agent reads, cites and
/// opens, and nothing else; not even a null.
#[test]
fn a_lean_match_serializes_only_what_an_agent_reads() {
    let lean = serde_json::to_value(lean_match(None)).expect("serializes");
    let keys: Vec<&str> = lean
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        [
            "handle",
            "file_path",
            "chunk_type",
            "line_range",
            "snippet",
            "snippet_truncated",
            "score"
        ]
    );
    let verbose = serde_json::to_value(lean_match(Some(ChunkMatchDetail {
        language: Some("rust".into()),
        chunk_index: 3,
        byte_range: (10, 90),
        match_kind: "full_text".into(),
        matched_excerpt: None,
        lexical_score: 1.5,
        similarity_score: 0.0,
    })))
    .expect("serializes");
    assert_eq!(verbose["language"], "rust");
    assert_eq!(verbose["chunk_index"], 3);
    assert_eq!(verbose["lexical_score"], 1.5);
    assert!(verbose["matched_excerpt"].is_null());
}

/// The advertised output schema must accept a lean match: the verbose
/// fields and the omitted ones are optional properties, not required.
#[test]
fn the_output_schema_requires_only_the_lean_fields() {
    let schema = serde_json::to_value(schemars::schema_for!(CodeSearchChunksOutput))
        .expect("schema serializes");
    let chunk = &schema["$defs"]["ChunkMatch"];
    let mut required: Vec<&str> = chunk["required"]
        .as_array()
        .expect("required list")
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect();
    required.sort_unstable();
    assert_eq!(
        required,
        [
            "chunk_type",
            "file_path",
            "handle",
            "line_range",
            "score",
            "snippet",
            "snippet_truncated"
        ]
    );
    for verbose_field in [
        "lexical_score",
        "similarity_score",
        "byte_range",
        "match_kind",
    ] {
        assert!(
            chunk["properties"].get(verbose_field).is_some(),
            "{verbose_field} is not described"
        );
    }
}

fn id(byte: u8) -> uuid::Uuid {
    uuid::Uuid::from_bytes([byte; 16])
}

#[test]
fn language_filter_accepts_exactly_the_chunker_labels() {
    for label in LANGUAGE_LABELS {
        assert!(reject_unknown_language(Some(label)).is_ok(), "{label}");
    }
    assert!(reject_unknown_language(None).is_ok());
    for bad in ["Python", "py", "js", "cobol", ""] {
        assert!(reject_unknown_language(Some(bad)).is_err(), "{bad:?}");
    }
}

/// The agent-facing description lists the accepted labels by hand; this
/// keeps it from drifting when the chunker gains one.
#[test]
fn language_description_names_every_label() {
    let schema = serde_json::to_value(schemars::schema_for!(CodeSearchChunksArgs))
        .expect("schema serializes");
    let description = schema["properties"]["language"]["description"]
        .as_str()
        .expect("language has a description");
    for label in LANGUAGE_LABELS {
        assert!(
            description.contains(&format!("`{label}`")),
            "description is missing `{label}`: {description}"
        );
    }
}

/// The property the rare bands depend on: a question with no identifiers
/// must produce no terms, so `rare_all_tsq`/`rare_any_tsq` bind NULL and
/// the rare bands contribute nothing.
#[test]
fn prose_questions_yield_no_distinctive_terms() {
    for q in [
        "how does the code chunker decide how big a chunk should be",
        "where is an input that the embedding provider rejected as too long split",
        "may a self hosted issuer serve its key set over plain http on loopback",
        "Fix Windows progress rendering",
    ] {
        assert_eq!(distinctive_terms(q), "", "query: {q}");
    }
}

#[test]
fn identifier_shapes_are_picked_up() {
    assert_eq!(
        distinctive_terms("the getModuleScriptSources helper only detects src tags"),
        "getModuleScriptSources"
    );
    assert_eq!(
        distinctive_terms("MAX_CHUNK_CHARS is the hard upper bound"),
        "MAX_CHUNK_CHARS"
    );
    assert_eq!(distinctive_terms("decode as utf8 please"), "utf8");
}

/// Punctuation is a separator, so a scoped package name contributes its
/// parts and `resolveFromAST` survives the surrounding backticks.
#[test]
fn punctuation_separates_without_swallowing_identifiers() {
    assert_eq!(
        distinctive_terms("`resolveFromAST` broke for @tailwindcss/postcss v4.1"),
        "resolveFromAST"
    );
}

#[test]
fn short_and_unstructured_tokens_are_rejected() {
    // `id` is too short, `fs` too short, `plugin` unstructured, `A1` too
    // short even though structured.
    assert_eq!(distinctive_terms("id fs plugin A1 the config"), "");
}

fn owner(byte: u8) -> proxima_core::Owner {
    proxima_core::Owner::Personal(proxima_core::UserId::new(id(byte)))
}

fn resolved() -> ResolvedChunkQuery<'static> {
    ResolvedChunkQuery {
        owner: owner(1),
        query: "parse_chunk",
        requested_mode: ChunkSearchMode::Hybrid,
        semantic_weight: None,
        repo_id: Some(id(9)),
        language: Some("rust"),
        chunk_type: Some("function"),
        file_class: None,
        exact_pattern: "%parse\\_chunk%".to_string(),
    }
}

/// The check that makes "the cursor binds the whole resolved query" an
/// executable statement rather than a review convention: every field the
/// fingerprint canon names must be able to move it. A field dropped from
/// the canon fails here instead of silently letting page 2 resume a
/// different candidate set under a matching cursor.
#[test]
fn every_resolved_field_moves_the_fingerprint() {
    let base = resolved();
    let baseline = base.fingerprint();

    let cases: Vec<(&str, ResolvedChunkQuery<'_>)> = vec![
        (
            "owner",
            ResolvedChunkQuery {
                owner: owner(2),
                ..resolved()
            },
        ),
        (
            "query",
            ResolvedChunkQuery {
                query: "parse_chunks",
                ..resolved()
            },
        ),
        (
            "requested_mode",
            ResolvedChunkQuery {
                requested_mode: ChunkSearchMode::Lexical,
                ..resolved()
            },
        ),
        (
            "repo_id",
            ResolvedChunkQuery {
                repo_id: Some(id(10)),
                ..resolved()
            },
        ),
        (
            "language",
            ResolvedChunkQuery {
                language: Some("typescript"),
                ..resolved()
            },
        ),
        (
            "chunk_type",
            ResolvedChunkQuery {
                chunk_type: Some("class"),
                ..resolved()
            },
        ),
        (
            "semantic_weight",
            ResolvedChunkQuery {
                semantic_weight: Some(weight(0.8)),
                ..resolved()
            },
        ),
        (
            "file_class",
            ResolvedChunkQuery {
                file_class: Some(FileClass::Lockfile),
                ..resolved()
            },
        ),
    ];

    for (field, flipped) in cases {
        assert_ne!(
            baseline,
            flipped.fingerprint(),
            "changing {field} left the cursor fingerprint unchanged"
        );
    }
}

/// `language` and `chunk_type` are adjacent `Option<&str>` values in the
/// canon, so transposing them would compile. It must not fingerprint the
/// same: a cursor minted for one filter would otherwise be accepted for
/// the other and resume page 1's keyset over a different candidate set.
#[test]
fn transposing_language_and_chunk_type_moves_the_fingerprint() {
    let language_only = ResolvedChunkQuery {
        language: Some("rust"),
        chunk_type: None,
        ..resolved()
    };
    let chunk_type_only = ResolvedChunkQuery {
        language: None,
        chunk_type: Some("rust"),
        ..resolved()
    };
    assert_ne!(
        language_only.fingerprint(),
        chunk_type_only.fingerprint(),
        "language and chunk_type are interchangeable in the cursor canon"
    );
}

fn weight(value: f32) -> SemanticWeight {
    SemanticWeight::new(value).expect("weight in range")
}

/// A cursor minted before `semantic_weight` existed still resumes: a
/// query without a weight fingerprints the six-value canon byte for
/// byte, and two weights fingerprint apart (#355).
#[test]
fn an_unweighted_query_keeps_the_six_value_canon() {
    let six_values = serde_json::json!([
        owner(1).external_key(),
        "parse_chunk",
        "hybrid",
        Some(id(9)),
        Some("rust"),
        Some("function"),
    ]);
    assert_eq!(
        resolved().fingerprint(),
        wire_cursor::fingerprint(&six_values.to_string())
    );
    let weighted = |value| ResolvedChunkQuery {
        semantic_weight: Some(weight(value)),
        ..resolved()
    };
    assert_ne!(weighted(0.7).fingerprint(), weighted(0.8).fingerprint());
    let classed = |class| ResolvedChunkQuery {
        file_class: Some(class),
        ..resolved()
    };
    assert_ne!(
        classed(FileClass::Generated).fingerprint(),
        classed(FileClass::Vendored).fingerprint()
    );
}

fn chunk_row(id_byte: u8, class: FileClass) -> (MemoryId, CodeChunkV1) {
    (
        MemoryId::new(id(id_byte)),
        CodeChunkV1 {
            repo_id: id(9),
            file_path: format!("f{id_byte}.rs"),
            chunk_index: 0,
            text: String::new(),
            language: Some("rust".into()),
            chunk_type: "function".into(),
            byte_range_start: 0,
            byte_range_end: 0,
            line_range_start: 1,
            line_range_end: 1,
            state: FileState::Present,
            file_class: class,
            calls: Vec::new(),
        },
    )
}

fn page_ids(page: &ChunkPage) -> Vec<uuid::Uuid> {
    page.eligible
        .iter()
        .map(|(memory_id, ..)| memory_id.into_inner())
        .collect()
}

/// Source first puts every source row ahead of every other one and
/// keeps the fused order inside each tier; without it the fused order
/// stands. Paging resumes inside the right tier (#360).
#[test]
fn source_first_pages_every_source_match_ahead_of_the_rest() {
    // Fused order: 5 (lockfile), 4, 3 (vendored), 2, 1.
    let rows = vec![
        chunk_row(5, FileClass::Lockfile),
        chunk_row(4, FileClass::Source),
        chunk_row(3, FileClass::Vendored),
        chunk_row(2, FileClass::Source),
        chunk_row(1, FileClass::Source),
    ];
    let scores: HashMap<uuid::Uuid, MatchScores> = (1..=5_u8)
        .map(|byte| {
            (
                id(byte),
                MatchScores {
                    memory_id: id(byte),
                    score: f32::from(byte),
                    ..MatchScores::default()
                },
            )
        })
        .collect();

    let arm_order = select_chunk_page(rows.clone(), &scores, None, 10, "fp", 0, false);
    assert_eq!(page_ids(&arm_order), [id(5), id(4), id(3), id(2), id(1)]);

    let first = select_chunk_page(rows.clone(), &scores, None, 2, "fp", 0, true);
    assert_eq!(page_ids(&first), [id(4), id(2)]);
    let cursor = first.next_cursor.expect("more remain");
    let pos: ChunkCursorPos = CHUNK_CURSOR.decode("fp", &cursor).expect("decodes");
    let second = select_chunk_page(rows.clone(), &scores, Some(pos), 2, "fp", 2, true);
    assert_eq!(page_ids(&second), [id(1), id(5)]);
    let pos: ChunkCursorPos = CHUNK_CURSOR
        .decode("fp", &second.next_cursor.expect("one left"))
        .expect("decodes");
    let third = select_chunk_page(rows, &scores, Some(pos), 2, "fp", 4, true);
    assert_eq!(page_ids(&third), [id(3)]);
    assert!(!third.has_more);
}

/// A cursor minted before tiers existed carries none and resumes as
/// tier 0, which every row was.
#[test]
fn a_cursor_without_a_tier_resumes_in_tier_zero() {
    let legacy = serde_json::json!({
        "score_bits": 2.0_f32.to_bits(),
        "memory_id": id(2),
        "seen": 1,
    });
    let pos: ChunkCursorPos = serde_json::from_value(legacy).expect("decodes");
    assert_eq!(pos.tier, 0);
    let scores = |byte: u8, tier: u8| MatchScores {
        memory_id: id(byte),
        score: f32::from(byte),
        tier,
        ..MatchScores::default()
    };
    assert!(ranks_after_chunk_cursor(scores(1, 0), pos));
    assert!(!ranks_after_chunk_cursor(scores(3, 0), pos));
    assert!(ranks_after_chunk_cursor(scores(3, 1), pos));
}

fn lexical(id_byte: u8, literal_bonus: f32) -> ChunkCandidateRow {
    ChunkCandidateRow {
        memory_id: id(id_byte),
        score: 1.0 + literal_bonus,
        literal_bonus,
    }
}

fn semantic(id_byte: u8) -> CodeChunkVectorCandidate {
    CodeChunkVectorCandidate {
        memory_id: id(id_byte),
        similarity_score: 0.5,
    }
}

fn fused_order(
    value: f32,
    lexical: &[ChunkCandidateRow],
    semantic: &[CodeChunkVectorCandidate],
) -> Vec<uuid::Uuid> {
    fuse_candidates(ChunkSearchMode::Hybrid, weight(value), lexical, &[semantic])
        .into_iter()
        .map(|scores| scores.memory_id)
        .collect()
}

/// The even weight is plain rank fusion; the ends follow one arm; and a
/// literal hit ranks first at every weight (#355).
#[test]
fn the_weight_moves_the_fusion_but_not_a_literal_hit() {
    let lexical_arm = [lexical(1, 0.0), lexical(2, 0.0)];
    let semantic_arm = [semantic(3), semantic(2)];

    let even = fuse_candidates(
        ChunkSearchMode::Hybrid,
        SemanticWeight::EVEN,
        &lexical_arm,
        &[&semantic_arm],
    );
    let both = even
        .iter()
        .find(|scores| scores.memory_id == id(2))
        .expect("in both arms");
    assert_eq!(
        both.score.to_bits(),
        (1.0_f32 / 62.0 + 1.0 / 62.0).to_bits()
    );
    assert_eq!(even[0].memory_id, id(2));

    assert_eq!(
        fused_order(1.0, &lexical_arm, &semantic_arm)[..2],
        [id(3), id(2)]
    );
    assert_eq!(
        fused_order(0.0, &lexical_arm, &semantic_arm)[..2],
        [id(1), id(2)]
    );

    let literal_last = [lexical(4, 0.0), lexical(5, 4.0)];
    for value in [0.0, 0.5, 1.0] {
        assert_eq!(
            fused_order(value, &literal_last, &[semantic(4)])[0],
            id(5),
            "weight {value}"
        );
    }
}

/// Two semantic slices each start at rank 0. Concatenating them would
/// make the second slice's best neighbour a rank-1 hit and let it lose
/// to a weaker chunk from the first slice.
#[test]
fn each_semantic_arm_keeps_its_own_reciprocal_rank() {
    let source = [semantic(1)];
    let other = [semantic(2)];
    let split = fuse_candidates(
        ChunkSearchMode::Hybrid,
        SemanticWeight::EVEN,
        &[],
        &[&source, &other],
    );
    let score = |rows: &[MatchScores], byte: u8| {
        rows.iter()
            .find(|scores| scores.memory_id == id(byte))
            .expect("present")
            .score
    };
    let rank0: f32 = 1.0 / 61.0;
    assert_eq!(score(&split, 1).to_bits(), rank0.to_bits());
    assert_eq!(score(&split, 2).to_bits(), rank0.to_bits());

    let concatenated = fuse_candidates(
        ChunkSearchMode::Hybrid,
        SemanticWeight::EVEN,
        &[],
        &[&[semantic(1), semantic(2)]],
    );
    assert!(
        score(&concatenated, 2) < score(&concatenated, 1),
        "one slice ranks the second neighbour behind the first"
    );
}

/// Only an unfiltered hybrid search spends a second neighbour scan, and
/// that scan is the non-source complement of the source scan. The two
/// keep the query's other filters.
#[test]
fn unfiltered_hybrid_splits_the_semantic_scan() {
    let shared = CodeChunkVectorFilters {
        repo_id: Some(id(9)),
        language: Some("rust"),
        chunk_type: Some("function"),
        file_class: None,
        exclude_source: false,
    };
    match semantic_scan(&resolved()) {
        SemanticScan::Split { source, non_source } => {
            assert_eq!(
                source,
                CodeChunkVectorFilters {
                    file_class: Some("source"),
                    ..shared
                }
            );
            assert_eq!(
                non_source,
                CodeChunkVectorFilters {
                    exclude_source: true,
                    ..shared
                }
            );
        }
        SemanticScan::One(_) => panic!("unfiltered hybrid must scan source and the rest"),
    }

    let classed = ResolvedChunkQuery {
        file_class: Some(FileClass::Lockfile),
        ..resolved()
    };
    match semantic_scan(&classed) {
        SemanticScan::One(filters) => {
            assert_eq!(filters.file_class, Some("lockfile"));
            assert!(!filters.exclude_source);
        }
        SemanticScan::Split { .. } => panic!("a named class is one scan"),
    }

    let semantic_mode = ResolvedChunkQuery {
        requested_mode: ChunkSearchMode::Semantic,
        ..resolved()
    };
    assert!(
        matches!(semantic_scan(&semantic_mode), SemanticScan::One(filters) if filters.file_class.is_none() && !filters.exclude_source),
        "semantic mode keeps one unfiltered scan"
    );
}

#[test]
fn a_semantic_weight_is_refused_out_of_range_or_off_hybrid() {
    for bad in [-0.1, 1.1, f32::NAN] {
        let err = requested_semantic_weight(Some(bad), ChunkSearchMode::Hybrid)
            .expect_err("out of range");
        assert!(err.to_string().contains("within 0.0..=1.0"), "{err}");
    }
    for mode in [ChunkSearchMode::Lexical, ChunkSearchMode::Semantic] {
        let err = requested_semantic_weight(Some(0.5), mode).expect_err("not hybrid");
        assert!(err.to_string().contains("only to mode=hybrid"), "{err}");
    }
    assert_eq!(
        requested_semantic_weight(None, ChunkSearchMode::Semantic).expect("omitted"),
        None
    );
    assert_eq!(
        requested_semantic_weight(Some(1.0), ChunkSearchMode::Hybrid).expect("in range"),
        Some(weight(1.0))
    );
}

/// The one field that must *not* reach the canon: `exact_pattern` is
/// derived from `query`, so it carries no independent binding, and
/// `effective_mode` is not on the type at all (see the type's docs).
#[test]
fn derived_exact_pattern_is_not_fingerprinted() {
    let base = resolved();
    let rewritten = ResolvedChunkQuery {
        exact_pattern: "%something else%".to_string(),
        ..resolved()
    };
    assert_eq!(base.fingerprint(), rewritten.fingerprint());
}

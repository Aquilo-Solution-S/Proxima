use std::collections::{HashMap, HashSet};

use proxima_core::verbs::query::{EdgeFilter, EdgeReadRequest, EdgeTargetProjection};
use proxima_core::{EdgeId, EntityRef, MemoryId};
use proxima_core::{Tool, ToolCtx, ToolError};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::payloads::{CodeChunkV1, FileState};

use super::CodeToolCtxExt;
use super::code_store;
use super::sql::{map_storage, resolve_repo_identifier};

/// How a chunk search ranks.
///
/// Mirrors `core_search_memories`' `mode` argument, including its behaviour
/// when no embedding client is configured: `hybrid` degrades to `lexical`
/// and says so in `degraded_to_lexical`, `semantic` fails rather than
/// silently answering a different question, `lexical` never needed one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ChunkSearchMode {
    /// Full-text bands plus the exact path/substring arms. Never needs an
    /// embedding client.
    Lexical,
    /// Nearest neighbours of the query embedding only.
    Semantic,
    /// Both, fused by reciprocal rank. The default.
    #[default]
    Hybrid,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CodeSearchChunksArgs {
    #[schemars(
        description = "Query string for code chunk search, matched against file paths and chunk text. Takes an identifier or path for exact lookup, or a plain-English question — chunks sharing any content word are returned when none share all of them. 1 to 512 chars."
    )]
    pub query: String,
    #[serde(default)]
    #[schemars(
        description = "Ranking mode: `hybrid` (default) fuses full-text and embedding similarity, `lexical` is full-text only, `semantic` is embedding-only. Without a configured embedding model `hybrid` falls back to lexical and reports degraded_to_lexical=true, and `semantic` is rejected."
    )]
    pub mode: ChunkSearchMode,
    #[schemars(
        description = "Optional maximum number of chunk matches. Omit or null for 12; values above 50 are clamped, and 0 is rejected."
    )]
    pub limit: Option<u32>,
    #[schemars(
        description = "Optional repo handle filter, typically `R...`. Omit or null to search all visible repos."
    )]
    pub repo_handle: Option<String>,
    #[schemars(
        description = "Optional language filter, for example `rust` or `typescript`. Omit or null for all languages."
    )]
    pub language: Option<String>,
    #[schemars(description = "Optional chunk type filter. Omit or null for all chunk types.")]
    pub chunk_type: Option<String>,
    #[serde(default = "default_include_calls")]
    #[schemars(
        description = "Whether to include neighboring proxima-code/calls edges. Defaults to true."
    )]
    pub include_calls: bool,
    #[serde(default)]
    #[schemars(
        description = "Maximum characters of chunk text per match. Omit or null for 2000; values above 8000 are clamped, and 0 is rejected. A match whose text was cut carries snippet_truncated=true — read the whole chunk with proxima-code_open_file_revision."
    )]
    pub snippet_max_chars: Option<usize>,
}

const fn default_include_calls() -> bool {
    true
}

/// Relation whose edges are returned as `calls_edges` neighbours.
const CALLS_RELATION: &str = "proxima-code/calls";

/// Neighbour edges returned across the whole result set.
///
/// Applied in request order — chunk by chunk, in search-rank order — so the
/// edges that survive belong to the best-ranked matches and the same search
/// answers the same way twice.
const MAX_CALL_EDGES: usize = 200;

/// Neighbour edges read per chunk per direction before the global cut.
const CALL_EDGES_PER_CHUNK: u32 = 200;

/// Characters of chunk text returned per match when the caller says nothing.
///
/// This was 480, and a chunk is much larger than that: measured over this
/// repository's index, the median chunk is 1,628 characters and only 15.3% of
/// chunks fit in 480 — so a search returned the right chunk with roughly its
/// first quarter visible, and an agent had to spend a second
/// `proxima-code_open_file_revision` call per result to see what it had
/// found. A local model asked for a constant's value routinely answered with
/// the constant's *name*, because the name was inside the first 480
/// characters and the value was not.
///
/// 2000 covers the median chunk whole and most of the p90 (2,675).
const DEFAULT_SNIPPET_MAX_CHARS: usize = 2_000;

/// Ceiling on `snippet_max_chars`, matching `core_search_memories`'
/// `body_max_chars`. Covers the p99 chunk (4,488 characters).
const MAX_SNIPPET_MAX_CHARS: usize = 8_000;

/// Most structured identifiers lifted out of one query. Bounds the size of
/// the derived tsquery; a query naming more than this many distinct
/// identifiers is already well served by the first twelve.
const MAX_DISTINCTIVE_TERMS: usize = 12;

/// Reciprocal-rank-fusion damping constant, at its conventional value.
///
/// Hybrid ranking fuses *ranks*, not scores. The two arms are not on one
/// scale and cannot be put on one: a lexical band score is an unbounded sum
/// of a tier, a `ts_rank`, and up to three substring bonuses, while cosine
/// similarity is bounded in `[0, 1]` and — for an embedding model over
/// source code — occupies a narrow, corpus-dependent slice of it. Any
/// weighted sum of the two needs a normalisation constant fitted to a
/// corpus, and would be silently wrong on the next one. Rank fusion needs
/// none: `1 / (k + rank)` per arm, summed.
///
/// `k = 60` is the value from the paper that introduced the method
/// (Cormack, Clarke & Buettcher, SIGIR 2009). Larger `k` flattens the curve
/// so deep ranks matter more; smaller `k` sharpens the head.
const RRF_K: f32 = 60.0;

/// What a caller is told when they ask for `semantic` and the deployment
/// has no embedding model. Mirrors `core_search_memories`: pure semantic
/// has no lexical component to fall back on, so answering lexically would
/// be answering a different question than the one asked.
const SEMANTIC_CHUNK_SEARCH_UNAVAILABLE: &str = "semantic chunk search unavailable: no embedding model is configured. \
     Use mode=lexical, or mode=hybrid to rank lexically when embeddings are absent.";

/// Shortest token worth treating as an identifier. `id` and `fs` carry no
/// selectivity against a code corpus.
const MIN_DISTINCTIVE_TERM_LEN: usize = 3;

/// The structured identifiers in `query`, space-joined, empty when there are
/// none.
///
/// A token is structured when it carries internal capitalisation, a digit, or
/// an underscore — the shape of `getModuleScriptSources`, `MAX_CHUNK_CHARS`,
/// `utf8`. Ordinary prose never has it, so a plain-English question yields an
/// empty string, the rare bands below stay switched off, and such queries rank
/// exactly as they did before this function existed.
///
/// Shape, deliberately, and not corpus rarity. Rarity was measured on the knip
/// corpus — keep only terms whose document frequency is under a threshold,
/// computed exactly — and ranked *worse* than this rule (MRR 0.086 against
/// 0.142). The rare tokens in a bug report are version numbers and stack-trace
/// noise; the diagnostic ones are the identifiers, and plenty of those are
/// common. Shape selects for "is an identifier", which is the question a code
/// search is actually asking.
fn distinctive_terms(query: &str) -> String {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<&str> = Vec::new();
    for token in query.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
        if token.len() < MIN_DISTINCTIVE_TERM_LEN
            || !token.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        {
            continue;
        }
        let structured = token
            .chars()
            .skip(1)
            .any(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
        if !structured {
            continue;
        }
        if seen.insert(token.to_ascii_lowercase()) {
            out.push(token);
            if out.len() == MAX_DISTINCTIVE_TERMS {
                break;
            }
        }
    }
    out.join(" ")
}

/// Resolve the requested snippet budget against the ceiling.
fn effective_snippet_max_chars(requested: Option<usize>) -> usize {
    requested.map_or(DEFAULT_SNIPPET_MAX_CHARS, |max| {
        max.min(MAX_SNIPPET_MAX_CHARS)
    })
}

#[derive(Debug, Serialize)]
pub struct CodeSearchChunksOutput {
    pub matches: Vec<ChunkMatch>,
    pub calls_edges: Vec<CallEdge>,
    /// At least one further eligible match exists past `limit` in the
    /// scanned candidate window; retry with a higher limit (max 50) or
    /// narrow the query. Truncation is a signal, never silent.
    pub has_more: bool,
    /// The mode that was requested, echoed so a caller reading a stored
    /// response knows what produced it.
    pub mode: String,
    /// A `hybrid` search ranked lexically only — no embedding client is
    /// configured, the provider call failed, or nothing in the searched
    /// scope is embedded yet. The results are still real full-text
    /// matches; they simply had no semantic component. Always `false` for
    /// `lexical` (which asked for nothing else) and for `semantic` (which
    /// fails instead of degrading).
    pub degraded_to_lexical: bool,
}

#[derive(Debug, Serialize)]
pub struct ChunkMatch {
    pub handle: String,
    pub repo_handle: String,
    pub file_path: String,
    pub chunk_index: i32,
    pub language: Option<String>,
    pub chunk_type: String,
    pub line_range: (i64, i64),
    pub byte_range: (i64, i64),
    pub snippet: String,
    /// `true` when `snippet` is shorter than the chunk it came from. Read the
    /// whole chunk with `proxima-code_open_file_revision`, or re-run with a
    /// larger `snippet_max_chars`.
    pub snippet_truncated: bool,
    pub match_kind: String,
    pub matched_line: Option<i64>,
    pub matched_excerpt: Option<String>,
    /// The score this match was ranked by, in the units of the mode that
    /// ran: a lexical band score for `lexical`, cosine similarity for
    /// `semantic`, a fused rank score for `hybrid`. Comparable within one
    /// response, not across modes.
    pub score: f32,
    /// The lexical band score, `0.0` when only the semantic arm found this
    /// chunk. Reported alongside `score` for the same reason
    /// `core_search_memories` reports it: a fused number alone cannot tell
    /// a caller which arm earned the hit.
    pub lexical_score: f32,
    /// Cosine similarity to the query embedding in `[0, 1]`, `0.0` when the
    /// semantic arm did not run or did not reach this chunk.
    pub similarity_score: f32,
}

#[derive(Debug, Serialize)]
pub struct CallEdge {
    pub edge_handle: String,
    pub source: Option<String>,
    pub target: Option<String>,
    pub callee_name: String,
    pub is_dynamic: bool,
}

#[derive(Debug)]
pub struct CodeSearchChunksTool;

impl Tool for CodeSearchChunksTool {
    const NAME: &'static str = "proxima-code_search_chunks";
    const DESCRIPTION: &'static str = "Search head code chunks by exact substring, path, or full-text content, including plain-English questions. Ranks by mode: hybrid (default) fuses full-text with embedding similarity, lexical is full-text only, semantic is embedding-only; a hybrid search with no embeddings available answers lexically and reports degraded_to_lexical. Each match carries its chunk text up to snippet_max_chars, flagged snippet_truncated when cut. Supports language/chunk_type filters and optional proxima-code/calls neighbor edges.";

    type Args = CodeSearchChunksArgs;
    type Output = CodeSearchChunksOutput;

    #[allow(clippy::too_many_lines)]
    fn call(
        ctx: ToolCtx,
        args: CodeSearchChunksArgs,
    ) -> futures::future::BoxFuture<'static, Result<CodeSearchChunksOutput, ToolError>> {
        Box::pin(async move {
            let query = args.query.trim();
            if query.is_empty() || query.chars().count() > 512 {
                return Err(ToolError::InvalidInput(
                    "query must be 1..=512 chars".into(),
                ));
            }
            if args.snippet_max_chars == Some(0) {
                return Err(ToolError::InvalidInput(
                    "snippet_max_chars must be at least 1".into(),
                ));
            }
            // Rejected rather than silently answered: a zero limit returns an
            // empty `matches` array indistinguishable from "nothing matched",
            // and `snippet_max_chars: 0` is already rejected for the same
            // reason.
            if args.limit == Some(0) {
                return Err(ToolError::InvalidInput("limit must be at least 1".into()));
            }
            let snippet_max_chars = effective_snippet_max_chars(args.snippet_max_chars);
            let limit = args.limit.unwrap_or(12).min(50);
            let exact_pattern = like_pattern(query);
            let repo_id = match args.repo_handle.as_deref() {
                Some(handle) => Some(resolve_repo_identifier(&ctx, handle).await?),
                None => None,
            };
            let pool = code_store(&ctx)?;
            let engine = super::engine(&ctx)?;
            // Resolved before either arm runs, because the answer decides
            // which arms run at all: a `semantic` request with no embedding
            // model is an error, not an empty result set, and a `hybrid` one
            // becomes a `lexical` one that reports having done so.
            let (effective_mode, query_embedding) =
                resolve_query_embedding(&engine, args.mode, query).await?;

            // Sidecar-only candidate scan (no owner filter, no core-table
            // supersession dedup): full-text/path rank over every `Present`
            // chunk row matching the search predicates, across any owner,
            // any historical revision. `authorized_abstraction_head_candidates`
            // below narrows this to the owner-or-World head per
            // (repo_id, file_path, chunk_index) via a `source_batch_id`
            // recency comparison (`code-chunk-v1` never sets
            // `memories.supersedes` — each derived chunk ties 1:1 to its
            // exact source file revision rather than declaring a successor),
            // so the raw candidate window is widened well past `limit` to
            // leave headroom for historical duplicates collapsing away.
            let candidate_limit = i64::from(limit.saturating_mul(20).max(limit).min(1_000));
            // `c.search_tsv` is the STORED generated column added by the v0.0.7
            // flavor migration, holding exactly
            // `proxima_core.lexical_tsv(proxima_core.lexical_join(file_path,
            // text))`. Reading it replaces two per-row `to_tsvector`
            // evaluations — one for the predicate, one for the rank — with a
            // column read, and the GIN index sits on the column rather than on
            // an expression. `code_chunk_search_tsv_matches_the_projection`
            // pins the column against that definition.
            //
            // Four disjoint score bands, so match *tier* dominates rank:
            //
            //   strict    [4.0, 4.9]   every content lexeme co-occurs
            //   rare-all  [3.0, 3.9]   every structured identifier co-occurs
            //   rare-any  [2.0, 2.9]   some structured identifier matched
            //   rescue    [1.0, 1.9]   any content lexeme matched
            //   neither    0.0         reached only via the substring arms
            //
            // In-band rank contributes at most 0.6 and the parsed-chunk
            // preference 0.3, so 0.9 < the 1.0 band gap: no amount of
            // in-band rank lets a weaker tier overtake a stronger one.
            //
            // The two rare bands exist because `ts_rank` has no IDF. Against
            // the knip corpus a Markdown chunk matching twenty ordinary
            // English words outranks the one TypeScript chunk holding the
            // identifier the report actually names, and the substring arms
            // below cannot help: they match `%<the whole query>%`, which never
            // fires for a sentence. Ranking a bug report's identifiers on
            // their own recovers that. `distinctive_terms` returns empty for
            // a query with no identifiers, leaving both bands NULL and prose
            // questions ranked exactly as before.
            //
            // The text-search configuration comes from
            // `proxima_core.lexical_config()`, not a literal. `code_chunk_v1`'s
            // `search_tsv` is generated by `proxima_core.lexical_tsv`, which
            // reads that same function, so naming the configuration here would
            // mean two places had to be changed together — and a deployment
            // that switched only one would get a tsquery in one configuration
            // matched against vectors in another, which does not match at all.
            // It is on the guardrail's pure-core-SQL allowlist for exactly this
            // reason: restating the definition is the drift it prevents.
            //
            // The rescue arm is what makes a question answerable at all.
            // `websearch_to_tsquery` is AND-semantics, so a query phrased as a
            // sentence needs every one of its content words in one chunk;
            // measured against this repository's own index, 0 of 24
            // natural-language queries returned a single row without this arm.
            // `plainto_tsquery` emits only '&' between lexemes (no phrase or
            // negation operators), so rewriting them to '|' is safe.
            //
            // `ts_rank_cd` is normalised with flag 32 (divide by itself + 1),
            // not scaled by a bare multiplier. An unlabelled document — every
            // document here, since nothing assigns A/B/C/D weights — has
            // cover density of at least 0.1, so the old
            // `LEAST(ts_rank_cd(...) * 10.0, 1.0)` was 1.0 for *every* row
            // that matched at all: measured on the knip corpus, 3,170 of
            // 3,170. The term contributed nothing and the strict band was a
            // flat tie. Flag 32 keeps it inside [0, 1) and it varies (0.091
            // to 0.888 over the same rows), so `LEAST` is now just a
            // guard rather than the whole story.
            //
            // Rescue ranks with `ts_rank(..., 1|32)`, not `ts_rank_cd`.
            // Cover density rewards a short span containing several query
            // terms, which repetitive DDL wins trivially: for "how does the
            // code chunker decide how big a chunk should be", `ts_rank_cd`
            // returned four chunks of a `*.sql` migration above
            // `flavors/code/src/chunker.rs`, while `ts_rank` with
            // length normalisation (flag 1) put chunker.rs first and second.
            //
            // The path/text substring bonuses sit on top of the bands, and the
            // largest of them outweighs a whole tier. That is deliberate: it is
            // where exact identifier and keyword lookup lives now that the
            // vector stems and drops stopwords. `embed_in_chunks` matches the
            // substring arm verbatim regardless of how it tokenises.
            // A pure `semantic` search runs no lexical branch at all, the
            // same way storage gates `core_search_memories`' lexical query
            // to `Lexical`/`Hybrid`.
            let lexical_rows: Vec<ChunkCandidateRow> = if effective_mode
                == ChunkSearchMode::Semantic
            {
                Vec::new()
            } else {
                sqlx::query_as(
                // 'english' literal, not lexical_config(): chunk vectors are
                // pinned english per row (migration 20260728000020 — code is
                // not prose in the deployment's language), so the query side
                // pins the same constant. Following the database default
                // here would stem the query german against english vectors
                // the moment a deployment switches its documents.
                "WITH q AS (
                     SELECT websearch_to_tsquery('english'::regconfig,
                                proxima_core.lexical_scrub($1)) AS tsq,
                            NULLIF(
                                replace(
                                    plainto_tsquery('english'::regconfig,
                                        proxima_core.lexical_scrub($1))::text,
                                    ' & ', ' | '),
                                '')::tsquery AS any_tsq,
                            websearch_to_tsquery('english'::regconfig,
                                proxima_core.lexical_scrub(NULLIF($7, ''))) AS rare_all_tsq,
                            NULLIF(
                                replace(
                                    plainto_tsquery('english'::regconfig,
                                        proxima_core.lexical_scrub(NULLIF($7, '')))::text,
                                    ' & ', ' | '),
                                '')::tsquery AS rare_any_tsq
                 )
                 SELECT c.memory_id,
                        (
                            GREATEST(
                                CASE WHEN c.search_tsv @@ q.tsq
                                     THEN 4.0 + LEAST(ts_rank_cd(c.search_tsv, q.tsq, 32), 1.0) * 0.6
                                     ELSE 0.0 END,
                                CASE WHEN q.rare_all_tsq IS NOT NULL AND c.search_tsv @@ q.rare_all_tsq
                                     THEN 3.0 + LEAST(ts_rank(c.search_tsv, q.rare_all_tsq, 1|32) * 100.0, 1.0) * 0.6
                                     ELSE 0.0 END,
                                CASE WHEN q.rare_any_tsq IS NOT NULL AND c.search_tsv @@ q.rare_any_tsq
                                     THEN 2.0 + LEAST(ts_rank(c.search_tsv, q.rare_any_tsq, 1|32) * 100.0, 1.0) * 0.6
                                     ELSE 0.0 END,
                                CASE WHEN q.any_tsq IS NOT NULL AND c.search_tsv @@ q.any_tsq
                                     THEN 1.0 + LEAST(ts_rank(c.search_tsv, q.any_tsq, 1|32) * 100.0, 1.0) * 0.6
                                     ELSE 0.0 END
                            )
                            + CASE WHEN c.chunk_type <> 'file' THEN 0.3 ELSE 0.0 END
                            + CASE WHEN lower(c.file_path) = lower($1) THEN 10.0 ELSE 0.0 END
                            + CASE WHEN lower(c.file_path) LIKE $4 ESCAPE '\\' THEN 6.0 ELSE 0.0 END
                            + CASE WHEN lower(c.text) LIKE $4 ESCAPE '\\' THEN 4.0 ELSE 0.0 END
                        )::real AS score,
                        -- The three literal arms again, on their own. Hybrid
                        -- ranking fuses *ranks*, which on its own would let a
                        -- strong embedding neighbour outrank an exact path
                        -- match — the one case where the caller has told us
                        -- precisely what they want. Reporting the literal
                        -- bonus separately lets the fusion keep those hits as
                        -- an absolute prefix. Deliberately duplicated rather
                        -- than factored out of `score` above: `score` is the
                        -- shipped lexical ordering, pinned by the retrieval
                        -- gate, and re-associating its additions could move a
                        -- result by a float's last bit.
                        (
                            CASE WHEN lower(c.file_path) = lower($1) THEN 10.0 ELSE 0.0 END
                            + CASE WHEN lower(c.file_path) LIKE $4 ESCAPE '\\' THEN 6.0 ELSE 0.0 END
                            + CASE WHEN lower(c.text) LIKE $4 ESCAPE '\\' THEN 4.0 ELSE 0.0 END
                        )::real AS literal_bonus
                   FROM proxima_code.code_chunk_v1 c, q
                  WHERE c.state = 'Present'
                    AND ($2::uuid IS NULL OR c.repo_id = $2)
                    AND ($3::text IS NULL OR c.language = $3)
                    AND ($5::text IS NULL OR c.chunk_type = $5)
                    AND (
                        c.search_tsv @@ q.tsq
                        OR (q.any_tsq IS NOT NULL AND c.search_tsv @@ q.any_tsq)
                        OR (q.rare_any_tsq IS NOT NULL AND c.search_tsv @@ q.rare_any_tsq)
                        OR lower(c.file_path) LIKE $4 ESCAPE '\\'
                        OR lower(c.text) LIKE $4 ESCAPE '\\'
                    )
                  ORDER BY score DESC, c.memory_id DESC
                  LIMIT $6",
            )
            .bind(query)
            .bind(repo_id)
            .bind(args.language.as_deref())
            .bind(exact_pattern)
            .bind(args.chunk_type.as_deref())
            .bind(candidate_limit)
            .bind(distinctive_terms(query))
            .fetch_all(pool.pool())
            .await
            .map_err(map_storage)?
            };

            // The semantic arm draws on the same candidate budget as the
            // lexical one and applies the same structural filters — pushed
            // into the neighbour scan rather than applied to its output,
            // because a search scoped to one repository would otherwise spend
            // its whole budget on whichever repository is largest and come
            // back empty.
            let semantic_rows = match query_embedding.as_ref() {
                Some((embedding, model_id)) => {
                    pool.nearest_code_chunk_candidates(
                        ctx.owner(),
                        model_id,
                        embedding,
                        proxima::flavor::CodeChunkVectorFilters {
                            repo_id,
                            language: args.language.as_deref(),
                            chunk_type: args.chunk_type.as_deref(),
                        },
                        usize::try_from(candidate_limit).unwrap_or(0),
                    )
                    .await?
                }
                None => Vec::new(),
            };

            let fused = fuse_candidates(effective_mode, &lexical_rows, &semantic_rows);
            let candidate_ids = fused
                .iter()
                .map(|scores| scores.memory_id)
                .collect::<Vec<_>>();
            let score_by_id = fused
                .into_iter()
                .map(|scores| (scores.memory_id, scores))
                .collect::<HashMap<_, _>>();
            let head_id_set = pool
                .authorized_code_chunk_head_candidates(ctx.owner(), &candidate_ids)
                .await?
                .into_iter()
                .collect::<HashSet<_>>();
            // Preserve the score-descending order from the candidate scan;
            // the head-candidate narrowing above returns an unordered set.
            let head_ids = candidate_ids
                .iter()
                .copied()
                .filter(|id| head_id_set.contains(id))
                .collect::<Vec<_>>();
            let rows = pool
                .authorized_abstraction_payloads::<CodeChunkV1>(
                    &engine,
                    ctx.authz(),
                    ctx.owner(),
                    &head_ids,
                    head_ids.len(),
                )
                .await?;

            let mut matches = Vec::new();
            let mut has_more = false;
            let mut chunk_ids = Vec::with_capacity(rows.len());
            let mut seen_keys = HashSet::new();
            for (memory_id, payload) in rows {
                let key = (
                    payload.repo_id,
                    payload.file_path.clone(),
                    payload.chunk_index,
                );
                if !seen_keys.insert(key) {
                    continue;
                }
                if payload.state != FileState::Present {
                    continue;
                }
                if u32::try_from(matches.len()).unwrap_or(u32::MAX) >= limit {
                    // One more eligible match past the page proves
                    // truncation without emitting the row.
                    has_more = true;
                    break;
                }
                let raw_id = memory_id.into_inner();
                chunk_ids.push(raw_id);
                let scores = score_by_id.get(&raw_id).copied().unwrap_or_default();
                let (match_kind, matched_line, matched_excerpt) = match_metadata(
                    query,
                    &payload.file_path,
                    &payload.text,
                    payload.line_range_start,
                );
                matches.push(ChunkMatch {
                    handle: ctx.format_abstraction_memory(memory_id),
                    repo_handle: ctx.format_flavor_object(
                        super::REPO_HANDLE_KIND,
                        payload.repo_id,
                        super::REPO_HANDLE_PREFIX,
                    ),
                    file_path: payload.file_path,
                    chunk_index: i32::try_from(payload.chunk_index)
                        .map_err(|_| ToolError::Other("chunk_index exceeds i32".into()))?,
                    language: payload.language,
                    chunk_type: payload.chunk_type,
                    line_range: (
                        i64::from(payload.line_range_start),
                        i64::from(payload.line_range_end),
                    ),
                    byte_range: (
                        i64::from(payload.byte_range_start),
                        i64::from(payload.byte_range_end),
                    ),
                    snippet: payload.text.chars().take(snippet_max_chars).collect(),
                    snippet_truncated: payload.text.chars().count() > snippet_max_chars,
                    match_kind,
                    matched_line,
                    matched_excerpt,
                    score: scores.score,
                    lexical_score: scores.lexical_score,
                    similarity_score: scores.similarity_score,
                });
            }

            let calls_edges = if args.include_calls && !chunk_ids.is_empty() {
                load_call_edges(&ctx, &chunk_ids).await?
            } else {
                Vec::new()
            };

            Ok(CodeSearchChunksOutput {
                degraded_to_lexical: degraded_to_lexical(args.mode, effective_mode, &matches),
                mode: mode_label(args.mode).to_string(),
                matches,
                calls_edges,
                has_more,
            })
        })
    }
}

/// The per-chunk scores a search ranked by, in one place so the ordering
/// and the reported numbers cannot drift apart.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct MatchScores {
    memory_id: uuid::Uuid,
    score: f32,
    lexical_score: f32,
    similarity_score: f32,
}

/// Resolve the requested mode against what the deployment can actually do,
/// returning the mode that will run and the query embedding if one is
/// needed.
///
/// The three outcomes deliberately match `core_search_memories`, because a
/// caller should not have to learn two rules: `lexical` never asks for an
/// embedding; `hybrid` degrades to `lexical` and reports it; `semantic`
/// fails, because it has no other arm to fall back on and answering
/// lexically would answer a question the caller did not ask.
async fn resolve_query_embedding(
    engine: &proxima_core::Engine,
    mode: ChunkSearchMode,
    query: &str,
) -> Result<(ChunkSearchMode, Option<(Vec<f32>, String)>), ToolError> {
    if mode == ChunkSearchMode::Lexical {
        return Ok((ChunkSearchMode::Lexical, None));
    }
    let Some(embed) = engine.embed_client() else {
        if mode == ChunkSearchMode::Semantic {
            return Err(ToolError::Other(
                SEMANTIC_CHUNK_SEARCH_UNAVAILABLE.to_string(),
            ));
        }
        return Ok((ChunkSearchMode::Lexical, None));
    };
    // The client can vanish, or its call fail, between the probe above and
    // here; both land in the same place.
    match embed.embed(query).await {
        Ok(embedding) => Ok((mode, Some((embedding, embed.model_id().to_string())))),
        Err(err) if mode == ChunkSearchMode::Semantic => Err(ToolError::Other(format!(
            "semantic chunk search unavailable: embedding provider error: {err}"
        ))),
        Err(err) => {
            // `degraded_to_lexical` tells the caller *that* this happened;
            // only the log can tell an operator *why*.
            tracing::warn!(
                error = %err,
                "hybrid chunk search query embedding unavailable; degrading to lexical",
            );
            Ok((ChunkSearchMode::Lexical, None))
        }
    }
}

/// `1 / (k + rank)` for a zero-based rank.
fn reciprocal_rank(rank: usize) -> f32 {
    // The candidate budget is capped at 1,000, so a rank never approaches
    // `u16::MAX` and the conversion is exact rather than merely saturating.
    let position = u16::try_from(rank).unwrap_or(u16::MAX);
    1.0 / (RRF_K + f32::from(position) + 1.0)
}

/// Merge the two arms into one ranked candidate list.
///
/// `Lexical` and `Semantic` each pass their own arm through untouched, so a
/// lexical search ranks exactly as it did before a semantic arm existed —
/// including the tiebreak, which reproduces the candidate scan's
/// `ORDER BY score DESC, memory_id DESC`.
///
/// `Hybrid` sums each arm's reciprocal rank and adds the literal bonus on
/// top. The bonus is 4.0 at the smallest and a reciprocal rank is at most
/// `1/61`, so a chunk whose path or text literally contains the query
/// outranks every chunk that merely resembles it, however strong the
/// resemblance. That is the one place where a caller has said exactly what
/// they want, and rank fusion on its own would let a confident embedding
/// neighbour bury it.
fn fuse_candidates(
    mode: ChunkSearchMode,
    lexical: &[ChunkCandidateRow],
    semantic: &[proxima::flavor::CodeChunkVectorCandidate],
) -> Vec<MatchScores> {
    let mut by_id: HashMap<uuid::Uuid, MatchScores> =
        HashMap::with_capacity(lexical.len() + semantic.len());
    for (rank, row) in lexical.iter().enumerate() {
        let entry = by_id.entry(row.memory_id).or_insert(MatchScores {
            memory_id: row.memory_id,
            ..MatchScores::default()
        });
        entry.lexical_score = row.score;
        entry.score = if mode == ChunkSearchMode::Hybrid {
            entry.score + row.literal_bonus + reciprocal_rank(rank)
        } else {
            row.score
        };
    }
    for (rank, row) in semantic.iter().enumerate() {
        let entry = by_id.entry(row.memory_id).or_insert(MatchScores {
            memory_id: row.memory_id,
            ..MatchScores::default()
        });
        entry.similarity_score = row.similarity_score;
        entry.score = if mode == ChunkSearchMode::Hybrid {
            entry.score + reciprocal_rank(rank)
        } else {
            row.similarity_score
        };
    }

    let mut fused = by_id.into_values().collect::<Vec<_>>();
    // Deterministic despite the `HashMap`: `memory_id` is unique across the
    // merged set, so score-then-id is a total order and the randomised
    // iteration above cannot survive the sort.
    fused.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.memory_id.cmp(&a.memory_id))
    });
    fused
}

/// Whether a `hybrid` search ended up ranked lexically only.
///
/// Two ways in: no embedding was available at all, or the semantic arm ran
/// and reached none of the returned chunks — the symptom of a scope where
/// nothing is embedded yet. An empty result set is neither; it is a genuine
/// no-match, and reporting degradation for it would cry wolf on every
/// query that simply found nothing.
fn degraded_to_lexical(
    requested: ChunkSearchMode,
    effective: ChunkSearchMode,
    matches: &[ChunkMatch],
) -> bool {
    if requested != ChunkSearchMode::Hybrid {
        return false;
    }
    effective == ChunkSearchMode::Lexical
        || (!matches.is_empty() && matches.iter().all(|m| m.similarity_score <= 0.0))
}

const fn mode_label(mode: ChunkSearchMode) -> &'static str {
    match mode {
        ChunkSearchMode::Lexical => "lexical",
        ChunkSearchMode::Semantic => "semantic",
        ChunkSearchMode::Hybrid => "hybrid",
    }
}

fn match_metadata(
    query: &str,
    file_path: &str,
    text: &str,
    line_range_start: u32,
) -> (String, Option<i64>, Option<String>) {
    let query_lower = query.to_ascii_lowercase();
    let path_lower = file_path.to_ascii_lowercase();
    if path_lower == query_lower {
        return ("path_exact".to_string(), None, Some(file_path.to_string()));
    }
    if path_lower.contains(&query_lower) {
        return (
            "path_contains".to_string(),
            None,
            Some(file_path.to_string()),
        );
    }

    for (idx, line) in text.lines().enumerate() {
        if line.to_ascii_lowercase().contains(&query_lower) {
            return (
                "text_contains".to_string(),
                i64::try_from(idx)
                    .ok()
                    .map(|offset| i64::from(line_range_start) + offset),
                Some(line.trim().chars().take(480).collect()),
            );
        }
    }

    ("full_text".to_string(), None, None)
}

async fn load_call_edges(
    ctx: &ToolCtx,
    chunk_ids: &[uuid::Uuid],
) -> Result<Vec<CallEdge>, ToolError> {
    let engine = super::engine(ctx)?;
    let pool = code_store(ctx)?;

    // One `read_edges` per (chunk, direction). They are independent reads
    // against disjoint filters, so they run concurrently rather than in a
    // sequential loop: at the default page size that is 24 round-trips, and
    // serialising them cost roughly half the tool's latency (measured on
    // this repository's index: 105ms with neighbours, 57ms without).
    // `include_calls` defaults to true, so every default search paid it.
    let requests = chunk_ids.iter().flat_map(|chunk_id| {
        let entity = EntityRef::Memory(MemoryId::new(*chunk_id));
        [
            EdgeFilter {
                relation: Some(CALLS_RELATION.to_string()),
                source: Some(entity),
                target: None,
            },
            EdgeFilter {
                relation: Some(CALLS_RELATION.to_string()),
                source: None,
                target: Some(entity),
            },
        ]
    });
    let responses = futures::future::try_join_all(requests.map(|filter| {
        let engine = engine.clone();
        async move {
            engine
                .read_edges(
                    ctx.authz(),
                    &EdgeReadRequest {
                        owner: ctx.owner(),
                        edge_ids: Vec::new(),
                        filter,
                        limit: CALL_EDGES_PER_CHUNK,
                        cursor: None,
                        include_payloads: false,
                    },
                )
                .await
        }
    }))
    .await?;

    // Request order, not hash order. `responses` comes back in the order the
    // filters were built — chunk by chunk, in search-rank order — and keeping
    // that order is what makes the `MAX_CALL_EDGES` cut below deterministic
    // and useful: the same search returns the same edges, and what survives
    // truncation belongs to the highest-ranked chunks. Collecting into a
    // `HashMap` and taking 200 of its values returned an arbitrary subset
    // that varied from run to run.
    let mut seen: HashSet<uuid::Uuid> = HashSet::new();
    let mut edges = Vec::new();
    for response in responses {
        for edge in response.edges {
            if seen.insert(edge.id) {
                edges.push(edge);
            }
        }
    }
    edges.truncate(MAX_CALL_EDGES);

    let edge_ids = edges.iter().map(|edge| edge.id).collect::<Vec<_>>();
    let payload_rows: Vec<CallPayloadRow> = sqlx::query_as(
        "SELECT edge_id, callee_name, is_dynamic
           FROM proxima_code.code_calls_v1
          WHERE edge_id = ANY($1::uuid[])",
    )
    .bind(&edge_ids)
    .fetch_all(pool.pool())
    .await
    .map_err(map_storage)?;
    let payloads = payload_rows
        .into_iter()
        .map(|row| (row.edge_id, row))
        .collect::<HashMap<_, _>>();

    let mut out = Vec::new();
    for edge in edges {
        let Some(payload) = payloads.get(&edge.id) else {
            continue;
        };
        out.push(CallEdge {
            edge_handle: ctx.format_edge(EdgeId::new(edge.id)),
            source: match edge.source {
                EntityRef::Memory(id) => Some(ctx.format_abstraction_memory(id)),
                EntityRef::Goal(_) | EntityRef::FactEntity(_) => None,
            },
            target: match edge.target {
                EdgeTargetProjection::Visible {
                    target: EntityRef::Memory(id),
                } => Some(ctx.format_abstraction_memory(id)),
                EdgeTargetProjection::Visible { .. }
                | EdgeTargetProjection::Redacted
                | EdgeTargetProjection::Unavailable => None,
            },
            callee_name: payload.callee_name.clone(),
            is_dynamic: payload.is_dynamic,
        });
    }
    Ok(out)
}

/// `%`-wrapped, escaped, lowercased pattern for the substring arms.
///
/// Lowercased the same way the SQL side is. The comparison is against
/// `lower(c.file_path)` / `lower(c.text)`, and Postgres `lower()` folds
/// non-ASCII: it maps `MÜNCHEN.RS` to `münchen.rs`, where
/// `to_ascii_lowercase` left `mÜnchen.rs`. The pattern could then never
/// match, so the substring arms — which is where exact identifier and path
/// lookup lives now that the vector stems and drops stopwords — silently did
/// nothing for any query carrying a non-ASCII capital.
fn like_pattern(query: &str) -> String {
    let mut out = String::with_capacity(query.len() + 2);
    out.push('%');
    for ch in query.to_lowercase().chars() {
        match ch {
            '%' | '_' | '\\' => {
                out.push('\\');
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out.push('%');
    out
}

#[derive(Debug, sqlx::FromRow)]
struct ChunkCandidateRow {
    memory_id: uuid::Uuid,
    score: f32,
    /// The part of `score` contributed by the exact path / path-substring /
    /// text-substring arms. Only hybrid fusion reads it.
    literal_bonus: f32,
}

#[derive(Debug, sqlx::FromRow)]
struct CallPayloadRow {
    edge_id: uuid::Uuid,
    callee_name: String,
    is_dynamic: bool,
}

#[cfg(test)]
mod tests {
    use super::{
        ChunkCandidateRow, ChunkSearchMode, MAX_DISTINCTIVE_TERMS, MatchScores, distinctive_terms,
        fuse_candidates, like_pattern, reciprocal_rank,
    };
    use proxima::flavor::CodeChunkVectorCandidate;

    fn id(byte: u8) -> uuid::Uuid {
        uuid::Uuid::from_bytes([byte; 16])
    }

    fn lex(byte: u8, score: f32, literal_bonus: f32) -> ChunkCandidateRow {
        ChunkCandidateRow {
            memory_id: id(byte),
            score,
            literal_bonus,
        }
    }

    fn sem(byte: u8, similarity_score: f32) -> CodeChunkVectorCandidate {
        CodeChunkVectorCandidate {
            memory_id: id(byte),
            similarity_score,
        }
    }

    fn ranked(fused: &[MatchScores]) -> Vec<uuid::Uuid> {
        fused.iter().map(|scores| scores.memory_id).collect()
    }

    /// The promise the whole `lexical` mode rests on: with a semantic arm
    /// available but not asked for, the merge must reproduce the candidate
    /// scan's own `ORDER BY score DESC, memory_id DESC` exactly — same
    /// order, same reported score.
    #[test]
    fn lexical_mode_passes_its_arm_through_untouched() {
        let lexical = vec![lex(3, 5.2, 0.0), lex(1, 4.4, 0.0), lex(2, 4.4, 0.0)];
        let fused = fuse_candidates(ChunkSearchMode::Lexical, &lexical, &[]);
        // 1 and 2 tie on score, so the id tiebreak decides and it is
        // descending, matching the SQL.
        assert_eq!(ranked(&fused), vec![id(3), id(2), id(1)]);
        assert!((fused[0].score - 5.2).abs() < f32::EPSILON);
        assert!((fused[0].lexical_score - 5.2).abs() < f32::EPSILON);
        assert!(fused[0].similarity_score.abs() < f32::EPSILON);
    }

    #[test]
    fn semantic_mode_ranks_by_similarity_alone() {
        let semantic = vec![sem(9, 0.81), sem(4, 0.42)];
        let fused = fuse_candidates(ChunkSearchMode::Semantic, &[], &semantic);
        assert_eq!(ranked(&fused), vec![id(9), id(4)]);
        assert!((fused[0].score - 0.81).abs() < f32::EPSILON);
        assert!((fused[0].similarity_score - 0.81).abs() < f32::EPSILON);
        assert!(fused[0].lexical_score.abs() < f32::EPSILON);
    }

    /// A chunk both arms found beats one that only a single arm found, even
    /// when the single arm ranked it first. This is the whole point of rank
    /// fusion, and the reason a hybrid search finds what neither arm does
    /// alone.
    #[test]
    fn agreement_between_the_arms_outranks_either_arm_alone() {
        let lexical = vec![lex(1, 4.9, 0.0), lex(2, 4.8, 0.0)];
        let semantic = vec![sem(3, 0.9), sem(2, 0.7)];
        let fused = fuse_candidates(ChunkSearchMode::Hybrid, &lexical, &semantic);
        // 2 is second in both arms; 1 leads the lexical arm and 3 the
        // semantic one, and neither appears in the other.
        assert_eq!(ranked(&fused)[0], id(2));
    }

    /// Rank fusion alone would let a confident embedding neighbour bury an
    /// exact path match. It must not: the literal arms are the one case
    /// where a caller has said precisely what they want.
    #[test]
    fn an_exact_path_match_survives_a_confident_semantic_neighbour() {
        // The path match is *last* lexically and absent semantically; the
        // rival tops both arms.
        let lexical = vec![lex(1, 5.2, 0.0), lex(2, 14.9, 10.0)];
        let semantic = vec![sem(1, 0.99)];
        let fused = fuse_candidates(ChunkSearchMode::Hybrid, &lexical, &semantic);
        assert_eq!(ranked(&fused), vec![id(2), id(1)]);
    }

    /// Even the smallest literal bonus (a text substring, 4.0) outranks the
    /// largest possible fused rank contribution (2/61 < 0.033).
    #[test]
    fn the_smallest_literal_bonus_outranks_the_best_possible_rank_pair() {
        let best_possible = reciprocal_rank(0) * 2.0;
        assert!(
            best_possible < 4.0,
            "a rank pair scored {best_possible}, which would cross a literal arm",
        );
    }

    /// Both arms return the same chunk; it must appear once, carrying both
    /// scores rather than whichever arm was merged last.
    #[test]
    fn a_chunk_found_by_both_arms_is_reported_once_with_both_scores() {
        let fused = fuse_candidates(
            ChunkSearchMode::Hybrid,
            &[lex(7, 4.5, 0.0)],
            &[sem(7, 0.66)],
        );
        assert_eq!(fused.len(), 1);
        assert!((fused[0].lexical_score - 4.5).abs() < f32::EPSILON);
        assert!((fused[0].similarity_score - 0.66).abs() < f32::EPSILON);
        assert!((fused[0].score - (reciprocal_rank(0) * 2.0)).abs() < f32::EPSILON);
    }

    /// The candidates are merged through a `HashMap`, whose iteration order
    /// Rust randomises by design — the defect that made `calls_edges`
    /// return an arbitrary subset per run. `memory_id` is unique across the
    /// merged set, so score-then-id is a total order and the sort erases it.
    #[test]
    fn fusion_is_deterministic_across_runs() {
        let lexical = (0..40).map(|i| lex(i, 4.0, 0.0)).collect::<Vec<_>>();
        let semantic = (20..60).map(|i| sem(i, 0.5)).collect::<Vec<_>>();
        let first = ranked(&fuse_candidates(
            ChunkSearchMode::Hybrid,
            &lexical,
            &semantic,
        ));
        for _ in 0..16 {
            assert_eq!(
                first,
                ranked(&fuse_candidates(
                    ChunkSearchMode::Hybrid,
                    &lexical,
                    &semantic
                ))
            );
        }
    }

    #[test]
    fn reciprocal_rank_decreases_with_depth() {
        assert!(reciprocal_rank(0) > reciprocal_rank(1));
        assert!(reciprocal_rank(1) > reciprocal_rank(999));
        assert!(reciprocal_rank(999) > 0.0);
    }

    /// Postgres `lower()` folds non-ASCII; the pattern must too, or it can
    /// never match the column it is compared against.
    #[test]
    fn like_pattern_lowercases_the_way_postgres_does() {
        assert_eq!(like_pattern("MÜNCHEN.RS"), "%münchen.rs%");
        assert_eq!(like_pattern("Straße"), "%straße%");
        assert_eq!(like_pattern("ÅNGSTRÖM"), "%ångström%");
    }

    #[test]
    fn like_pattern_escapes_wildcards() {
        assert_eq!(like_pattern("a_b%c\\d"), "%a\\_b\\%c\\\\d%");
    }

    /// The property the rare bands depend on: a question with no identifiers
    /// must produce no terms, so `rare_all_tsq`/`rare_any_tsq` bind NULL and
    /// the query ranks exactly as it did before those bands existed.
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
    fn terms_are_deduplicated_case_insensitively_and_ordered() {
        assert_eq!(
            distinctive_terms("parseURL then PARSEURL then parseUrl and readFile2"),
            "parseURL readFile2"
        );
    }

    #[test]
    fn short_and_unstructured_tokens_are_rejected() {
        // `id` is too short, `fs` too short, `plugin` unstructured, `A1` too
        // short even though structured.
        assert_eq!(distinctive_terms("id fs plugin A1 the config"), "");
    }

    #[test]
    fn term_count_is_bounded() {
        let query = (0..40)
            .map(|i| format!("ident{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(
            distinctive_terms(&query).split_whitespace().count(),
            MAX_DISTINCTIVE_TERMS
        );
    }
}

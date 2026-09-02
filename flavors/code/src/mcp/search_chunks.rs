//! Code-chunk search is the flavor-scoped reference:
//!
//! 1. **content** — GIN on `proxima_code.projection` (and optional HNSW),
//!    joined to `code_chunk_v1` for its filters and literal bonuses
//! 2. **admit** — `Engine::query` `HeadsOnly` (`memory_head`)
//! 3. **pins** — call-neighbour index, only if `include_calls`
//!
//! Core `memory` is not in the content SQL. `core_search_memories` never
//! scans this table.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use proxima_core::MemoryId;
use proxima_core::mcp::cursor as wire_cursor;
use proxima_core::verbs::query::like_pattern;
use proxima_core::{Tool, ToolCtx, ToolError};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::contract::{
    CHUNK_BAND_RARE_ALL, CHUNK_BAND_RARE_ANY, CHUNK_BAND_RESCUE_ANY, CHUNK_BAND_STRICT,
    CODE_CHUNK_SCHEMA_ID,
};
use crate::payloads::{CodeChunkV1, FileState};
use proxima_storage_pg::query::{CodeChunkVectorCandidate, CodeChunkVectorFilters};

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
        length(max = proxima_core::MAX_QUERY_CHARS),
        description = "Query string for code chunk search, matched against file paths and chunk text. Takes an identifier or path for exact lookup, or a plain-English question — chunks sharing any content word are returned when none share all of them. 1 to 512 chars."
    )]
    pub query: String,
    #[serde(default)]
    #[schemars(
        description = "Ranking mode: `hybrid` (default) fuses full-text and embedding similarity, `lexical` is full-text only, `semantic` is embedding-only. Without a configured embedding model `hybrid` falls back to lexical and reports degraded_to_lexical=true, and `semantic` is rejected."
    )]
    pub mode: ChunkSearchMode,
    #[schemars(
        range(min = 1),
        description = "Optional maximum number of chunk matches. Omit or null for 12; values above 50 are clamped, and 0 is rejected."
    )]
    pub limit: Option<u32>,
    #[serde(default)]
    #[schemars(
        description = "Opaque pagination cursor from a previous response's `next_cursor`. Repeat the same query, mode, and filters; `limit` may vary between pages."
    )]
    pub cursor: Option<String>,
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
        description = "Whether to include neighbouring call connections, in both directions. Defaults to true."
    )]
    pub include_calls: bool,
    #[serde(default)]
    #[schemars(
        range(min = 1),
        description = "Maximum characters of chunk text per match. Omit or null for 2000; values above 8000 are clamped, and 0 is rejected. A match whose text was cut carries snippet_truncated=true — read the whole chunk with proxima-code_open_file_revision."
    )]
    pub snippet_max_chars: Option<usize>,
}

const fn default_include_calls() -> bool {
    true
}

/// Neighbour edges returned across the whole result set.
///
/// Applied in request order — chunk by chunk, in search-rank order — so the
/// edges that survive belong to the best-ranked matches and the same search
/// answers the same way twice.
const MAX_CALL_EDGES: usize = 200;

/// Characters of chunk text returned per match when the caller says nothing.
/// Covers a typical chunk whole; ceiling matches `core_search_memories`
/// `body_max_chars`.
const DEFAULT_SNIPPET_MAX_CHARS: usize = 2_000;

/// Ceiling on `snippet_max_chars`, matching `core_search_memories`'
/// `body_max_chars`.
const MAX_SNIPPET_MAX_CHARS: usize = proxima_core::MAX_TEXT_CAP_CHARS;

/// Most structured identifiers lifted out of one query. Bounds the size of
/// the derived tsquery; a query naming more than this many distinct
/// identifiers is already well served by the first twelve.
const MAX_DISTINCTIVE_TERMS: usize = 12;

/// Opaque cursor codec: `{v, fp, c}` like `list_repos` / `core_search_memories`.
/// The resume point is fused-rank `(score_bits, memory_id)`, not Query SQL.
const CHUNK_CURSOR: wire_cursor::FingerprintedCursor = wire_cursor::FingerprintedCursor {
    version: 1,
    source: "proxima-code_search_chunks response",
    rebind_hint: "repeat the query, mode, and filters that produced it",
};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct ChunkCursorPos {
    score_bits: u32,
    memory_id: Uuid,
    seen: u32,
}

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
/// `utf8`. Ordinary prose yields an empty string and the rare bands stay off.
///
/// Shape, not corpus rarity: identifiers, not low-df tokens (version
/// numbers and stack-trace noise).
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

#[derive(Debug, Serialize, JsonSchema)]
pub struct CodeSearchChunksOutput {
    pub matches: Vec<ChunkMatch>,
    pub calls_edges: Vec<CallEdge>,
    /// At least one further eligible match exists past this page in the
    /// scanned candidate window. True iff `next_cursor` is `Some`.
    pub has_more: bool,
    /// Opaque resume token for the next page. Pass back as `cursor` with
    /// the same query, mode, and filters. `None` when `has_more` is false.
    pub next_cursor: Option<String>,
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

#[derive(Debug, Serialize, JsonSchema)]
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

/// One caller→callee connection between two code chunks, with every call
/// site the caller's payload records for it.
///
/// There is no `edge_handle`: an edge has no id. The connection comes back
/// from the index; the sites come from the caller chunk's own payload
/// (docs/16 §The Model).
#[derive(Debug, Serialize, JsonSchema)]
pub struct CallEdge {
    pub source: Option<String>,
    pub target: Option<String>,
    /// Call sites in the caller chunk that reach this callee, in payload
    /// order. Empty when the caller chunk is not readable by this caller,
    /// which is also when `source` comes back `null`.
    pub sites: Vec<CallSite>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct CallSite {
    pub callee_name: String,
    pub is_dynamic: bool,
    pub byte_start: i64,
    pub byte_end: i64,
}

#[derive(Debug)]
pub struct CodeSearchChunksTool;

impl Tool for CodeSearchChunksTool {
    const NAME: &'static str = "proxima-code_search_chunks";
    const DESCRIPTION: &'static str = "Search head code chunks by exact substring, path, or full-text content, including plain-English questions. Ranks by mode: hybrid (default) fuses full-text with embedding similarity, lexical is full-text only, semantic is embedding-only; a hybrid search with no embeddings available answers lexically and reports degraded_to_lexical. Pages of at most 50: has_more plus an opaque next_cursor passed back as cursor with the same query, mode, and filters. Each match carries its chunk text up to snippet_max_chars, flagged snippet_truncated when cut. Supports language/chunk_type filters and optional call-neighbour connections with their call sites.";
    const ANNOTATIONS: Option<proxima_core::mcp::McpToolAnnotations> = Some(super::READ_ONLY);

    type Args = CodeSearchChunksArgs;
    type Output = CodeSearchChunksOutput;

    fn call(
        ctx: ToolCtx,
        args: CodeSearchChunksArgs,
    ) -> futures::future::BoxFuture<'static, Result<CodeSearchChunksOutput, ToolError>> {
        Box::pin(async move {
            let query = proxima_core::validate_search_query(&args.query)?;
            if args.snippet_max_chars == Some(0) {
                return Err(ToolError::InvalidInput(
                    "snippet_max_chars must be at least 1".into(),
                ));
            }
            proxima_core::reject_zero_limit(args.limit)?;
            let snippet_max_chars = effective_snippet_max_chars(args.snippet_max_chars);
            let limit = args.limit.unwrap_or(12).min(50);
            // The three input checks above stay ahead of this: resolving a
            // repo handle is a DB round trip that can answer `NotFound`, and
            // a request that is malformed *and* names a bad handle must
            // still be told what is malformed about it.
            let repo_id = match args.repo_handle.as_deref() {
                Some(handle) => Some(resolve_repo_identifier(&ctx, handle).await?),
                None => None,
            };
            let resolved = ResolvedChunkQuery {
                owner: ctx.owner(),
                query,
                requested_mode: args.mode,
                repo_id,
                language: args.language.as_deref(),
                chunk_type: args.chunk_type.as_deref(),
                exact_pattern: like_pattern(query),
            };
            let fingerprint = resolved.fingerprint();
            let after: Option<ChunkCursorPos> = args
                .cursor
                .as_deref()
                .map(|raw| CHUNK_CURSOR.decode(&fingerprint, raw))
                .transpose()?;
            let seen = after.map_or(0_u32, |pos| pos.seen);
            let pool = code_store(&ctx)?;
            let engine = super::engine(&ctx)?;
            // Resolved before either arm runs, because the answer decides
            // which arms run at all: a `semantic` request with no embedding
            // model is an error, not an empty result set, and a `hybrid` one
            // becomes a `lexical` one that reports having done so.
            let (effective_mode, query_embedding) =
                resolve_query_embedding(&engine, resolved.requested_mode, resolved.query).await?;

            let read_owner_ids = super::read_owner_ids(&engine, &ctx).await?;
            let needed = seen.saturating_add(limit);
            let candidate_limit = i64::from(needed.saturating_mul(20).max(needed).min(1_000));
            let scan = ChunkCandidateScan {
                resolved: &resolved,
                effective_mode,
                candidate_limit,
                read_owner_ids: &read_owner_ids,
                query_embedding: query_embedding.as_ref(),
            };
            let (rows, score_by_id) = collect_candidates(&ctx, &pool, &engine, &scan).await?;

            let ChunkPage {
                eligible,
                has_more,
                next_cursor,
            } = select_chunk_page(
                rows,
                &score_by_id,
                after,
                usize::try_from(limit).unwrap_or(usize::MAX),
                &fingerprint,
                seen,
            );
            let (matches, chunk_ids) =
                render_chunk_matches(&ctx, resolved.query, snippet_max_chars, eligible)?;

            // Phase 3: the call-neighbour pins, only when the caller asks
            // for them and the page phase 2 admitted is non-empty. Keyed on
            // that page's chunk ids, so the pins cost nothing on a miss.
            let calls_edges = if args.include_calls && !chunk_ids.is_empty() {
                load_call_edges(&ctx, &chunk_ids).await?
            } else {
                Vec::new()
            };

            Ok(CodeSearchChunksOutput {
                degraded_to_lexical: degraded_to_lexical(
                    resolved.requested_mode,
                    effective_mode,
                    &matches,
                ),
                mode: mode_label(resolved.requested_mode).to_string(),
                matches,
                calls_edges,
                has_more,
                next_cursor,
            })
        })
    }
}

/// Everything one chunk search's cursor is bound to, resolved once from the
/// arguments and used by every phase after it.
///
/// One value, one fingerprint: a filter cannot be added to the scan and
/// forgotten in the cursor canon, because both read the same fields.
///
/// `effective_mode` is deliberately *not* a field here. What the cursor
/// binds is the question the caller asked; the mode that ends up running is
/// a deployment fact resolved after the cursor is decoded. Fingerprinting
/// over it would fingerprint over embedding availability, so a cursor minted
/// while the embedding client was healthy would come back
/// `cursor does not match this query` after a provider blip — turning a
/// transient outage into a hard paging failure. The split across two types
/// is the statement of that distinction.
struct ResolvedChunkQuery<'a> {
    owner: proxima_core::Owner,
    query: &'a str,
    /// The mode the caller asked for, not the one that will run.
    requested_mode: ChunkSearchMode,
    repo_id: Option<Uuid>,
    language: Option<&'a str>,
    chunk_type: Option<&'a str>,
    exact_pattern: String,
}

impl ResolvedChunkQuery<'_> {
    /// The cursor fingerprint: blake3 over a positional JSON array of the
    /// six resolved values.
    ///
    /// The spelling is load-bearing, not incidental. `CHUNK_CURSOR.version`
    /// is `1` and outstanding cursors were minted against exactly these
    /// bytes, so the element order, the `mode_label` rendering, and the
    /// `json!([...]).to_string()` canon all have to stay as they are. A
    /// switch to a named-object canon is a cursor version bump, not a
    /// refactor.
    fn fingerprint(&self) -> String {
        let canon = serde_json::json!([
            self.owner.external_key(),
            self.query,
            mode_label(self.requested_mode),
            self.repo_id,
            self.language,
            self.chunk_type,
        ]);
        wire_cursor::fingerprint(&canon.to_string())
    }

    /// The lexical arm's sidecar scan over this query, with the run-time
    /// budget the caller supplies.
    fn sidecar_scan<'s>(
        &'s self,
        distinctive: &'s str,
        candidate_limit: i64,
        read_owner_ids: &'s [Uuid],
    ) -> ChunkSidecarScan<'s> {
        ChunkSidecarScan {
            repo_id: self.repo_id,
            language: self.language,
            chunk_type: self.chunk_type,
            exact_pattern: &self.exact_pattern,
            candidate_limit,
            distinctive,
            read_owner_ids,
        }
    }
}

/// One chunk search's run-time budget, shared by both candidate arms so a
/// filter cannot reach one arm and miss the other.
///
/// The filters themselves live on [`ResolvedChunkQuery`]; what is added here
/// is only what the deployment, not the caller, decided.
struct ChunkCandidateScan<'a> {
    resolved: &'a ResolvedChunkQuery<'a>,
    /// The mode that will actually run, after `resolve_query_embedding`.
    effective_mode: ChunkSearchMode,
    candidate_limit: i64,
    read_owner_ids: &'a [Uuid],
    /// The query embedding and the model that produced it, `None` when the
    /// semantic arm does not run.
    query_embedding: Option<&'a (Vec<f32>, String)>,
}

/// Phases 1 and 2: scan both content arms, fuse their ranks, and admit the
/// fused candidates.
///
/// Returns the admitted head payloads and, keyed by row id, the scores each
/// candidate was ranked by.
async fn collect_candidates(
    ctx: &ToolCtx,
    pool: &crate::CodeFlavorStore,
    engine: &proxima_core::Engine,
    scan: &ChunkCandidateScan<'_>,
) -> Result<(Vec<(MemoryId, CodeChunkV1)>, HashMap<Uuid, MatchScores>), ToolError> {
    let lexical_rows = scan_lexical_candidates(pool, scan).await?;
    let semantic_rows = scan_semantic_candidates(ctx, pool, scan).await?;

    // Admit: Query HeadsOnly. Content hits on a superseded t drop.
    let fused = fuse_candidates(scan.effective_mode, &lexical_rows, &semantic_rows);
    let candidate_ids = fused
        .iter()
        .map(|scores| scores.memory_id)
        .collect::<Vec<_>>();
    let score_by_id = fused
        .into_iter()
        .map(|scores| (scores.memory_id, scores))
        .collect::<HashMap<_, _>>();
    let rows = pool
        .authorized_abstraction_payloads::<CodeChunkV1>(
            engine,
            ctx.authz(),
            ctx.owner(),
            &candidate_ids,
            candidate_ids.len(),
        )
        .await?;
    Ok((rows, score_by_id))
}

/// Phase 1: sidecar-only content scan, narrowed to the caller's resolved
/// read set so the projection's composite `gin(owner_id, search_tsv)` can
/// serve it. Still overfetch: the read set admits a whole group, and phase 2
/// drops non-head ts.
async fn scan_lexical_candidates(
    pool: &crate::CodeFlavorStore,
    scan: &ChunkCandidateScan<'_>,
) -> Result<Vec<ChunkCandidateRow>, ToolError> {
    if scan.effective_mode == ChunkSearchMode::Semantic {
        return Ok(Vec::new());
    }
    let distinctive = distinctive_terms(scan.resolved.query);
    let sidecar =
        scan.resolved
            .sidecar_scan(&distinctive, scan.candidate_limit, scan.read_owner_ids);
    let gin = scan_chunk_sidecar(pool.pool(), scan.resolved.query, &sidecar).await?;
    // The substring arm is DECLARED, not blanket. A schema whose contract
    // says `SubstringArm::Off` contributes no statement and no rows; the
    // price for stopword-only and partial-word queries is then paid per
    // declaration, visibly, instead of being a mechanism nobody can turn
    // off.
    if gin.is_empty() && chunk_substring_arm_is_declared() {
        scan_chunk_sidecar_like(pool.pool(), scan.resolved.query, &sidecar).await
    } else {
        Ok(gin)
    }
}

/// The semantic arm draws on the same candidate budget as the lexical one
/// and applies the same structural filters — pushed into the neighbour scan
/// rather than applied to its output, because a search scoped to one
/// repository would otherwise spend its whole budget on whichever repository
/// is largest and come back empty.
async fn scan_semantic_candidates(
    ctx: &ToolCtx,
    pool: &crate::CodeFlavorStore,
    scan: &ChunkCandidateScan<'_>,
) -> Result<Vec<CodeChunkVectorCandidate>, ToolError> {
    let Some((embedding, model_id)) = scan.query_embedding else {
        return Ok(Vec::new());
    };
    pool.nearest_code_chunk_candidates(
        ctx.owner(),
        model_id,
        embedding,
        CodeChunkVectorFilters {
            repo_id: scan.resolved.repo_id,
            language: scan.resolved.language,
            chunk_type: scan.resolved.chunk_type,
        },
        usize::try_from(scan.candidate_limit).unwrap_or(0),
    )
    .await
}

/// One page of admitted candidates, with the truncation signal and the
/// cursor that resumes past it.
struct ChunkPage {
    eligible: Vec<(MemoryId, CodeChunkV1, MatchScores)>,
    has_more: bool,
    next_cursor: Option<String>,
}

/// Drop absent files and anything an earlier page already returned, cut the
/// page to `page_len`, and mint the resume token when more remain.
fn select_chunk_page(
    rows: Vec<(MemoryId, CodeChunkV1)>,
    score_by_id: &HashMap<Uuid, MatchScores>,
    after: Option<ChunkCursorPos>,
    page_len: usize,
    fingerprint: &str,
    seen: u32,
) -> ChunkPage {
    let mut eligible = Vec::new();
    for (memory_id, payload) in rows {
        if payload.state != FileState::Present {
            continue;
        }
        let raw_id = memory_id.into_inner();
        let scores = score_by_id.get(&raw_id).copied().unwrap_or_default();
        if after.is_some_and(|pos| !ranks_after_chunk_cursor(scores, pos)) {
            continue;
        }
        eligible.push((memory_id, payload, scores));
    }
    let has_more = eligible.len() > page_len;
    eligible.truncate(page_len);
    let next_cursor = (has_more && !eligible.is_empty()).then(|| {
        let (_, _, scores) = eligible.last().expect("non-empty page");
        CHUNK_CURSOR.encode(
            fingerprint,
            &ChunkCursorPos {
                score_bits: scores.score.to_bits(),
                memory_id: scores.memory_id,
                seen: seen.saturating_add(u32::try_from(eligible.len()).unwrap_or(u32::MAX)),
            },
        )
    });
    ChunkPage {
        eligible,
        has_more,
        next_cursor,
    }
}

/// Render the page into wire matches, and collect the same page's row ids
/// for the call-neighbour phase.
fn render_chunk_matches(
    ctx: &ToolCtx,
    query: &str,
    snippet_max_chars: usize,
    eligible: Vec<(MemoryId, CodeChunkV1, MatchScores)>,
) -> Result<(Vec<ChunkMatch>, Vec<Uuid>), ToolError> {
    let mut matches = Vec::with_capacity(eligible.len());
    let mut chunk_ids = Vec::with_capacity(eligible.len());
    for (memory_id, payload, scores) in eligible {
        chunk_ids.push(memory_id.into_inner());
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
    Ok((matches, chunk_ids))
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
            return Err(ToolError::Unavailable(
                SEMANTIC_CHUNK_SEARCH_UNAVAILABLE.to_string(),
            ));
        }
        return Ok((ChunkSearchMode::Lexical, None));
    };
    // The client can vanish, or its call fail, between the probe above and
    // here; both land in the same place.
    match embed.embed(query).await {
        Ok(embedding) => Ok((mode, Some((embedding, embed.model_id().to_string())))),
        Err(err) if mode == ChunkSearchMode::Semantic => {
            tracing::warn!(error = %err, "embedding provider failed");
            Err(ToolError::Unavailable(
                "semantic chunk search unavailable: embedding provider error".into(),
            ))
        }
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
/// `Lexical` and `Semantic` each pass their own arm through untouched: the
/// order and the reported score are the arm's own, including the tiebreak,
/// which reproduces the candidate scan's
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
    semantic: &[CodeChunkVectorCandidate],
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

/// True when `scores` sorts strictly after the last emitted row
/// (`score DESC, memory_id DESC`).
fn ranks_after_chunk_cursor(scores: MatchScores, pos: ChunkCursorPos) -> bool {
    let score = f32::from_bits(pos.score_bits);
    match scores.score.total_cmp(&score) {
        std::cmp::Ordering::Less => true,
        std::cmp::Ordering::Equal => scores.memory_id < pos.memory_id,
        std::cmp::Ordering::Greater => false,
    }
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
        || proxima::flavor::hybrid_degraded_to_lexical(
            proxima::flavor::SearchMode::Hybrid,
            matches.is_empty(),
            matches.iter().any(|m| m.similarity_score > 0.0),
        )
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
    let pool = code_store(ctx)?;
    let pair_rows: Vec<(uuid::Uuid, uuid::Uuid)> = sqlx::query_as(
        "SELECT caller_memory_id, callee_memory_id
           FROM proxima_code.code_chunk_call_v1
          WHERE caller_memory_id = ANY($1::uuid[])
             OR callee_memory_id = ANY($1::uuid[])
          ORDER BY caller_memory_id, callee_memory_id
          LIMIT $2",
    )
    .bind(chunk_ids)
    .bind(i64::try_from(MAX_CALL_EDGES).unwrap_or(i64::MAX))
    .fetch_all(pool.pool())
    .await
    .map_err(map_storage)?;
    let mut seen: HashSet<(uuid::Uuid, uuid::Uuid)> = HashSet::new();
    let mut pairs: Vec<(MemoryId, MemoryId)> = Vec::new();
    for (caller, callee) in pair_rows {
        if seen.insert((caller, callee)) {
            pairs.push((MemoryId::new(caller), MemoryId::new(callee)));
        }
    }

    // Hydrate the sites from the caller chunks' payload rows. The index
    // answers "is there a connection"; this is the node answering "what is
    // it", and it is one query for the whole page.
    let sources = pairs
        .iter()
        .map(|(source, _)| source.into_inner())
        .collect::<Vec<_>>();
    let targets = pairs
        .iter()
        .map(|(_, target)| target.into_inner())
        .collect::<Vec<_>>();
    let site_rows: Vec<CallSiteRow> = sqlx::query_as(
        "SELECT caller_memory_id, callee_memory_id, callee_name, is_dynamic,
                byte_start, byte_end
           FROM proxima_code.code_chunk_call_v1
          WHERE caller_memory_id = ANY($1::uuid[])
            AND callee_memory_id = ANY($2::uuid[])
          ORDER BY caller_memory_id, callee_memory_id, site_index",
    )
    .bind(&sources)
    .bind(&targets)
    .fetch_all(pool.pool())
    .await
    .map_err(map_storage)?;
    let mut sites: HashMap<(uuid::Uuid, uuid::Uuid), Vec<CallSite>> = HashMap::new();
    for row in site_rows {
        sites
            .entry((row.caller_memory_id, row.callee_memory_id))
            .or_default()
            .push(CallSite {
                callee_name: row.callee_name,
                is_dynamic: row.is_dynamic,
                byte_start: row.byte_start,
                byte_end: row.byte_end,
            });
    }

    Ok(pairs
        .into_iter()
        .map(|(source, target)| CallEdge {
            source: Some(ctx.format_abstraction_memory(source)),
            target: Some(ctx.format_abstraction_memory(target)),
            sites: sites
                .remove(&(source.into_inner(), target.into_inner()))
                .unwrap_or_default(),
        })
        .collect())
}

struct ChunkSidecarScan<'a> {
    repo_id: Option<uuid::Uuid>,
    language: Option<&'a str>,
    chunk_type: Option<&'a str>,
    exact_pattern: &'a str,
    candidate_limit: i64,
    distinctive: &'a str,
    /// The caller's read access set, resolved by
    /// [`proxima_core::Engine::authorized_read_owners`]. Phase 2 admits
    /// against the same set; this copy exists so the scan can index on it.
    read_owner_ids: &'a [uuid::Uuid],
}

/// Phase 1: GIN on `proxima_code.projection` only. No `proxima_core.*`
/// content tables.
///
/// `proxima_code.code_lexical_config()` is pinned (code is not the
/// deployment's prose language).
/// Bands: strict 4.x / rare-all 3.x / rare-any 2.x / rescue 1.x, plus
/// path/text literal bonuses on the GIN hit set. `LIKE` is a separate
/// scan, only when this GIN arm returns nothing.
async fn scan_chunk_sidecar(
    pool: &sqlx::PgPool,
    query: &str,
    scan: &ChunkSidecarScan<'_>,
) -> Result<Vec<ChunkCandidateRow>, ToolError> {
    // SQL-POLICY: fixed-fragment
    sqlx::query_as(sqlx::AssertSqlSafe(CHUNK_GIN_SQL.as_str()))
        .bind(query)
        .bind(scan.repo_id)
        .bind(scan.language)
        .bind(scan.exact_pattern)
        .bind(scan.chunk_type)
        .bind(scan.candidate_limit)
        .bind(scan.distinctive)
        .bind(scan.read_owner_ids)
        .fetch_all(pool)
        .await
        .map_err(map_storage)
}

/// The GIN arm, over `proxima_code.projection`.
///
/// Every window and every `ts_rank` normalization flag below is READ OFF
/// the declaration (`contract::band`), not spelled here: the four
/// `CHUNK_BAND_*` names are the lookup keys, and the numbers they resolve
/// to live in `CHUNK_BANDS` where the contract can be checked. The
/// rendering is `{:.2}`, so `0.6` becomes `0.60` — the same `numeric` to
/// `PostgreSQL`, so no score moves.
///
/// # Why this arm drives from the sidecar, where core's drives from the
/// projection
///
/// Because the contract says so:
/// `RankSource::SidecarWithProjectionOwner` on `CODE_PROJECTION`, whose
/// `why` carries the argument below in a form a deployment layer can read.
///
/// The two reasons, in short, both of which change *which rows come back*
/// and not just how fast:
///
/// 1. The score reads sidecar columns. `chunk_type <> 'file'`, an exact
///    `file_path` match, a path substring and a text substring contribute
///    `0.3 / 10.0 / 6.0 / 4.0`. Those dwarf the tsvector band. Ranking on
///    `search_tsv` first and adding the literal bonuses afterwards would
///    order by the smaller half of the score and truncate before the larger
///    half is known.
/// 2. The filters are selective and sidecar-side. `repo_id`, `language`,
///    `chunk_type` and `state = 'Present'` are the shape of every real
///    query. Taking a projection-side top-k first and filtering after would
///    spend the whole candidate budget on the largest repository and answer
///    a repo-scoped search with nothing — the same failure the semantic arm
///    documents at its call site.
///
/// The composite `gin(owner_id, search_tsv)` on `proxima_code.projection`
/// stays reachable because `p.owner_id = ANY($8)` puts both index columns
/// on `p`. The owner set is the caller's resolved read set, so the scan
/// reads only rows phase 2 could admit anyway.
/// `code_hot_path_plans_use_expected_indexes` pins the index.
static CHUNK_GIN_SQL: LazyLock<String> = LazyLock::new(|| {
    let strict = crate::contract::band(CODE_CHUNK_SCHEMA_ID, CHUNK_BAND_STRICT);
    let rare_all = crate::contract::band(CODE_CHUNK_SCHEMA_ID, CHUNK_BAND_RARE_ALL);
    let rare_any = crate::contract::band(CODE_CHUNK_SCHEMA_ID, CHUNK_BAND_RARE_ANY);
    let rescue = crate::contract::band(CODE_CHUNK_SCHEMA_ID, CHUNK_BAND_RESCUE_ANY);
    let (strict_floor, strict_width) = strict.parts();
    let (rare_all_floor, rare_all_width) = rare_all.parts();
    let (rare_any_floor, rare_any_width) = rare_any.parts();
    let (rescue_floor, rescue_width) = rescue.parts();
    let strict_norm = strict.normalization_arg();
    let rare_all_norm = rare_all.normalization_arg();
    let rare_any_norm = rare_any.normalization_arg();
    let rescue_norm = rescue.normalization_arg();
    format!(
        "WITH q AS (
             SELECT websearch_to_tsquery(proxima_code.code_lexical_config(),
                        proxima_core.lexical_scrub($1)) AS tsq,
                    NULLIF(
                        replace(
                            plainto_tsquery(proxima_code.code_lexical_config(),
                                proxima_core.lexical_scrub($1))::text,
                            ' & ', ' | '),
                        '')::tsquery AS any_tsq,
                    websearch_to_tsquery(proxima_code.code_lexical_config(),
                        proxima_core.lexical_scrub(NULLIF($7, ''))) AS rare_all_tsq,
                    NULLIF(
                        replace(
                            plainto_tsquery(proxima_code.code_lexical_config(),
                                proxima_core.lexical_scrub(NULLIF($7, '')))::text,
                            ' & ', ' | '),
                        '')::tsquery AS rare_any_tsq
         )
         SELECT c.t AS memory_id,
                (
                    GREATEST(
                        CASE WHEN p.search_tsv @@ q.tsq
                             THEN {strict_floor} + LEAST(ts_rank_cd(p.search_tsv, q.tsq{strict_norm}), 1.0) * {strict_width}
                             ELSE 0.0 END,
                        CASE WHEN q.rare_all_tsq IS NOT NULL AND p.search_tsv @@ q.rare_all_tsq
                             THEN {rare_all_floor} + LEAST(ts_rank(p.search_tsv, q.rare_all_tsq{rare_all_norm}) * 100.0, 1.0) * {rare_all_width}
                             ELSE 0.0 END,
                        CASE WHEN q.rare_any_tsq IS NOT NULL AND p.search_tsv @@ q.rare_any_tsq
                             THEN {rare_any_floor} + LEAST(ts_rank(p.search_tsv, q.rare_any_tsq{rare_any_norm}) * 100.0, 1.0) * {rare_any_width}
                             ELSE 0.0 END,
                        CASE WHEN q.any_tsq IS NOT NULL AND p.search_tsv @@ q.any_tsq
                             THEN {rescue_floor} + LEAST(ts_rank(p.search_tsv, q.any_tsq{rescue_norm}) * 100.0, 1.0) * {rescue_width}
                             ELSE 0.0 END
                    )
                    + CASE WHEN c.chunk_type <> 'file' THEN 0.3 ELSE 0.0 END
                    + CASE WHEN lower(c.file_path) = lower($1) THEN 10.0 ELSE 0.0 END
                    + CASE WHEN lower(c.file_path) LIKE $4 ESCAPE '\\' THEN 6.0 ELSE 0.0 END
                    + CASE WHEN lower(c.text) LIKE $4 ESCAPE '\\' THEN 4.0 ELSE 0.0 END
                )::real AS score,
                (
                    CASE WHEN lower(c.file_path) = lower($1) THEN 10.0 ELSE 0.0 END
                    + CASE WHEN lower(c.file_path) LIKE $4 ESCAPE '\\' THEN 6.0 ELSE 0.0 END
                    + CASE WHEN lower(c.text) LIKE $4 ESCAPE '\\' THEN 4.0 ELSE 0.0 END
                )::real AS literal_bonus
           FROM proxima_code.code_chunk_v1 c
           JOIN proxima_code.projection p
             ON p.memory_id = c.t
            AND p.schema_id = 'proxima-code/code-chunk-v1'
            AND p.owner_id = ANY($8::uuid[]), q
          WHERE c.state = 'Present'
            AND ($2::uuid IS NULL OR c.repo_id = $2)
            AND ($3::text IS NULL OR c.language = $3)
            AND ($5::text IS NULL OR c.chunk_type = $5)
            AND (
                p.search_tsv @@ q.tsq
                OR (q.any_tsq IS NOT NULL AND p.search_tsv @@ q.any_tsq)
                OR (q.rare_any_tsq IS NOT NULL AND p.search_tsv @@ q.rare_any_tsq)
            )
          ORDER BY score DESC, c.t DESC
          LIMIT $6"
    )
});

#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
#[must_use]
pub fn chunk_gin_sql_for_tests() -> &'static str {
    CHUNK_GIN_SQL.as_str()
}

/// The substring arm, over the sidecar's own trigram-indexed columns.
///
/// `SubstringArm::SameTableLike` declares this shape, and
/// `chunk_substring_arm_is_declared` stops it running for a schema that
/// turns it off. It fires only when the `@@` arm returns nothing: this tool
/// ranks exactly one schema, so "the ranked arm returned nothing for this
/// schema" and "the ranked arm returned nothing" are the same sentence.
///
/// `p.owner_id = ANY($7)` keeps candidate generation owner-scoped, so a
/// neighbour's repository cannot consume the whole candidate budget before
/// authorization runs. The owner reaches a code sidecar through the Memory,
/// and the spelling is a join to THIS FLAVOR's own projection — the same
/// table, composite index and alias the `@@` arm joins — rather than to
/// `proxima_core.memory`, which flavor SQL may not name.
static CHUNK_LIKE_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "SELECT c.t AS memory_id,
                (
                    CASE WHEN c.chunk_type <> 'file' THEN 0.3 ELSE 0.0 END
                    + CASE WHEN lower(c.file_path) = lower($1) THEN 10.0 ELSE 0.0 END
                    + CASE WHEN lower(c.file_path) LIKE $4 ESCAPE '\\' THEN 6.0 ELSE 0.0 END
                    + CASE WHEN lower(c.text) LIKE $4 ESCAPE '\\' THEN 4.0 ELSE 0.0 END
                )::real AS score,
                (
                    CASE WHEN lower(c.file_path) = lower($1) THEN 10.0 ELSE 0.0 END
                    + CASE WHEN lower(c.file_path) LIKE $4 ESCAPE '\\' THEN 6.0 ELSE 0.0 END
                    + CASE WHEN lower(c.text) LIKE $4 ESCAPE '\\' THEN 4.0 ELSE 0.0 END
                )::real AS literal_bonus
           FROM proxima_code.code_chunk_v1 c
           JOIN proxima_code.projection p
             ON p.memory_id = c.t
            AND p.schema_id = '{CODE_CHUNK_SCHEMA_ID}'
            AND p.owner_id = ANY($7::uuid[])
          WHERE c.state = 'Present'
            AND ($2::uuid IS NULL OR c.repo_id = $2)
            AND ($3::text IS NULL OR c.language = $3)
            AND ($5::text IS NULL OR c.chunk_type = $5)
            AND (
                lower(c.file_path) = lower($1)
                OR lower(c.file_path) LIKE $4 ESCAPE '\\'
                OR lower(c.text) LIKE $4 ESCAPE '\\'
            )
          ORDER BY score DESC, c.t DESC
          LIMIT $6"
    )
});

#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
#[must_use]
pub fn chunk_like_sql_for_tests() -> &'static str {
    CHUNK_LIKE_SQL.as_str()
}

/// Whether `proxima-code/code-chunk-v1` opts into a substring arm.
///
/// `SameTableLike` is the shape this tool implements; anything else — an
/// `Off`, or a shape this renderer does not have — contributes nothing
/// rather than silently running the one lane that exists.
fn chunk_substring_arm_is_declared() -> bool {
    matches!(
        crate::contract::substring_arm(CODE_CHUNK_SCHEMA_ID),
        Some(proxima_core::flavor::SubstringArm::SameTableLike)
    )
}

async fn scan_chunk_sidecar_like(
    pool: &sqlx::PgPool,
    query: &str,
    scan: &ChunkSidecarScan<'_>,
) -> Result<Vec<ChunkCandidateRow>, ToolError> {
    // SQL-POLICY: fixed-fragment
    sqlx::query_as(sqlx::AssertSqlSafe(CHUNK_LIKE_SQL.as_str()))
        .bind(query)
        .bind(scan.repo_id)
        .bind(scan.language)
        .bind(scan.exact_pattern)
        .bind(scan.chunk_type)
        .bind(scan.candidate_limit)
        .bind(scan.read_owner_ids)
        .fetch_all(pool)
        .await
        .map_err(map_storage)
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
struct CallSiteRow {
    caller_memory_id: uuid::Uuid,
    callee_memory_id: uuid::Uuid,
    callee_name: String,
    is_dynamic: bool,
    byte_start: i64,
    byte_end: i64,
}

#[cfg(test)]
mod tests {
    use super::{
        ChunkCandidateRow, ChunkSearchMode, MAX_DISTINCTIVE_TERMS, MatchScores, ResolvedChunkQuery,
        distinctive_terms, fuse_candidates, reciprocal_rank,
    };
    use proxima_storage_pg::query::CodeChunkVectorCandidate;

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
    /// Rust randomises by design. `memory_id` is unique across the merged
    /// set, so score-then-id is a total order and the sort erases that
    /// randomness.
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

    fn owner(byte: u8) -> proxima_core::Owner {
        proxima_core::Owner::Personal(proxima_core::UserId::new(id(byte)))
    }

    fn resolved() -> ResolvedChunkQuery<'static> {
        ResolvedChunkQuery {
            owner: owner(1),
            query: "parse_chunk",
            requested_mode: ChunkSearchMode::Hybrid,
            repo_id: Some(id(9)),
            language: Some("rust"),
            chunk_type: Some("function"),
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
}

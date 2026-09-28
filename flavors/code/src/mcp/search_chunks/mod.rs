//! Code-chunk search is the flavor-scoped reference:
//!
//! 1. **content** — GIN on `proxima_code.projection` (and optional HNSW),
//!    joined to `code_chunk_v1` for its filters and literal bonuses
//! 2. **admit** — `Engine::query` `HeadsOnly` (`memory_head`)
//! 3. **pins** — call-neighbour index, only if `include_calls`
//!
//! Core `memory` is not in the content SQL. `core_search_memories` never
//! scans this table.

use proxima_core::mcp::ToolEffect;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use proxima_core::MemoryId;
use proxima_core::llm::SemanticWeight;
use proxima_core::mcp::cursor as wire_cursor;
use proxima_core::verbs::query::like_pattern;
use proxima_core::{Tool, ToolCtx, ToolError};
use proxima_storage_pg::begin_compatible_owner_transaction;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::contract::{
    CHUNK_BAND_RARE_ALL, CHUNK_BAND_RARE_ANY, CHUNK_BAND_RESCUE_ANY, CHUNK_BAND_STRICT,
    CODE_CHUNK_SCHEMA_ID,
};
use crate::file_class::FileClass;
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
        description = "Ranking mode. `semantic` (embedding-only) for a question describing behaviour; `lexical` (full-text only) for an exact identifier, string or path; `hybrid` (default) fuses both. Without a configured embedding model `hybrid` falls back to lexical and reports degraded_to_lexical=true, and `semantic` is rejected."
    )]
    pub mode: ChunkSearchMode,
    #[schemars(
        description = "Hybrid fusion weight on the semantic ranking in 0..=1; the lexical ranking gets the complement, and 0.5 weighs them alike. A chunk whose path or text contains the query ranks first at any weight. Omit or null for the deployment's weight for code search, else 0.5. Only valid with mode=hybrid."
    )]
    pub semantic_weight: Option<f32>,
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
        description = "Optional repository filter: an `R…` repo_handle, or a registered repository's display name, path or directory name, case-insensitive. A name matching more than one repository is rejected; pass that repository's repo_handle instead. Omit or null to search all visible repos."
    )]
    pub repo_handle: Option<String>,
    #[schemars(
        description = "Optional language filter, one of `rust`, `typescript`, `tsx`, `javascript`, `python`, `go`, `markdown`, `toml`, `json`, `yaml`, `sql`, `text`; `tsx` files are not `typescript`. Any other value is rejected. Omit or null for all languages."
    )]
    pub language: Option<String>,
    #[schemars(description = "Optional chunk type filter. Omit or null for all chunk types.")]
    pub chunk_type: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Optional file class filter: `source`, `generated` (generated code, snapshots, minified bundles, source maps), `vendored` (vendored and third-party code) or `lockfile`. Omit or null for every class; hybrid then ranks every source match above every other one."
    )]
    pub file_class: Option<FileClass>,
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
    #[serde(default)]
    #[schemars(
        description = "Return the lines within this many of matched_line, each prefixed with its line number (`2610: …`), as the snippet instead of the chunk text from its start. Applies only to matches that carry matched_line; 0 returns that line alone. snippet_max_chars still caps the result. Omit or null for the chunk text."
    )]
    pub context_lines: Option<u32>,
    #[serde(default)]
    #[schemars(
        description = "Add diagnostic fields to each match: language, chunk_index, byte_range, match_kind, matched_excerpt, lexical_score and similarity_score. Defaults to false."
    )]
    pub verbose: bool,
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
    /// The last row's [`MatchScores::tier`]. Absent from a cursor minted
    /// before tiers existed, when every row was tier 0.
    #[serde(default)]
    tier: u8,
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
const SEMANTIC_CHUNK_SEARCH_UNAVAILABLE: &str = "semantic chunk search unavailable: no embedding model is configured for this space. \
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

/// Refuse a `language` no chunk can carry. The filter is exact, so an
/// unknown value would otherwise return an empty page that reads like
/// "no such code".
fn reject_unknown_language(language: Option<&str>) -> Result<(), ToolError> {
    match language {
        Some(label) if !crate::chunker::LANGUAGE_LABELS.contains(&label) => {
            Err(ToolError::InvalidInput(format!(
                "language must be one of {}; got {label:?}",
                crate::chunker::LANGUAGE_LABELS.join(", ")
            )))
        }
        _ => Ok(()),
    }
}

/// The argument checks that need no database round trip.
fn reject_malformed_args(args: &CodeSearchChunksArgs) -> Result<(), ToolError> {
    if args.snippet_max_chars == Some(0) {
        return Err(ToolError::InvalidInput(
            "snippet_max_chars must be at least 1".into(),
        ));
    }
    proxima_core::reject_zero_limit(args.limit)?;
    reject_unknown_language(args.language.as_deref())
}

/// A repository's `R…` handle.
fn format_repo_handle(ctx: &ToolCtx, repo_id: Uuid) -> String {
    ctx.format_flavor_object(super::REPO_HANDLE_KIND, repo_id, super::REPO_HANDLE_PREFIX)
}

/// Resolve the requested snippet budget against the ceiling.
fn effective_snippet_max_chars(requested: Option<usize>) -> usize {
    requested.map_or(DEFAULT_SNIPPET_MAX_CHARS, |max| {
        max.min(MAX_SNIPPET_MAX_CHARS)
    })
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct CodeSearchChunksOutput {
    /// The repository a search scoped by `repo_handle` ran in, as its `R…`
    /// handle. Absent for a search over every visible repository, where
    /// each match carries its own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_handle: Option<String>,
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

/// One chunk a search found, lean by default: what an agent reads, cites
/// and opens. `verbose` adds [`ChunkMatchDetail`].
#[derive(Debug, Serialize, JsonSchema)]
pub struct ChunkMatch {
    /// The chunk's handle, the name `calls_edges` uses for it.
    pub handle: String,
    /// The chunk's repository. Absent when the search was scoped to one,
    /// which the response's `repo_handle` names.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_handle: Option<String>,
    pub file_path: String,
    pub chunk_type: String,
    /// The class of the file the chunk was cut from: `generated`,
    /// `vendored` or `lockfile`. Absent for source.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_class: Option<FileClass>,
    /// The chunk's first and last line in the file, 1-based.
    pub line_range: (i64, i64),
    /// The chunk text from its start, or with `context_lines` the numbered
    /// lines around `matched_line`.
    pub snippet: String,
    /// `true` when `snippet` holds less than the whole chunk: cut at
    /// `snippet_max_chars`, or a `context_lines` window. Read the whole
    /// chunk with `proxima-code_open_file_revision`, or re-run with a larger
    /// `snippet_max_chars` or `context_lines`.
    pub snippet_truncated: bool,
    /// The file line that best matches the query: the first line holding
    /// the whole query, else the first line sharing the most query words.
    /// Absent for a path match and for a chunk only the semantic arm found.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_line: Option<i64>,
    /// The score this match was ranked by, in the units of the mode that
    /// ran: a lexical band score for `lexical`, cosine similarity for
    /// `semantic`, a fused rank score for `hybrid`. Comparable within one
    /// response, not across modes.
    pub score: f32,
    #[serde(flatten)]
    pub detail: Option<ChunkMatchDetail>,
}

/// The per-match fields a `verbose` search adds.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ChunkMatchDetail {
    pub language: Option<String>,
    pub chunk_index: i32,
    pub byte_range: (i64, i64),
    /// `path_exact`, `path_contains`, `text_contains` (a line holds the
    /// whole query) or `full_text`.
    pub match_kind: String,
    /// The matched line, trimmed to 480 characters, or the path for a path
    /// match.
    pub matched_excerpt: Option<String>,
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
    /// The caller chunk, `null` when this caller cannot read it.
    pub source: Option<String>,
    /// The callee chunk, `null` when this caller cannot read it — a callee
    /// moved to another Owner keeps its index row but not its id.
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
    const DESCRIPTION: &'static str = "Search head code chunks by exact substring, path, or full-text content, including plain-English questions. Ranks by mode: semantic (embedding-only) suits a question describing behaviour, lexical (full-text only) an exact identifier, string or path, and hybrid (default) fuses both; a hybrid search with no embeddings available answers lexically and reports degraded_to_lexical. Pages of at most 50: has_more plus an opaque next_cursor passed back as cursor with the same query, mode, and filters. Each match carries its chunk text up to snippet_max_chars, flagged snippet_truncated when cut, and matched_line, the line that best matches the query when one does; context_lines returns the numbered lines around it instead, and verbose adds per-arm scores and byte ranges. Supports language/chunk_type/file_class filters and optional call-neighbour connections with their call sites. Generated, vendored and lockfile chunks stay searchable, carry file_class, and rank after every source match in hybrid mode unless file_class asks for them. An unfiltered hybrid search takes semantic neighbours for source and for the other classes as two sets, each up to the candidate budget, so a non-source vector cannot take a source slot.";
    const EFFECT: Option<ToolEffect> = Some(ToolEffect::ReadOnly);

    type Args = CodeSearchChunksArgs;
    type Output = CodeSearchChunksOutput;

    fn call(
        ctx: ToolCtx,
        args: CodeSearchChunksArgs,
    ) -> futures::future::BoxFuture<'static, Result<CodeSearchChunksOutput, ToolError>> {
        Box::pin(async move {
            let query = proxima_core::validate_search_query(&args.query)?;
            reject_malformed_args(&args)?;
            let semantic_weight = requested_semantic_weight(args.semantic_weight, args.mode)?;
            let snippet_max_chars = effective_snippet_max_chars(args.snippet_max_chars);
            let limit = args.limit.unwrap_or(12).min(50);
            // The input checks above stay ahead of this: resolving a
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
                semantic_weight,
                repo_id,
                language: args.language.as_deref(),
                chunk_type: args.chunk_type.as_deref(),
                file_class: args.file_class,
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
            let QueryEmbedding {
                mode: effective_mode,
                vector: query_embedding,
                route_weight,
            } = resolve_query_embedding(
                &engine,
                &resolved.owner,
                resolved.requested_mode,
                resolved.query,
            )
            .await?;

            let read_owner_ids = super::read_owner_ids(&engine, &ctx).await?;
            let needed = seen.saturating_add(limit);
            let candidate_limit = i64::from(needed.saturating_mul(20).max(needed).min(1_000));
            let scan = ChunkCandidateScan {
                resolved: &resolved,
                effective_mode,
                semantic_weight: resolved
                    .semantic_weight
                    .or(route_weight)
                    .unwrap_or(SemanticWeight::EVEN),
                candidate_limit,
                read_owner_ids: &read_owner_ids,
                query_embedding: query_embedding.as_ref(),
            };
            let (rows, score_by_id) = collect_candidates(&ctx, &pool, &engine, &scan).await?;

            let page = select_chunk_page(
                rows,
                &score_by_id,
                after,
                usize::try_from(limit).unwrap_or(usize::MAX),
                &fingerprint,
                seen,
                resolved.source_first(),
            );
            let semantic_reached = page
                .eligible
                .iter()
                .any(|(_, _, scores)| scores.similarity_score > 0.0);
            let render = MatchRendering {
                query: resolved.query,
                snippet_max_chars,
                context_lines: args.context_lines,
                verbose: args.verbose,
                repo_scoped: resolved.repo_id.is_some(),
            };
            let (matches, chunk_ids) = render_chunk_matches(&ctx, &render, page.eligible)?;

            // Phase 3: the call-neighbour pins, only when the caller asks
            // for them and the page phase 2 admitted is non-empty. Keyed on
            // that page's chunk ids, so the pins cost nothing on a miss.
            let calls_edges = if args.include_calls && !chunk_ids.is_empty() {
                load_call_edges(&ctx, &engine, &chunk_ids).await?
            } else {
                Vec::new()
            };

            Ok(CodeSearchChunksOutput {
                degraded_to_lexical: degraded_to_lexical(
                    resolved.requested_mode,
                    effective_mode,
                    matches.is_empty(),
                    semantic_reached,
                ),
                mode: mode_label(resolved.requested_mode).to_string(),
                repo_handle: resolved
                    .repo_id
                    .map(|repo_id| format_repo_handle(&ctx, repo_id)),
                matches,
                calls_edges,
                has_more: page.has_more,
                next_cursor: page.next_cursor,
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
    /// The hybrid weight the caller asked for. A route's default is, like the
    /// effective mode, a deployment fact and not bound here.
    semantic_weight: Option<SemanticWeight>,
    repo_id: Option<Uuid>,
    language: Option<&'a str>,
    chunk_type: Option<&'a str>,
    file_class: Option<FileClass>,
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
    /// refactor. A requested `semantic_weight` is appended as a seventh
    /// value, its bits, so a query without one keeps exactly those bytes;
    /// a `file_class` filter after it, as its name — a string, which no
    /// weight's bits can be.
    fn fingerprint(&self) -> String {
        let mut canon = vec![
            serde_json::json!(self.owner.external_key()),
            serde_json::json!(self.query),
            serde_json::json!(mode_label(self.requested_mode)),
            serde_json::json!(self.repo_id),
            serde_json::json!(self.language),
            serde_json::json!(self.chunk_type),
        ];
        if let Some(weight) = self.semantic_weight {
            canon.push(serde_json::json!(weight.get().to_bits()));
        }
        if let Some(class) = self.file_class {
            canon.push(serde_json::json!(class.as_str()));
        }
        wire_cursor::fingerprint(&serde_json::Value::Array(canon).to_string())
    }

    /// A hybrid search over every class ranks each source match above
    /// every non-source one. Keyed on the requested mode, like the cursor:
    /// a hybrid search that degrades to lexical keeps the order it asked
    /// for. A lexical or semantic search, or one naming a class, ranks as
    /// its arm does.
    fn source_first(&self) -> bool {
        self.requested_mode == ChunkSearchMode::Hybrid && self.file_class.is_none()
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
            file_class: self.file_class.map(FileClass::as_str),
            source_first: self.source_first(),
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
    /// The weight hybrid fusion runs at: the caller's, else the route's,
    /// else even.
    semantic_weight: SemanticWeight,
    candidate_limit: i64,
    read_owner_ids: &'a [Uuid],
    /// The query embedding and the space it was embedded in, `None` when
    /// the semantic arm does not run.
    query_embedding: Option<&'a proxima_core::SpaceVector>,
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
    let (semantic_rows, semantic_rest) = scan_semantic_candidates(ctx, pool, scan).await?;

    // Admit: Query HeadsOnly. Content hits on a superseded t drop.
    // Each semantic slice is its own reciprocal-rank space: the second
    // holds non-source neighbours of an unfiltered hybrid search, and an
    // empty slice adds nothing.
    let fused = fuse_candidates(
        scan.effective_mode,
        scan.semantic_weight,
        &lexical_rows,
        &[&semantic_rows, &semantic_rest],
    );
    if fused.is_empty() {
        return Ok((Vec::new(), HashMap::new()));
    }
    let candidate_ids = fused
        .iter()
        .map(|scores| scores.memory_id)
        .collect::<Vec<_>>();
    let score_by_id = fused
        .into_iter()
        .map(|scores| (scores.memory_id, scores))
        .collect::<HashMap<_, _>>();
    let rows = proxima::flavor::authorized_abstraction_payloads::<CodeChunkV1>(
        engine,
        ctx.authz(),
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
    let gin = scan_chunk_sidecar(pool, scan.resolved.query, &sidecar).await?;
    // The substring arm is DECLARED, not blanket. A schema whose contract
    // says `SubstringArm::Off` contributes no statement and no rows; the
    // price for stopword-only and partial-word queries is then paid per
    // declaration, visibly, instead of being a mechanism nobody can turn
    // off.
    if gin.is_empty() && chunk_substring_arm_is_declared() {
        scan_chunk_sidecar_like(pool, scan.resolved.query, &sidecar).await
    } else {
        Ok(gin)
    }
}

/// The semantic arm draws on the same candidate budget as the lexical one
/// and applies the same structural filters — pushed into the neighbour scan
/// rather than applied to its output, because a search scoped to one
/// repository would otherwise spend its whole budget on whichever repository
/// is largest and come back empty.
///
/// An unfiltered hybrid search runs that scan twice ([`semantic_scan`]):
/// once over source, once over every other class. One distance-ordered
/// limit would fill itself with whichever class sits nearest, and the
/// tier sort that runs after fusion cannot put back a source chunk the
/// scan never returned. Each set keeps the full budget, and fusion ranks
/// each set from its own rank 0.
async fn scan_semantic_candidates(
    ctx: &ToolCtx,
    pool: &crate::CodeFlavorStore,
    scan: &ChunkCandidateScan<'_>,
) -> Result<(Vec<CodeChunkVectorCandidate>, Vec<CodeChunkVectorCandidate>), ToolError> {
    let Some(query) = scan.query_embedding else {
        return Ok((Vec::new(), Vec::new()));
    };
    let limit = usize::try_from(scan.candidate_limit).unwrap_or(0);
    match semantic_scan(scan.resolved) {
        SemanticScan::One(filters) => Ok((
            nearest_chunks(ctx, pool, query, filters, limit).await?,
            Vec::new(),
        )),
        SemanticScan::Split { source, non_source } => {
            // The limit applies inside each scan. One shared call: the two
            // paths differ only in the class predicate.
            let (source, non_source) = tokio::try_join!(
                nearest_chunks(ctx, pool, query, source, limit),
                nearest_chunks(ctx, pool, query, non_source, limit),
            )?;
            Ok((source, non_source))
        }
    }
}

async fn nearest_chunks(
    ctx: &ToolCtx,
    pool: &crate::CodeFlavorStore,
    query: &proxima_core::SpaceVector,
    filters: CodeChunkVectorFilters<'_>,
    limit: usize,
) -> Result<Vec<CodeChunkVectorCandidate>, ToolError> {
    pool.nearest_code_chunk_candidates(
        ctx.authz().owner_scope(),
        ctx.owner(),
        query,
        filters,
        limit,
    )
    .await
}

/// How many neighbour scans an unfiltered hybrid search spends, and with
/// which class predicate.
enum SemanticScan<'a> {
    /// One scan. `semantic` mode, a search that named `file_class`, and a
    /// hybrid search that is not ranking source first.
    One(CodeChunkVectorFilters<'a>),
    /// Source, then every other class. Each filter carries the query's
    /// repo, language and chunk type.
    Split {
        source: CodeChunkVectorFilters<'a>,
        non_source: CodeChunkVectorFilters<'a>,
    },
}

/// The neighbour-scan plan for `resolved`.
///
/// Unfiltered hybrid is the only query that splits. A named class, and
/// `lexical` or `semantic` mode, stay one scan: those rankings do not
/// put source ahead of the other classes, so the class mix inside the
/// budget is the ranking.
fn semantic_scan<'query>(resolved: &ResolvedChunkQuery<'query>) -> SemanticScan<'query> {
    let shared = CodeChunkVectorFilters {
        repo_id: resolved.repo_id,
        language: resolved.language,
        chunk_type: resolved.chunk_type,
        file_class: resolved.file_class.map(FileClass::as_str),
        exclude_source: false,
    };
    if resolved.source_first() {
        SemanticScan::Split {
            source: CodeChunkVectorFilters {
                file_class: Some(FileClass::Source.as_str()),
                ..shared
            },
            non_source: CodeChunkVectorFilters {
                file_class: None,
                exclude_source: true,
                ..shared
            },
        }
    } else {
        SemanticScan::One(shared)
    }
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
///
/// `rows` arrive in fused order. With `source_first` every non-source row
/// moves to tier 1 behind every source row; the sort is stable, so each
/// tier keeps the fused order.
fn select_chunk_page(
    rows: Vec<(MemoryId, CodeChunkV1)>,
    score_by_id: &HashMap<Uuid, MatchScores>,
    after: Option<ChunkCursorPos>,
    page_len: usize,
    fingerprint: &str,
    seen: u32,
    source_first: bool,
) -> ChunkPage {
    let mut eligible = Vec::new();
    for (memory_id, payload) in rows {
        if payload.state != FileState::Present {
            continue;
        }
        let raw_id = memory_id.into_inner();
        let mut scores = score_by_id.get(&raw_id).copied().unwrap_or_default();
        scores.tier = u8::from(source_first && !payload.file_class.is_source());
        if after.is_some_and(|pos| !ranks_after_chunk_cursor(scores, pos)) {
            continue;
        }
        eligible.push((memory_id, payload, scores));
    }
    eligible.sort_by_key(|(_, _, scores)| scores.tier);
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
                tier: scores.tier,
            },
        )
    });
    ChunkPage {
        eligible,
        has_more,
        next_cursor,
    }
}

/// How a page of chunks is rendered: the arguments that shape a match but
/// not which chunks match, so none of them is in the cursor canon.
struct MatchRendering<'a> {
    query: &'a str,
    snippet_max_chars: usize,
    context_lines: Option<u32>,
    verbose: bool,
    /// The search named one repository, which the envelope then carries
    /// instead of every match.
    repo_scoped: bool,
}

/// Render the page into wire matches, and collect the same page's row ids
/// for the call-neighbour phase.
fn render_chunk_matches(
    ctx: &ToolCtx,
    render: &MatchRendering<'_>,
    eligible: Vec<(MemoryId, CodeChunkV1, MatchScores)>,
) -> Result<(Vec<ChunkMatch>, Vec<Uuid>), ToolError> {
    let mut matches = Vec::with_capacity(eligible.len());
    let mut chunk_ids = Vec::with_capacity(eligible.len());
    for (memory_id, payload, scores) in eligible {
        chunk_ids.push(memory_id.into_inner());
        let (match_kind, matched_line, matched_excerpt) = match_metadata(
            render.query,
            &payload.file_path,
            &payload.text,
            payload.line_range_start,
            scores.lexical_hit,
        );
        let (snippet, snippet_truncated) = render_snippet(
            &payload.text,
            payload.line_range_start,
            matched_line.zip(render.context_lines),
            render.snippet_max_chars,
        );
        let detail = if render.verbose {
            Some(ChunkMatchDetail {
                language: payload.language,
                chunk_index: i32::try_from(payload.chunk_index)
                    .map_err(|_| ToolError::Other("chunk_index exceeds i32".into()))?,
                byte_range: (
                    i64::from(payload.byte_range_start),
                    i64::from(payload.byte_range_end),
                ),
                match_kind,
                matched_excerpt,
                lexical_score: scores.lexical_score,
                similarity_score: scores.similarity_score,
            })
        } else {
            None
        };
        matches.push(ChunkMatch {
            handle: ctx.format_abstraction_memory(memory_id),
            repo_handle: (!render.repo_scoped).then(|| format_repo_handle(ctx, payload.repo_id)),
            file_path: payload.file_path,
            chunk_type: payload.chunk_type,
            file_class: (!payload.file_class.is_source()).then_some(payload.file_class),
            line_range: (
                i64::from(payload.line_range_start),
                i64::from(payload.line_range_end),
            ),
            snippet,
            snippet_truncated,
            matched_line,
            score: scores.score,
            detail,
        });
    }
    Ok((matches, chunk_ids))
}

/// A match's snippet and whether it holds less than the whole chunk.
///
/// With a `window` of `(matched_line, context_lines)` the snippet is the
/// lines within `context_lines` of `matched_line`, each prefixed with its
/// file line number, so a caller can cite a line without opening the file.
/// Without one it is the chunk text from its start. `max_chars` caps both.
fn render_snippet(
    text: &str,
    line_range_start: u32,
    window: Option<(i64, u32)>,
    max_chars: usize,
) -> (String, bool) {
    let Some((matched_line, context_lines)) = window else {
        return (
            text.chars().take(max_chars).collect(),
            text.chars().count() > max_chars,
        );
    };
    let line_count = text.lines().count();
    let offset = usize::try_from(matched_line - i64::from(line_range_start)).unwrap_or(0);
    let context = usize::try_from(context_lines).unwrap_or(usize::MAX);
    let first = offset.saturating_sub(context);
    let last = offset
        .saturating_add(context)
        .min(line_count.saturating_sub(1));
    let window_text = text
        .lines()
        .enumerate()
        .take(last + 1)
        .skip(first)
        .map(|(idx, line)| {
            let number = i64::from(line_range_start) + i64::try_from(idx).unwrap_or(i64::MAX);
            format!("{number}: {line}")
        })
        .collect::<Vec<_>>()
        .join("\n");
    let whole_chunk = first == 0 && last + 1 >= line_count;
    let cut = window_text.chars().count() > max_chars;
    let snippet = if cut {
        window_text.chars().take(max_chars).collect()
    } else {
        window_text
    };
    (snippet, cut || !whole_chunk)
}

/// The per-chunk scores a search ranked by, in one place so the ordering
/// and the reported numbers cannot drift apart.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct MatchScores {
    memory_id: uuid::Uuid,
    score: f32,
    lexical_score: f32,
    similarity_score: f32,
    /// The lexical arm returned this chunk, whatever its score.
    lexical_hit: bool,
    /// A semantic slice has already contributed this chunk's reciprocal
    /// rank. A later slice leaves it: the slices partition by class, and
    /// the first one is the rank that was asked for.
    semantic_ranked: bool,
    /// Ranks before `score`: 1 for a non-source chunk in a search that puts
    /// source first, else 0. Set once the payload is read.
    tier: u8,
}

/// Resolve the requested mode against what the current Owner's embedding
/// route can do, returning the mode that will run and the query embedding
/// if one is needed. The query is embedded by the current Owner's client
/// only, so the semantic arm ranks the read set's chunks stored in that
/// Owner's space; chunks another route embedded are reachable lexically.
///
/// The three outcomes deliberately match `core_search_memories`, because a
/// caller should not have to learn two rules: `lexical` never asks for an
/// embedding; `hybrid` degrades to `lexical` and reports it; `semantic`
/// fails, because it has no other arm to fall back on and answering
/// lexically would answer a question the caller did not ask.
async fn resolve_query_embedding(
    engine: &proxima_core::Engine,
    owner: &proxima_core::Owner,
    mode: ChunkSearchMode,
    query: &str,
) -> Result<QueryEmbedding, ToolError> {
    if mode == ChunkSearchMode::Lexical {
        return Ok(QueryEmbedding::lexical());
    }
    let route = engine.search_route(owner).await;
    let Some(embed) = route.current_client() else {
        if mode == ChunkSearchMode::Semantic {
            return Err(ToolError::Unavailable(
                SEMANTIC_CHUNK_SEARCH_UNAVAILABLE.to_string(),
            ));
        }
        return Ok(QueryEmbedding::lexical());
    };
    match embed.embed_query(query, CODE_QUERY_TASK).await {
        Ok(vector) => Ok(QueryEmbedding {
            mode,
            vector: Some(vector),
            route_weight: embed.semantic_weight(CODE_QUERY_TASK),
        }),
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
            Ok(QueryEmbedding::lexical())
        }
    }
}

/// What the current Owner's route makes of a chunk search.
struct QueryEmbedding {
    /// The mode that will run.
    mode: ChunkSearchMode,
    /// The query embedding, `None` when the semantic arm does not run.
    vector: Option<proxima_core::SpaceVector>,
    /// The route's hybrid weight for code search, if it sets one.
    route_weight: Option<SemanticWeight>,
}

impl QueryEmbedding {
    const fn lexical() -> Self {
        Self {
            mode: ChunkSearchMode::Lexical,
            vector: None,
            route_weight: None,
        }
    }
}

/// The caller's `semantic_weight`, refused outside `0.0..=1.0` and outside
/// `mode=hybrid`, as `core_search_memories` does: a knob only hybrid fusion
/// reads would otherwise look like a ranking that ignored the caller.
fn requested_semantic_weight(
    weight: Option<f32>,
    mode: ChunkSearchMode,
) -> Result<Option<SemanticWeight>, ToolError> {
    let Some(weight) = weight else {
        return Ok(None);
    };
    let weight =
        SemanticWeight::new(weight).map_err(|err| ToolError::InvalidInput(err.to_string()))?;
    if mode != ChunkSearchMode::Hybrid {
        return Err(ToolError::InvalidInput(
            "semantic_weight applies only to mode=hybrid".into(),
        ));
    }
    Ok(Some(weight))
}

/// The [`QueryTask`](proxima_core::llm::QueryTask) chunk search embeds its
/// query under, so a route can word code queries apart from memory queries.
pub const CODE_QUERY_TASK: proxima_core::llm::QueryTask =
    proxima_core::llm::QueryTask::named("code");

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
/// `Hybrid` sums each arm's reciprocal rank, weighted `2(1 - w)` for the
/// lexical arm and `2w` for the semantic one, and adds the literal bonus on
/// top. At the even weight both factors are exactly 1, which is plain rank
/// fusion. The bonus is 4.0 at the smallest and a weighted reciprocal rank
/// sum is at most `2/61`, so a chunk whose path or text literally contains
/// the query outranks every chunk that merely resembles it, at any weight
/// and however strong the resemblance. That is the one place where a caller
/// has said exactly what they want, and rank fusion on its own would let a
/// confident embedding neighbour bury it.
///
/// `semantic_arms` are separate rank spaces. An unfiltered hybrid search
/// passes the source neighbours and the other-class neighbours as two
/// slices, so a non-source chunk at rank 0 of its own scan scores as a
/// rank-0 neighbour and does not push a source neighbour down the source
/// scan. A chunk that appears in more than one slice keeps the first
/// slice's rank.
fn fuse_candidates(
    mode: ChunkSearchMode,
    weight: SemanticWeight,
    lexical: &[ChunkCandidateRow],
    semantic_arms: &[&[CodeChunkVectorCandidate]],
) -> Vec<MatchScores> {
    let semantic_share = 2.0 * weight.get();
    let lexical_share = 2.0 - semantic_share;
    let semantic_len: usize = semantic_arms.iter().map(|arm| arm.len()).sum();
    let mut by_id: HashMap<uuid::Uuid, MatchScores> =
        HashMap::with_capacity(lexical.len() + semantic_len);
    for (rank, row) in lexical.iter().enumerate() {
        let entry = by_id.entry(row.memory_id).or_insert(MatchScores {
            memory_id: row.memory_id,
            ..MatchScores::default()
        });
        entry.lexical_score = row.score;
        entry.lexical_hit = true;
        entry.score = if mode == ChunkSearchMode::Hybrid {
            entry.score + row.literal_bonus + lexical_share * reciprocal_rank(rank)
        } else {
            row.score
        };
    }
    for arm in semantic_arms {
        for (rank, row) in arm.iter().enumerate() {
            let entry = by_id.entry(row.memory_id).or_insert(MatchScores {
                memory_id: row.memory_id,
                ..MatchScores::default()
            });
            if entry.semantic_ranked {
                continue;
            }
            entry.semantic_ranked = true;
            entry.similarity_score = row.similarity_score;
            entry.score = if mode == ChunkSearchMode::Hybrid {
                entry.score + semantic_share * reciprocal_rank(rank)
            } else {
                row.similarity_score
            };
        }
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
/// (`tier ASC, score DESC, memory_id DESC`).
fn ranks_after_chunk_cursor(scores: MatchScores, pos: ChunkCursorPos) -> bool {
    let score = f32::from_bits(pos.score_bits);
    scores
        .tier
        .cmp(&pos.tier)
        .then_with(|| score.total_cmp(&scores.score))
        .then_with(|| pos.memory_id.cmp(&scores.memory_id))
        .is_gt()
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
    no_matches: bool,
    semantic_reached: bool,
) -> bool {
    if requested != ChunkSearchMode::Hybrid {
        return false;
    }
    effective == ChunkSearchMode::Lexical
        || proxima::flavor::hybrid_degraded_to_lexical(
            proxima::flavor::SearchMode::Hybrid,
            no_matches,
            semantic_reached,
        )
}

const fn mode_label(mode: ChunkSearchMode) -> &'static str {
    match mode {
        ChunkSearchMode::Lexical => "lexical",
        ChunkSearchMode::Semantic => "semantic",
        ChunkSearchMode::Hybrid => "hybrid",
    }
}

/// How a match was made, the file line it points at, and that line's text.
///
/// A line holding the whole query wins. Otherwise a chunk the lexical arm
/// found (`lexical_hit`) points at its first line sharing the most query
/// words, the line most likely to be why full text matched; a chunk only
/// the semantic arm found has no such line and keeps no pointer.
fn match_metadata(
    query: &str,
    file_path: &str,
    text: &str,
    line_range_start: u32,
    lexical_hit: bool,
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

    let pointed = |idx: usize, line: &str| {
        (
            i64::try_from(idx)
                .ok()
                .map(|offset| i64::from(line_range_start) + offset),
            Some(line.trim().chars().take(480).collect()),
        )
    };
    for (idx, line) in text.lines().enumerate() {
        if line.to_ascii_lowercase().contains(&query_lower) {
            let (matched_line, excerpt) = pointed(idx, line);
            return ("text_contains".to_string(), matched_line, excerpt);
        }
    }
    if lexical_hit && let Some((idx, line)) = line_sharing_most_query_words(query, text) {
        let (matched_line, excerpt) = pointed(idx, line);
        return ("full_text".to_string(), matched_line, excerpt);
    }

    ("full_text".to_string(), None, None)
}

/// Question words that point at no particular line of code.
const POINTER_STOPWORDS: &[&str] = &[
    "about", "after", "all", "and", "any", "are", "been", "before", "being", "but", "can", "code",
    "does", "done", "each", "for", "from", "get", "gets", "happen", "happens", "has", "have",
    "how", "into", "its", "not", "our", "the", "their", "then", "there", "these", "this", "those",
    "was", "were", "what", "when", "where", "which", "while", "who", "why", "will", "with",
    "would",
];

/// Most query words a pointer compares, bounding the per-line work.
const MAX_POINTER_WORDS: usize = 16;

/// The words of `text`, lowercased, split at non-alphanumerics and at
/// camel-case humps, so `getModuleScriptSources` and `module_script` both
/// yield `module` and `script`.
fn pointer_words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric())
        .flat_map(|token| {
            let chars: Vec<char> = token.chars().collect();
            let mut words = Vec::new();
            let mut start = 0;
            for i in 1..chars.len() {
                let hump = chars[i].is_uppercase()
                    && (chars[i - 1].is_lowercase()
                        || chars.get(i + 1).is_some_and(|next| next.is_lowercase()));
                if hump {
                    words.push(chars[start..i].iter().collect::<String>());
                    start = i;
                }
            }
            words.push(chars[start..].iter().collect::<String>());
            words
        })
        .filter(|word| !word.is_empty())
        .map(|word| word.to_lowercase())
}

/// Whether two lowercased words are the same word up to its ending:
/// `retries`/`retry`, `resolved`/`resolution`, `chunk`/`chunker`. Words
/// under four characters must be equal. An approximation of the stemmer
/// the lexical arm ranks with, good enough to point at a line.
fn same_word(a: &str, b: &str) -> bool {
    let shorter = a.chars().count().min(b.chars().count());
    if shorter < 4 {
        return a == b;
    }
    let common = a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count();
    common >= 4 && common * 5 >= shorter * 3
}

/// The first line of `text` sharing the most distinct query words with the
/// query, with its index, or `None` when no line shares one.
fn line_sharing_most_query_words<'t>(query: &str, text: &'t str) -> Option<(usize, &'t str)> {
    let mut terms: Vec<String> = Vec::new();
    for word in pointer_words(query) {
        if word.chars().count() >= 3
            && !POINTER_STOPWORDS.contains(&word.as_str())
            && !terms.contains(&word)
        {
            terms.push(word);
            if terms.len() == MAX_POINTER_WORDS {
                break;
            }
        }
    }
    if terms.is_empty() {
        return None;
    }
    let mut best: Option<(usize, usize, &str)> = None;
    for (idx, line) in text.lines().enumerate() {
        let words: Vec<String> = pointer_words(line).collect();
        let shared = terms
            .iter()
            .filter(|term| words.iter().any(|word| same_word(term, word)))
            .count();
        if shared > best.map_or(0, |(count, _, _)| count) {
            best = Some((shared, idx, line));
        }
    }
    best.map(|(_, idx, line)| (idx, line))
}

async fn load_call_edges(
    ctx: &ToolCtx,
    engine: &proxima_core::Engine,
    chunk_ids: &[uuid::Uuid],
) -> Result<Vec<CallEdge>, ToolError> {
    let pool = code_store(ctx)?;
    // The index names a callee by series handle, never by `t`: ingest names
    // every callee of a file before any of its chunks is written. The head
    // join that turns the handle into the `t` a match carries is backend
    // SQL, and it drops rows a superseded caller revision left behind.
    let pairs = pool
        .head_chunk_call_pairs(pool.owner_scope(), chunk_ids, MAX_CALL_EDGES)
        .await?;
    if pairs.is_empty() {
        return Ok(Vec::new());
    }
    // Hydrate the sites from the caller chunks' payload rows. The index
    // answers "is there a connection"; this is the node answering "what is
    // it", and it is one query for the whole page.
    let sources = pairs.iter().map(|pair| pair.caller_t).collect::<Vec<_>>();
    let callees = pairs
        .iter()
        .map(|pair| pair.callee_handle)
        .collect::<Vec<_>>();
    let mut tx = begin_compatible_owner_transaction(pool.pool(), pool.owner_scope())
        .await
        .map_err(ToolError::Storage)?;
    let site_rows: Vec<CallSiteRow> = sqlx::query_as(
        "SELECT caller_memory_id, callee_memory_id, callee_name, is_dynamic,
                byte_start, byte_end
           FROM proxima_code.code_chunk_call_v1
          WHERE caller_memory_id = ANY($1::uuid[])
            AND callee_memory_id = ANY($2::uuid[])
          ORDER BY caller_memory_id, callee_memory_id, site_index",
    )
    .bind(&sources)
    .bind(&callees)
    .fetch_all(&mut *tx)
    .await
    .map_err(map_storage)?;
    tx.commit().await.map_err(map_storage)?;
    let endpoints = pairs
        .iter()
        .flat_map(|pair| std::iter::once(pair.caller_t).chain(pair.callee_t))
        .collect::<Vec<_>>();
    let readable = readable_call_endpoints(ctx, engine, &endpoints).await?;
    let shown = |id: uuid::Uuid| {
        let id = MemoryId::new(id);
        readable
            .contains(&id)
            .then(|| ctx.format_abstraction_memory(id))
    };
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
        .map(|pair| {
            let pair_sites = sites
                .remove(&(pair.caller_t, pair.callee_handle))
                .unwrap_or_default();
            let source = shown(pair.caller_t);
            CallEdge {
                sites: if source.is_some() {
                    pair_sites
                } else {
                    Vec::new()
                },
                source,
                target: pair.callee_t.and_then(shown),
            }
        })
        .collect())
}

/// The call endpoints this caller may read.
///
/// The index row belongs to the caller chunk's Owner; the callee it names
/// does not. Both endpoints pass the same read check as any other memory, so
/// an unreadable one comes back `null`, id and all.
async fn readable_call_endpoints(
    ctx: &ToolCtx,
    engine: &proxima_core::Engine,
    endpoints: &[uuid::Uuid],
) -> Result<HashSet<MemoryId>, ToolError> {
    Ok(proxima::flavor::authorized_memory_ids(
        engine,
        ctx.authz(),
        endpoints,
        proxima_core::EntityKind::Abstraction,
        Some(<CodeChunkV1 as proxima_core::AbstractionPayload>::schema_id()),
        endpoints.len(),
    )
    .await?
    .into_iter()
    .collect())
}

struct ChunkSidecarScan<'a> {
    repo_id: Option<uuid::Uuid>,
    language: Option<&'a str>,
    chunk_type: Option<&'a str>,
    file_class: Option<&'static str>,
    /// Order source chunks ahead of the rest before the candidate cut, so
    /// a vendored tree full of hits cannot fill the budget ahead of them.
    source_first: bool,
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
    pool: &crate::CodeFlavorStore,
    query: &str,
    scan: &ChunkSidecarScan<'_>,
) -> Result<Vec<ChunkCandidateRow>, ToolError> {
    let mut tx = begin_compatible_owner_transaction(pool.pool(), pool.owner_scope())
        .await
        .map_err(ToolError::Storage)?;
    // SQL-POLICY: fixed-fragment
    let rows = sqlx::query_as(sqlx::AssertSqlSafe(CHUNK_GIN_SQL.as_str()))
        .bind(query)
        .bind(scan.repo_id)
        .bind(scan.language)
        .bind(scan.exact_pattern)
        .bind(scan.chunk_type)
        .bind(scan.candidate_limit)
        .bind(scan.distinctive)
        .bind(scan.read_owner_ids)
        .bind(scan.file_class)
        .bind(scan.source_first)
        .fetch_all(&mut *tx)
        .await
        .map_err(map_storage)?;
    tx.commit().await.map_err(map_storage)?;
    Ok(rows)
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
            AND ($9::text IS NULL OR COALESCE(c.file_class::text, 'source') = $9)
            AND (
                p.search_tsv @@ q.tsq
                OR (q.any_tsq IS NOT NULL AND p.search_tsv @@ q.any_tsq)
                OR (q.rare_any_tsq IS NOT NULL AND p.search_tsv @@ q.rare_any_tsq)
            )
          ORDER BY ($10::bool AND COALESCE(c.file_class::text, 'source') <> 'source'),
                   score DESC, c.t DESC
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
            AND ($8::text IS NULL OR COALESCE(c.file_class::text, 'source') = $8)
            AND (
                lower(c.file_path) = lower($1)
                OR lower(c.file_path) LIKE $4 ESCAPE '\\'
                OR lower(c.text) LIKE $4 ESCAPE '\\'
            )
          ORDER BY ($9::bool AND COALESCE(c.file_class::text, 'source') <> 'source'),
                   score DESC, c.t DESC
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
    pool: &crate::CodeFlavorStore,
    query: &str,
    scan: &ChunkSidecarScan<'_>,
) -> Result<Vec<ChunkCandidateRow>, ToolError> {
    let mut tx = begin_compatible_owner_transaction(pool.pool(), pool.owner_scope())
        .await
        .map_err(ToolError::Storage)?;
    // SQL-POLICY: fixed-fragment
    let rows = sqlx::query_as(sqlx::AssertSqlSafe(CHUNK_LIKE_SQL.as_str()))
        .bind(query)
        .bind(scan.repo_id)
        .bind(scan.language)
        .bind(scan.exact_pattern)
        .bind(scan.chunk_type)
        .bind(scan.candidate_limit)
        .bind(scan.read_owner_ids)
        .bind(scan.file_class)
        .bind(scan.source_first)
        .fetch_all(&mut *tx)
        .await
        .map_err(map_storage)?;
    tx.commit().await.map_err(map_storage)?;
    Ok(rows)
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
mod tests;

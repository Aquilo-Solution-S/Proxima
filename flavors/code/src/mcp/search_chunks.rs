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

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CodeSearchChunksArgs {
    #[schemars(
        description = "Query string for code chunk search, matched against file paths and chunk text. Takes an identifier or path for exact lookup, or a plain-English question — chunks sharing any content word are returned when none share all of them. 1 to 512 chars."
    )]
    pub query: String,
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
    pub score: f32,
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
    const DESCRIPTION: &'static str = "Search head code chunks by exact substring, path, or full-text content, including plain-English questions. Each match carries its chunk text up to snippet_max_chars, flagged snippet_truncated when cut. Supports language/chunk_type filters and optional proxima-code/calls neighbor edges.";

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
            let rows: Vec<ChunkCandidateRow> = sqlx::query_as(
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
                        )::real AS score
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
            .map_err(map_storage)?;
            let candidate_ids = rows.iter().map(|row| row.memory_id).collect::<Vec<_>>();
            let score_by_id = rows
                .into_iter()
                .map(|row| (row.memory_id, row.score))
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
                    score: score_by_id.get(&raw_id).copied().unwrap_or_default(),
                });
            }

            let calls_edges = if args.include_calls && !chunk_ids.is_empty() {
                load_call_edges(&ctx, &chunk_ids).await?
            } else {
                Vec::new()
            };

            Ok(CodeSearchChunksOutput {
                matches,
                calls_edges,
                has_more,
            })
        })
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
}

#[derive(Debug, sqlx::FromRow)]
struct CallPayloadRow {
    edge_id: uuid::Uuid,
    callee_name: String,
    is_dynamic: bool,
}

#[cfg(test)]
mod tests {
    use super::{MAX_DISTINCTIVE_TERMS, distinctive_terms, like_pattern};

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

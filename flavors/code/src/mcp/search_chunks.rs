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
        description = "Lexical query string for code chunk search. Matches file paths and chunk text; 1 to 512 chars."
    )]
    pub query: String,
    #[schemars(
        description = "Optional maximum number of chunk matches. Omit or null for 12; values above 50 are clamped."
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
}

const fn default_include_calls() -> bool {
    true
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
    const DESCRIPTION: &'static str = "Search head code chunks by exact substring, path, or full-text content. Supports language/chunk_type filters and optional proxima-code/calls neighbor edges.";

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
            // `to_tsvector('simple', file_path || ' ' || text)`. Reading it
            // replaces two per-row `to_tsvector` evaluations — one for the
            // predicate, one for the rank — with a column read, and the GIN
            // index now sits on the column rather than on the expression.
            // `code_chunk_search_tsv_matches_the_scoring_expression` pins the
            // column against the expression it replaced.
            let rows: Vec<ChunkCandidateRow> = sqlx::query_as(
                "WITH q AS (SELECT websearch_to_tsquery('pg_catalog.simple'::regconfig, $1) AS tsq)
                 SELECT c.memory_id,
                        (
                            ts_rank_cd(c.search_tsv, q.tsq)
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
                    snippet: payload.text.chars().take(480).collect(),
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

    let mut edges = HashMap::new();
    for chunk_id in chunk_ids {
        for filter in [
            EdgeFilter {
                relation: Some("proxima-code/calls".to_string()),
                source: Some(EntityRef::Memory(MemoryId::new(*chunk_id))),
                target: None,
            },
            EdgeFilter {
                relation: Some("proxima-code/calls".to_string()),
                source: None,
                target: Some(EntityRef::Memory(MemoryId::new(*chunk_id))),
            },
        ] {
            let response = engine
                .read_edges(
                    ctx.authz(),
                    &EdgeReadRequest {
                        owner: ctx.owner(),
                        edge_ids: Vec::new(),
                        filter,
                        limit: 200,
                        cursor: None,
                        include_payloads: false,
                    },
                )
                .await?;
            for edge in response.edges {
                edges.entry(edge.id).or_insert(edge);
            }
        }
    }

    let edge_ids = edges.keys().copied().collect::<Vec<_>>();
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
    for edge in edges.into_values().take(200) {
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

fn like_pattern(query: &str) -> String {
    let mut out = String::with_capacity(query.len() + 2);
    out.push('%');
    for ch in query.to_ascii_lowercase().chars() {
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

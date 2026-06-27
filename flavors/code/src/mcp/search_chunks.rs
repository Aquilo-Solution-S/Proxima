use proxima_core::mcp::{McpTool, McpToolCtx, McpToolError};
use proxima_core::{EdgeId, MemoryId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::pg_pool;
use super::sql::{CHUNK_HEADS_CTE, map_storage, owner_principal, resolve_repo_identifier};

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

impl McpTool for CodeSearchChunksTool {
    const NAME: &'static str = "proxima-code_search_chunks";
    const DESCRIPTION: &'static str = "Search head code chunks by exact substring, path, or full-text content. Supports language/chunk_type filters and optional proxima-code/calls neighbor edges.";

    type Args = CodeSearchChunksArgs;
    type Output = CodeSearchChunksOutput;

    fn call(
        ctx: McpToolCtx,
        args: CodeSearchChunksArgs,
    ) -> futures::future::BoxFuture<'static, Result<CodeSearchChunksOutput, McpToolError>> {
        Box::pin(async move {
            let query = args.query.trim();
            if query.is_empty() || query.chars().count() > 512 {
                return Err(McpToolError::InvalidInput(
                    "query must be 1..=512 chars".into(),
                ));
            }
            let limit = args.limit.unwrap_or(12).min(50);
            let (owner_kind, owner_principal_id) = owner_principal(&ctx.owner);
            let exact_pattern = like_pattern(query);
            let repo_id = match args.repo_handle.as_deref() {
                Some(handle) => Some(resolve_repo_identifier(&ctx, handle).await?),
                None => None,
            };
            let pool = pg_pool(&ctx)?;

            let sql = format!(
                "WITH {CHUNK_HEADS_CTE}, q AS (SELECT websearch_to_tsquery('pg_catalog.simple'::regconfig, $3) AS tsq)
                 SELECT memory_id, repo_id, file_path, chunk_index, language,
                        chunk_type, line_range_start, line_range_end,
                        byte_range_start, byte_range_end,
                        text,
                        left(text, 480) AS snippet,
                        (
                            ts_rank_cd(to_tsvector('pg_catalog.simple'::regconfig, file_path || ' ' || text), q.tsq)
                            + CASE WHEN lower(file_path) = lower($3) THEN 10.0 ELSE 0.0 END
                            + CASE WHEN lower(file_path) LIKE $6 ESCAPE '\\' THEN 6.0 ELSE 0.0 END
                            + CASE WHEN lower(text) LIKE $6 ESCAPE '\\' THEN 4.0 ELSE 0.0 END
                        )::real AS score
                 FROM chunk_heads, q
                 WHERE ($4::uuid IS NULL OR repo_id = $4)
                   AND ($5::text IS NULL OR language = $5)
                   AND ($7::text IS NULL OR chunk_type = $7)
                   AND (
                       to_tsvector('pg_catalog.simple'::regconfig, file_path || ' ' || text) @@ q.tsq
                       OR lower(file_path) LIKE $6 ESCAPE '\\'
                       OR lower(text) LIKE $6 ESCAPE '\\'
                   )
                 ORDER BY score DESC, memory_id DESC
                 LIMIT $8"
            );
            let rows: Vec<ChunkRow> = sqlx::query_as(&sql)
                .bind(owner_kind)
                .bind(owner_principal_id)
                .bind(query)
                .bind(repo_id)
                .bind(args.language.as_deref())
                .bind(exact_pattern)
                .bind(args.chunk_type.as_deref())
                .bind(i64::from(limit))
                .fetch_all(pool.as_ref())
                .await
                .map_err(map_storage)?;

            let mut matches = Vec::with_capacity(rows.len());
            let mut chunk_ids = Vec::with_capacity(rows.len());
            for row in rows {
                chunk_ids.push(row.memory_id);
                let (match_kind, matched_line, matched_excerpt) =
                    match_metadata(query, &row.file_path, &row.text, row.line_range_start);
                matches.push(ChunkMatch {
                    handle: ctx.format_abstraction_memory(MemoryId::new(row.memory_id)),
                    repo_handle: ctx.format_flavor_object(
                        super::REPO_HANDLE_KIND,
                        row.repo_id,
                        super::REPO_HANDLE_PREFIX,
                    ),
                    file_path: row.file_path,
                    chunk_index: row.chunk_index,
                    language: row.language,
                    chunk_type: row.chunk_type,
                    line_range: (row.line_range_start, row.line_range_end),
                    byte_range: (row.byte_range_start, row.byte_range_end),
                    snippet: row.snippet,
                    match_kind,
                    matched_line,
                    matched_excerpt,
                    score: row.score,
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
            })
        })
    }
}

fn match_metadata(
    query: &str,
    file_path: &str,
    text: &str,
    line_range_start: i64,
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
                    .map(|offset| line_range_start + offset),
                Some(line.trim().chars().take(480).collect()),
            );
        }
    }

    ("full_text".to_string(), None, None)
}

async fn load_call_edges(
    ctx: &McpToolCtx,
    chunk_ids: &[uuid::Uuid],
) -> Result<Vec<CallEdge>, McpToolError> {
    let pool = pg_pool(ctx)?;
    let rows: Vec<CallEdgeRow> = sqlx::query_as(
        "SELECT e.edge_id, e.source_memory_id, e.target_memory_id,
                c.callee_name, c.is_dynamic
         FROM proxima_core.edges e
         JOIN proxima_code.code_calls_v1 c USING (edge_id)
         WHERE e.relation = 'proxima-code/calls'
           AND (e.source_memory_id = ANY($1::uuid[]) OR e.target_memory_id = ANY($1::uuid[]))
         ORDER BY e.edge_id DESC
         LIMIT 200",
    )
    .bind(chunk_ids)
    .fetch_all(pool.as_ref())
    .await
    .map_err(map_storage)?;

    Ok(rows
        .into_iter()
        .map(|row| CallEdge {
            edge_handle: ctx.format_edge(EdgeId::new(row.edge_id)),
            source: row
                .source_memory_id
                .map(|id| ctx.format_abstraction_memory(MemoryId::new(id))),
            target: row
                .target_memory_id
                .map(|id| ctx.format_abstraction_memory(MemoryId::new(id))),
            callee_name: row.callee_name,
            is_dynamic: row.is_dynamic,
        })
        .collect())
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
struct ChunkRow {
    memory_id: uuid::Uuid,
    repo_id: uuid::Uuid,
    file_path: String,
    chunk_index: i32,
    language: Option<String>,
    chunk_type: String,
    line_range_start: i64,
    line_range_end: i64,
    byte_range_start: i64,
    byte_range_end: i64,
    text: String,
    snippet: String,
    score: f32,
}

#[derive(Debug, sqlx::FromRow)]
struct CallEdgeRow {
    edge_id: uuid::Uuid,
    source_memory_id: Option<uuid::Uuid>,
    target_memory_id: Option<uuid::Uuid>,
    callee_name: String,
    is_dynamic: bool,
}

use proxima_core::mcp::{McpTool, McpToolCtx, McpToolError};
use proxima_core::{EdgeId, MemoryId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::sql::{CHUNK_HEADS_CTE, map_storage, owner_principal};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CodeSearchChunksArgs {
    pub query: String,
    pub limit: Option<u32>,
    pub repo_id: Option<uuid::Uuid>,
    pub language: Option<String>,
    pub chunk_type: Option<String>,
    #[serde(default = "default_include_calls")]
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
    pub uuid: uuid::Uuid,
    pub repo_id: uuid::Uuid,
    pub file_path: String,
    pub chunk_index: i32,
    pub language: Option<String>,
    pub chunk_type: String,
    pub line_range: (i64, i64),
    pub byte_range: (i64, i64),
    pub snippet: String,
    pub score: f32,
}

#[derive(Debug, Serialize)]
pub struct CallEdge {
    pub edge_handle: String,
    pub edge_uuid: uuid::Uuid,
    pub source: Option<String>,
    pub target: Option<String>,
    pub callee_name: String,
    pub is_dynamic: bool,
}

#[derive(Debug)]
pub struct CodeSearchChunksTool;

impl McpTool for CodeSearchChunksTool {
    const NAME: &'static str = "proxima-code/code_search_chunks";
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

            let sql = format!(
                "WITH {CHUNK_HEADS_CTE}, q AS (SELECT websearch_to_tsquery('pg_catalog.simple'::regconfig, $3) AS tsq)
                 SELECT memory_id, repo_id, file_path, chunk_index, language,
                        chunk_type, line_range_start, line_range_end,
                        byte_range_start, byte_range_end,
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
                .bind(args.repo_id)
                .bind(args.language.as_deref())
                .bind(exact_pattern)
                .bind(args.chunk_type.as_deref())
                .bind(i64::from(limit))
                .fetch_all(&ctx.pool)
                .await
                .map_err(map_storage)?;

            let mut matches = Vec::with_capacity(rows.len());
            let mut chunk_ids = Vec::with_capacity(rows.len());
            for row in rows {
                let handle = ctx.handles.assign_memory(MemoryId::new(row.memory_id));
                chunk_ids.push(row.memory_id);
                matches.push(ChunkMatch {
                    handle: handle.as_str().to_string(),
                    uuid: row.memory_id,
                    repo_id: row.repo_id,
                    file_path: row.file_path,
                    chunk_index: row.chunk_index,
                    language: row.language,
                    chunk_type: row.chunk_type,
                    line_range: (row.line_range_start, row.line_range_end),
                    byte_range: (row.byte_range_start, row.byte_range_end),
                    snippet: row.snippet,
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

async fn load_call_edges(
    ctx: &McpToolCtx,
    chunk_ids: &[uuid::Uuid],
) -> Result<Vec<CallEdge>, McpToolError> {
    let (owner_kind, owner_principal_id) = owner_principal(&ctx.owner);
    let rows: Vec<CallEdgeRow> = sqlx::query_as(
        "SELECT e.edge_id, e.source_memory_id, e.target_memory_id,
                c.callee_name, c.is_dynamic
         FROM proxima_core.edges e
         JOIN proxima_code.code_calls_v1 c USING (edge_id)
         WHERE e.owner_principal_kind = $1
           AND e.owner_principal_id   = $2
           AND e.relation = 'proxima-code/calls'
           AND (e.source_memory_id = ANY($3) OR e.target_memory_id = ANY($3))
         ORDER BY e.edge_id DESC
         LIMIT 200",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(chunk_ids)
    .fetch_all(&ctx.pool)
    .await
    .map_err(map_storage)?;

    Ok(rows
        .into_iter()
        .map(|row| {
            let edge_handle = ctx.handles.assign_edge(EdgeId::new(row.edge_id));
            CallEdge {
                edge_handle: edge_handle.as_str().to_string(),
                edge_uuid: row.edge_id,
                source: row.source_memory_id.map(|id| {
                    ctx.handles
                        .assign_memory(MemoryId::new(id))
                        .as_str()
                        .to_string()
                }),
                target: row.target_memory_id.map(|id| {
                    ctx.handles
                        .assign_memory(MemoryId::new(id))
                        .as_str()
                        .to_string()
                }),
                callee_name: row.callee_name,
                is_dynamic: row.is_dynamic,
            }
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

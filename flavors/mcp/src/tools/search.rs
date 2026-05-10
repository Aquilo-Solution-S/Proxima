use proxima_core::mcp::{EntityRef, HandleTable, McpTool, McpToolCtx, McpToolError};
use proxima_core::{EdgeId, MemoryId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::util::{map_storage, memory_kind_for_edge, owner_principal};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchGraphArgs {
    pub query: String,
    pub limit: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct SearchGraphOutput {
    pub matches: Vec<GraphMatch>,
    pub neighbor_edges: Vec<NeighborEdge>,
}

#[derive(Debug, Serialize)]
pub struct GraphMatch {
    pub handle: String,
    pub kind: String,
    pub schema_id: String,
    pub title: String,
    pub snippet: String,
    pub score: f32,
    pub tags: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct NeighborEdge {
    pub handle: String,
    pub relation: String,
    pub source: Option<String>,
    pub target: Option<String>,
}

#[derive(Debug)]
pub struct SearchGraphTool;

impl McpTool for SearchGraphTool {
    const NAME: &'static str = "proxima-mcp/proxima_search_graph";
    const DESCRIPTION: &'static str =
        "Search agent-authored notes and derivations. Returns session handles.";
    type Args = SearchGraphArgs;
    type Output = SearchGraphOutput;

    fn call(
        ctx: McpToolCtx,
        args: SearchGraphArgs,
    ) -> futures::future::BoxFuture<'static, Result<SearchGraphOutput, McpToolError>> {
        Box::pin(async move {
            let query = args.query.trim();
            if query.is_empty() || query.chars().count() > 512 {
                return Err(McpToolError::InvalidInput(
                    "query must be 1..=512 chars".into(),
                ));
            }
            let limit = args.limit.unwrap_or(12).min(50);
            let (owner_kind, owner_principal_id) = owner_principal(&ctx.owner);
            let rows: Vec<SearchRow> = sqlx::query_as(SEARCH_SQL)
                .bind(owner_kind)
                .bind(owner_principal_id)
                .bind(query)
                .bind(i64::from(limit))
                .fetch_all(&ctx.pool)
                .await
                .map_err(map_storage)?;

            let mut matches = Vec::with_capacity(rows.len());
            let mut memory_ids = Vec::with_capacity(rows.len());
            for row in rows {
                let handle = ctx.handles.assign_memory(MemoryId::new(row.memory_id));
                memory_ids.push(row.memory_id);
                matches.push(GraphMatch {
                    handle: handle.as_str().to_string(),
                    kind: row.kind,
                    schema_id: row.schema_id,
                    title: row.title,
                    snippet: row.snippet,
                    score: row.score,
                    tags: row.tags,
                });
            }

            let neighbor_edges =
                neighbor_edges(&ctx.pool, &ctx.owner, &ctx.handles, &memory_ids).await?;
            Ok(SearchGraphOutput {
                matches,
                neighbor_edges,
            })
        })
    }
}

const SEARCH_SQL: &str = r"
WITH q AS (SELECT websearch_to_tsquery('simple', $3) AS tsq)
SELECT *
FROM (
    SELECT m.memory_id,
           'Fact' AS kind,
           m.schema_id,
           a.title,
           left(a.body, 480) AS snippet,
           ts_rank_cd(to_tsvector('simple', a.title || ' ' || a.body), q.tsq) AS score,
           a.tags
    FROM q, proxima_core.memories m
    JOIN proxima_mcp.agent_note_v1 a USING (memory_id)
    WHERE m.owner_principal_kind = $1
      AND m.owner_principal_id = $2
      AND m.kind IS NULL
      AND to_tsvector('simple', a.title || ' ' || a.body) @@ q.tsq
    UNION ALL
    SELECT m.memory_id,
           m.kind,
           m.schema_id,
           d.title,
           left(d.body, 480),
           ts_rank_cd(to_tsvector('simple', d.title || ' ' || d.body), q.tsq),
           d.tags
    FROM q, proxima_core.memories m
    JOIN proxima_mcp.agent_derivation_v1 d USING (memory_id)
    WHERE m.owner_principal_kind = $1
      AND m.owner_principal_id = $2
      AND m.kind IN ('Abstraction', 'Perspective')
      AND to_tsvector('simple', d.title || ' ' || d.body) @@ q.tsq
) ranked
ORDER BY score DESC, memory_id DESC
LIMIT $4
";

#[derive(Debug, sqlx::FromRow)]
struct SearchRow {
    memory_id: uuid::Uuid,
    kind: String,
    schema_id: String,
    title: String,
    snippet: String,
    score: f32,
    tags: Vec<String>,
}

/// # Errors
///
/// Returns storage errors from the owner-filtered edge query.
pub async fn neighbor_edges(
    pool: &sqlx::PgPool,
    owner: &proxima_core::Owner,
    handles: &HandleTable,
    memory_ids: &[uuid::Uuid],
) -> Result<Vec<NeighborEdge>, McpToolError> {
    if memory_ids.is_empty() {
        return Ok(Vec::new());
    }
    let (owner_kind, owner_principal_id) = owner_principal(owner);
    let rows: Vec<EdgeRow> = sqlx::query_as(
        "SELECT edge_id, relation, source_memory_id, target_memory_id
         FROM proxima_core.edges
         WHERE owner_principal_kind = $1
           AND owner_principal_id = $2
           AND (source_memory_id = ANY($3) OR target_memory_id = ANY($3))
         ORDER BY edge_id DESC
         LIMIT 200",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(memory_ids)
    .fetch_all(pool)
    .await
    .map_err(map_storage)?;

    Ok(rows
        .into_iter()
        .map(|row| {
            let edge_handle = handles.assign_edge(EdgeId::new(row.edge_id));
            NeighborEdge {
                handle: edge_handle.as_str().to_string(),
                relation: row.relation,
                source: row.source_memory_id.map(|id| {
                    handles
                        .assign_memory(MemoryId::new(id))
                        .as_str()
                        .to_string()
                }),
                target: row.target_memory_id.map(|id| {
                    handles
                        .assign_memory(MemoryId::new(id))
                        .as_str()
                        .to_string()
                }),
            }
        })
        .collect())
}

#[derive(Debug, sqlx::FromRow)]
struct EdgeRow {
    edge_id: uuid::Uuid,
    relation: String,
    source_memory_id: Option<uuid::Uuid>,
    target_memory_id: Option<uuid::Uuid>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct OpenArgs {
    pub handle: String,
}

#[derive(Debug, Serialize)]
pub struct OpenOutput {
    pub handle: String,
    pub kind: String,
    pub schema_id: String,
    pub title: Option<String>,
    pub body: Option<String>,
    pub tags: Vec<String>,
    pub neighbor_edges: Vec<NeighborEdge>,
}

#[derive(Debug)]
pub struct OpenTool;

impl McpTool for OpenTool {
    const NAME: &'static str = "proxima-mcp/proxima_open";
    const DESCRIPTION: &'static str = "Resolve a memory handle to its payload and neighbor edges.";
    type Args = OpenArgs;
    type Output = OpenOutput;

    fn call(
        ctx: McpToolCtx,
        args: OpenArgs,
    ) -> futures::future::BoxFuture<'static, Result<OpenOutput, McpToolError>> {
        Box::pin(async move {
            let entity = ctx
                .handles
                .resolve(&args.handle)
                .ok_or_else(|| McpToolError::UnknownHandle(args.handle.clone()))?;
            let EntityRef::Memory(memory_id) = entity else {
                return Err(McpToolError::InvalidInput(
                    "proxima_open expects a memory handle".into(),
                ));
            };
            let (owner_kind, owner_principal_id) = owner_principal(&ctx.owner);
            let memory_uuid = memory_id.into_inner();
            let row = sqlx::query_as::<_, OpenRow>(OPEN_SQL)
                .bind(memory_uuid)
                .bind(owner_kind)
                .bind(owner_principal_id)
                .fetch_optional(&ctx.pool)
                .await
                .map_err(map_storage)?
                .ok_or_else(|| {
                    McpToolError::InvalidInput(format!("memory {memory_uuid} not found"))
                })?;
            let neighbor_edges =
                neighbor_edges(&ctx.pool, &ctx.owner, &ctx.handles, &[memory_uuid]).await?;
            Ok(OpenOutput {
                handle: args.handle,
                kind: memory_kind_for_edge(row.kind.as_deref()).to_string(),
                schema_id: row.schema_id,
                title: row.title,
                body: row.body,
                tags: row.tags.unwrap_or_default(),
                neighbor_edges,
            })
        })
    }
}

const OPEN_SQL: &str = r"
SELECT m.kind, m.schema_id,
       COALESCE(n.title, d.title) AS title,
       COALESCE(n.body, d.body) AS body,
       COALESCE(n.tags, d.tags) AS tags
FROM proxima_core.memories m
LEFT JOIN proxima_mcp.agent_note_v1 n USING (memory_id)
LEFT JOIN proxima_mcp.agent_derivation_v1 d USING (memory_id)
WHERE m.memory_id = $1
  AND m.owner_principal_kind = $2
  AND m.owner_principal_id = $3
";

#[derive(Debug, sqlx::FromRow)]
struct OpenRow {
    kind: Option<String>,
    schema_id: String,
    title: Option<String>,
    body: Option<String>,
    tags: Option<Vec<String>>,
}

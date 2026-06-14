use std::collections::BTreeMap;

use proxima_core::mcp::{McpTool, McpToolCtx, McpToolError};
use proxima_core::verbs::query::{MemorySearchRequest, MemorySearchResult, SearchMode};
use proxima_core::{EdgeId, MemoryId, PersonalityInstanceId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::util::{map_storage, memory_kind_for_edge, owner_principal};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchGraphArgs {
    #[schemars(
        description = "Lexical, semantic, or hybrid query over visible agent-authored graph memories. 1 to 512 chars."
    )]
    pub query: String,
    #[schemars(
        description = "Optional maximum number of graph matches. Omit or null for 12; values above 50 are clamped."
    )]
    pub limit: Option<u32>,
    #[serde(default)]
    #[schemars(description = "Search mode. Omit for lexical search.")]
    pub mode: SearchGraphMode,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SearchGraphMode {
    #[default]
    Lexical,
    Semantic,
    Hybrid,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authoring_personality_instance_id: Option<String>,
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
    const NAME: &'static str = "proxima-agent-memory/proxima_search_graph";
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
            let mode = args.mode;
            if matches!(mode, SearchGraphMode::Lexical) {
                return search_graph_lexical(ctx, query, limit).await;
            }

            search_graph_semantic_or_hybrid(ctx, query, limit, mode).await
        })
    }
}

async fn search_graph_lexical(
    ctx: McpToolCtx,
    query: &str,
    limit: u32,
) -> Result<SearchGraphOutput, McpToolError> {
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
        memory_ids.push(row.memory_id);
        matches.push(GraphMatch {
            handle: format_memory_by_kind(&ctx, MemoryId::new(row.memory_id), row.kind),
            kind: row.kind.as_str().to_string(),
            schema_id: row.schema_id,
            authoring_personality_instance_id: format_authoring_personality(
                &ctx,
                decode_personality(row.authoring_personality_instance_id),
            ),
            title: row.title,
            snippet: row.snippet,
            score: row.score,
            tags: row.tags,
        });
    }

    let neighbor_edges = neighbor_edges(&ctx, &memory_ids).await?;
    Ok(SearchGraphOutput {
        matches,
        neighbor_edges,
    })
}

async fn search_graph_semantic_or_hybrid(
    ctx: McpToolCtx,
    query: &str,
    limit: u32,
    mode: SearchGraphMode,
) -> Result<SearchGraphOutput, McpToolError> {
    let engine = ctx
        .engine()
        .ok_or_else(|| McpToolError::Other("engine-backed search unavailable".into()))?;
    let embed = engine
        .embed_client()
        .ok_or_else(|| McpToolError::Other("embedding client not wired into engine".into()))?;
    let query_embedding = embed
        .embed(query)
        .await
        .map_err(|e| McpToolError::Other(format!("embed query: {e}")))?;

    let mut candidates = BTreeMap::<uuid::Uuid, GraphCandidate>::new();
    if matches!(mode, SearchGraphMode::Hybrid) {
        merge_lexical_candidates(&ctx, query, limit, &mut candidates).await?;
    }

    let storage = ctx
        .storage()
        .ok_or_else(|| McpToolError::Other("engine-backed storage unavailable".into()))?;
    let semantic_rows = storage
        .search_memories(
            &MemorySearchRequest {
                principal: ctx.owner.principal.clone(),
                query: query.to_string(),
                mode: SearchMode::Semantic,
                limit: limit.saturating_mul(4),
                kind: None,
                schema_id: None,
                query_embedding: Some(query_embedding),
                embedding_model_id: Some(embed.model_id().to_string()),
                embedding_dim: Some(embed.dim()),
                reader_personality_instance_id: None,
            },
            ctx.registry.search_projections(),
        )
        .await?;
    merge_semantic_candidates(&ctx, semantic_rows, &mut candidates).await?;
    graph_output_from_candidates(&ctx, candidates, mode, limit).await
}

async fn merge_lexical_candidates(
    ctx: &McpToolCtx,
    query: &str,
    limit: u32,
    candidates: &mut BTreeMap<uuid::Uuid, GraphCandidate>,
) -> Result<(), McpToolError> {
    for row in lexical_rows(ctx, query, limit.saturating_mul(4)).await? {
        candidates.insert(
            row.memory_id,
            GraphCandidate {
                memory_id: row.memory_id,
                kind: row.kind.as_str().to_string(),
                schema_id: row.schema_id,
                authoring_personality_instance_id: decode_personality(
                    row.authoring_personality_instance_id,
                ),
                title: row.title,
                snippet: row.snippet,
                tags: row.tags,
                lexical_score: (row.score * 10.0).clamp(0.0, 1.0),
                similarity_score: 0.0,
            },
        );
    }
    Ok(())
}

async fn merge_semantic_candidates(
    ctx: &McpToolCtx,
    semantic_rows: Vec<MemorySearchResult>,
    candidates: &mut BTreeMap<uuid::Uuid, GraphCandidate>,
) -> Result<(), McpToolError> {
    let semantic_ids: Vec<uuid::Uuid> = semantic_rows
        .iter()
        .map(|row| row.memory_id.into_inner())
        .collect();
    let payloads = load_graph_payloads(ctx, &semantic_ids).await?;
    for row in semantic_rows {
        let memory_id = row.memory_id.into_inner();
        let payload = payloads.get(&memory_id);
        let entry = candidates
            .entry(memory_id)
            .or_insert_with(|| GraphCandidate {
                memory_id,
                kind: memory_kind_for_edge(Some(row.kind)).as_str().to_string(),
                schema_id: row.schema_id.as_str().to_string(),
                authoring_personality_instance_id: row.authoring_personality_instance_id,
                title: payload
                    .and_then(|p| p.title.clone())
                    .unwrap_or_else(|| row.schema_id.as_str().to_string()),
                snippet: payload.and_then(|p| p.body.clone()).map_or_else(
                    || row.snippet.clone(),
                    |body| body.chars().take(480).collect(),
                ),
                tags: payload.and_then(|p| p.tags.clone()).unwrap_or_default(),
                lexical_score: 0.0,
                similarity_score: 0.0,
            });
        entry.similarity_score = entry.similarity_score.max(row.similarity_score);
        if entry.snippet.is_empty() {
            entry.snippet = row.snippet;
        }
    }
    Ok(())
}

async fn graph_output_from_candidates(
    ctx: &McpToolCtx,
    candidates: BTreeMap<uuid::Uuid, GraphCandidate>,
    mode: SearchGraphMode,
    limit: u32,
) -> Result<SearchGraphOutput, McpToolError> {
    let mut ranked: Vec<_> = candidates.into_values().collect();
    ranked.sort_by(|a, b| {
        b.score(mode)
            .total_cmp(&a.score(mode))
            .then_with(|| b.memory_id.cmp(&a.memory_id))
    });
    ranked.truncate(usize::try_from(limit).unwrap_or(50));

    let mut matches = Vec::with_capacity(ranked.len());
    let mut memory_ids = Vec::with_capacity(ranked.len());
    for row in ranked {
        let score = row.score(mode);
        memory_ids.push(row.memory_id);
        matches.push(GraphMatch {
            handle: format_memory_by_kind_label(ctx, MemoryId::new(row.memory_id), &row.kind),
            kind: row.kind,
            schema_id: row.schema_id,
            authoring_personality_instance_id: format_authoring_personality(
                ctx,
                row.authoring_personality_instance_id,
            ),
            title: row.title,
            snippet: row.snippet,
            score,
            tags: row.tags,
        });
    }

    let neighbor_edges = neighbor_edges(ctx, &memory_ids).await?;
    Ok(SearchGraphOutput {
        matches,
        neighbor_edges,
    })
}

async fn lexical_rows(
    ctx: &McpToolCtx,
    query: &str,
    limit: u32,
) -> Result<Vec<SearchRow>, McpToolError> {
    let (owner_kind, owner_principal_id) = owner_principal(&ctx.owner);
    sqlx::query_as(SEARCH_SQL)
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(query)
        .bind(i64::from(limit))
        .fetch_all(&ctx.pool)
        .await
        .map_err(map_storage)
}

#[derive(Debug)]
struct GraphCandidate {
    memory_id: uuid::Uuid,
    kind: String,
    schema_id: String,
    authoring_personality_instance_id: Option<PersonalityInstanceId>,
    title: String,
    snippet: String,
    tags: Vec<String>,
    lexical_score: f32,
    similarity_score: f32,
}

impl GraphCandidate {
    fn score(&self, mode: SearchGraphMode) -> f32 {
        match mode {
            SearchGraphMode::Lexical => self.lexical_score,
            SearchGraphMode::Semantic => self.similarity_score,
            SearchGraphMode::Hybrid => (0.6 * self.similarity_score) + (0.4 * self.lexical_score),
        }
    }
}

async fn load_graph_payloads(
    ctx: &McpToolCtx,
    memory_ids: &[uuid::Uuid],
) -> Result<BTreeMap<uuid::Uuid, GraphPayloadRow>, McpToolError> {
    if memory_ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let (owner_kind, owner_principal_id) = owner_principal(&ctx.owner);
    let rows: Vec<GraphPayloadRow> = sqlx::query_as(
        "SELECT m.memory_id,
                COALESCE(n.title, d.title) AS title,
                COALESCE(n.body, d.body) AS body,
                COALESCE(n.tags, d.tags) AS tags
         FROM proxima_core.memories m
         LEFT JOIN proxima_agent_memory.agent_note_v1 n USING (memory_id)
         LEFT JOIN proxima_agent_memory.agent_derivation_v1 d USING (memory_id)
         WHERE m.owner_principal_kind = $1
           AND m.owner_principal_id = $2
           AND m.memory_id = ANY($3::uuid[])",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(memory_ids)
    .fetch_all(&ctx.pool)
    .await
    .map_err(map_storage)?;
    Ok(rows.into_iter().map(|row| (row.memory_id, row)).collect())
}

#[derive(Debug, sqlx::FromRow)]
struct GraphPayloadRow {
    memory_id: uuid::Uuid,
    title: Option<String>,
    body: Option<String>,
    tags: Option<Vec<String>>,
}

const SEARCH_SQL: &str = r"
WITH q AS (SELECT websearch_to_tsquery('simple', $3) AS tsq)
SELECT *
FROM (
    SELECT m.memory_id,
           'Fact'::proxima_core.entity_kind AS kind,
           m.schema_id,
           m.personality_instance_id AS authoring_personality_instance_id,
           a.title,
           left(a.body, 480) AS snippet,
           ts_rank_cd(to_tsvector('simple', a.title || ' ' || a.body), q.tsq) AS score,
           a.tags
    FROM q, proxima_core.memories m
    JOIN proxima_agent_memory.agent_note_v1 a USING (memory_id)
    WHERE m.owner_principal_kind = $1
      AND m.owner_principal_id = $2
      AND m.kind IS NULL
      AND to_tsvector('simple', a.title || ' ' || a.body) @@ q.tsq
    UNION ALL
    SELECT m.memory_id,
           m.kind,
           m.schema_id,
           m.personality_instance_id AS authoring_personality_instance_id,
           d.title,
           left(d.body, 480),
           ts_rank_cd(to_tsvector('simple', d.title || ' ' || d.body), q.tsq),
           d.tags
    FROM q, proxima_core.memories m
    JOIN proxima_agent_memory.agent_derivation_v1 d USING (memory_id)
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
    kind: proxima_core::EntityKind,
    schema_id: String,
    authoring_personality_instance_id: Option<uuid::Uuid>,
    title: String,
    snippet: String,
    score: f32,
    tags: Vec<String>,
}

/// # Errors
///
/// Returns storage errors from the owner-filtered edge query.
pub async fn neighbor_edges(
    ctx: &McpToolCtx,
    memory_ids: &[uuid::Uuid],
) -> Result<Vec<NeighborEdge>, McpToolError> {
    if memory_ids.is_empty() {
        return Ok(Vec::new());
    }
    let (owner_kind, owner_principal_id) = owner_principal(&ctx.owner);
    let rows: Vec<EdgeRow> = sqlx::query_as(
        "SELECT edge_id, relation, source_kind, source_memory_id, target_kind, target_memory_id
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
    .fetch_all(&ctx.pool)
    .await
    .map_err(map_storage)?;

    Ok(rows
        .into_iter()
        .map(|row| NeighborEdge {
            handle: ctx.format_edge(EdgeId::new(row.edge_id)),
            relation: row.relation,
            source: row
                .source_memory_id
                .map(|id| format_memory_by_kind(ctx, MemoryId::new(id), row.source_kind)),
            target: row
                .target_memory_id
                .map(|id| format_memory_by_kind(ctx, MemoryId::new(id), row.target_kind)),
        })
        .collect())
}

#[derive(Debug, sqlx::FromRow)]
struct EdgeRow {
    edge_id: uuid::Uuid,
    relation: String,
    source_kind: proxima_core::EntityKind,
    source_memory_id: Option<uuid::Uuid>,
    target_kind: proxima_core::EntityKind,
    target_memory_id: Option<uuid::Uuid>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct OpenArgs {
    #[schemars(
        description = "`F...`, `A...`, or `P...` memory handle to open and inspect with neighbor edges."
    )]
    pub handle: String,
}

fn format_memory_by_kind(
    ctx: &McpToolCtx,
    memory_id: MemoryId,
    kind: proxima_core::EntityKind,
) -> String {
    match kind {
        proxima_core::EntityKind::Abstraction => ctx.format_abstraction_memory(memory_id),
        proxima_core::EntityKind::Perspective => ctx.format_perspective_memory(memory_id),
        _ => ctx.format_fact_memory(memory_id),
    }
}

fn format_memory_by_kind_label(ctx: &McpToolCtx, memory_id: MemoryId, kind: &str) -> String {
    match kind {
        "Abstraction" => ctx.format_abstraction_memory(memory_id),
        "Perspective" => ctx.format_perspective_memory(memory_id),
        _ => ctx.format_fact_memory(memory_id),
    }
}

fn decode_personality(instance_id: Option<uuid::Uuid>) -> Option<PersonalityInstanceId> {
    instance_id
        .filter(|id| !id.is_nil())
        .map(PersonalityInstanceId::new)
}

fn format_authoring_personality(
    ctx: &McpToolCtx,
    instance_id: Option<PersonalityInstanceId>,
) -> Option<String> {
    instance_id.map(|id| ctx.format_personality(id))
}

#[derive(Debug, Serialize)]
pub struct OpenOutput {
    pub handle: String,
    pub kind: String,
    pub schema_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authoring_personality_instance_id: Option<String>,
    pub title: Option<String>,
    pub body: Option<String>,
    pub tags: Vec<String>,
    pub neighbor_edges: Vec<NeighborEdge>,
}

#[derive(Debug)]
pub struct OpenTool;

impl McpTool for OpenTool {
    const NAME: &'static str = "proxima-agent-memory/proxima_open";
    const DESCRIPTION: &'static str = "Resolve a memory handle to its payload and neighbor edges.";
    type Args = OpenArgs;
    type Output = OpenOutput;

    fn call(
        ctx: McpToolCtx,
        args: OpenArgs,
    ) -> futures::future::BoxFuture<'static, Result<OpenOutput, McpToolError>> {
        Box::pin(async move {
            let memory_id = ctx.resolve_memory(&args.handle)?;
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
            let neighbor_edges = neighbor_edges(&ctx, &[memory_uuid]).await?;
            Ok(OpenOutput {
                handle: args.handle,
                kind: memory_kind_for_edge(row.kind).as_str().to_string(),
                schema_id: row.schema_id,
                authoring_personality_instance_id: format_authoring_personality(
                    &ctx,
                    decode_personality(row.authoring_personality_instance_id),
                ),
                title: row.title,
                body: row.body,
                tags: row.tags.unwrap_or_default(),
                neighbor_edges,
            })
        })
    }
}

const OPEN_SQL: &str = r"
SELECT m.kind, m.schema_id, m.personality_instance_id AS authoring_personality_instance_id,
       COALESCE(n.title, d.title) AS title,
       COALESCE(n.body, d.body) AS body,
       COALESCE(n.tags, d.tags) AS tags
FROM proxima_core.memories m
LEFT JOIN proxima_agent_memory.agent_note_v1 n USING (memory_id)
LEFT JOIN proxima_agent_memory.agent_derivation_v1 d USING (memory_id)
WHERE m.memory_id = $1
  AND m.owner_principal_kind = $2
  AND m.owner_principal_id = $3
";

#[derive(Debug, sqlx::FromRow)]
struct OpenRow {
    kind: Option<proxima_core::EntityKind>,
    schema_id: String,
    authoring_personality_instance_id: Option<uuid::Uuid>,
    title: Option<String>,
    body: Option<String>,
    tags: Option<Vec<String>>,
}

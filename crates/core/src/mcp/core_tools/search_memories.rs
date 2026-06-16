//! `core/search_memories` — owner-scoped lexical/semantic/hybrid memory search.

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use time::format_description::well_known::Rfc3339;

use crate::mcp::{McpToolCtx, McpToolError};
use crate::verbs::query::{EntityKind, MemorySearchRequest, SearchMode, SearchOrder, TagMatch};
use crate::{McpTool, SchemaId};

use super::memory::search::{NeighborEdge, load_graph_payloads, neighbor_edges};

#[derive(Debug, Default)]
pub struct SearchMemoriesTool;

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SearchMemoriesMode {
    Lexical,
    Semantic,
    Hybrid,
}

impl From<SearchMemoriesMode> for SearchMode {
    fn from(value: SearchMemoriesMode) -> Self {
        match value {
            SearchMemoriesMode::Lexical => Self::Lexical,
            SearchMemoriesMode::Semantic => Self::Semantic,
            SearchMemoriesMode::Hybrid => Self::Hybrid,
        }
    }
}

fn default_mode() -> SearchMemoriesMode {
    SearchMemoriesMode::Hybrid
}

fn default_limit() -> u32 {
    8
}

fn default_include_neighbor_edges() -> bool {
    true
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
pub enum SearchMemoriesKind {
    #[serde(rename = "Fact", alias = "fact")]
    Fact,
    #[serde(rename = "Abstraction", alias = "abstraction")]
    Abstraction,
    #[serde(rename = "Perspective", alias = "perspective")]
    Perspective,
}

impl From<SearchMemoriesKind> for EntityKind {
    fn from(value: SearchMemoriesKind) -> Self {
        match value {
            SearchMemoriesKind::Fact => Self::Fact,
            SearchMemoriesKind::Abstraction => Self::Abstraction,
            SearchMemoriesKind::Perspective => Self::Perspective,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchMemoriesArgs {
    #[schemars(description = "Search query over owner-visible memories. 1 to 512 chars.")]
    pub query: String,
    #[serde(default = "default_mode")]
    #[schemars(description = "Search mode: lexical, semantic, or hybrid. Defaults to hybrid.")]
    pub mode: SearchMemoriesMode,
    #[serde(default = "default_limit")]
    #[schemars(
        description = "Maximum number of memories to return. Defaults to 8; values are clamped to 1..=50."
    )]
    pub limit: u32,
    #[serde(default)]
    #[schemars(
        description = "Optional memory kind filter: Fact, Abstraction, or Perspective. Omit or null for all kinds."
    )]
    pub kind: Option<SearchMemoriesKind>,
    #[serde(default)]
    #[schemars(description = "Optional schema_id filter. Omit or null for all schemas.")]
    pub schema_id: Option<String>,
    #[serde(default)]
    #[schemars(description = "Optional exact tag filter. Empty means no tag filter.")]
    pub tags: Vec<String>,
    #[serde(default)]
    #[schemars(description = "Tag filter mode: any or all. Defaults to any.")]
    pub tag_match: TagMatch,
    #[serde(default)]
    #[schemars(description = "Optional inclusive lower created_at bound as an RFC3339 timestamp.")]
    pub since: Option<String>,
    #[serde(default)]
    #[schemars(description = "Optional inclusive upper created_at bound as an RFC3339 timestamp.")]
    pub until: Option<String>,
    #[serde(default)]
    #[schemars(description = "Result ordering: relevance or recency. Defaults to relevance.")]
    pub order: SearchOrder,
    #[serde(default)]
    #[schemars(
        description = "Optional reader personality id/handle for Abstraction/Perspective visibility. Omit for no-reader owner-visible semantics."
    )]
    pub reader_personality: Option<String>,
    #[serde(default = "default_include_neighbor_edges")]
    #[schemars(
        description = "Include neighbor edges touching matched memories. Defaults to true; set false for lean results."
    )]
    pub include_neighbor_edges: bool,
}

#[derive(Debug, Serialize)]
pub struct SearchMemoriesOutput {
    pub mode: String,
    pub memories: Vec<SearchMemoryOutput>,
    pub neighbor_edges: Vec<NeighborEdge>,
}

#[derive(Debug, Serialize)]
pub struct SearchMemoryOutput {
    pub memory: String,
    pub kind: String,
    pub schema_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authoring_personality_instance_id: Option<String>,
    pub created_at: String,
    pub snippet: String,
    pub score: f32,
    pub lexical_score: f32,
    pub similarity_score: f32,
    pub wake_chain_depth: u16,
    pub tags: Vec<String>,
}

impl McpTool for SearchMemoriesTool {
    const NAME: &'static str = "core/search_memories";
    const DESCRIPTION: &'static str = "Search owner-scoped memories by lexical, semantic, or hybrid ranking. Optional reader_personality applies read-scope filtering for Abstractions/Perspectives; omitted reader uses no-reader owner-visible semantics.";
    type Args = SearchMemoriesArgs;
    type Output = SearchMemoriesOutput;

    fn call(
        ctx: McpToolCtx,
        args: SearchMemoriesArgs,
    ) -> BoxFuture<'static, Result<SearchMemoriesOutput, McpToolError>> {
        Box::pin(async move {
            let query = args.query.trim();
            if query.is_empty() || query.chars().count() > 512 {
                return Err(McpToolError::InvalidInput(
                    "query must be 1..=512 chars".into(),
                ));
            }

            let mode = SearchMode::from(args.mode);
            let since = parse_rfc3339(args.since.as_deref(), "since")?;
            let until = parse_rfc3339(args.until.as_deref(), "until")?;
            let reader = args
                .reader_personality
                .as_deref()
                .map(|raw| ctx.resolve_personality(raw))
                .transpose()?;
            let mut req = MemorySearchRequest {
                principal: ctx.owner.principal.clone(),
                query: query.to_string(),
                mode,
                limit: args.limit.clamp(1, 50),
                kind: args.kind.map(EntityKind::from),
                schema_id: args.schema_id.map(SchemaId::new),
                tags: args.tags,
                tag_match: args.tag_match,
                since,
                until,
                order: args.order,
                query_embedding: None,
                embedding_model_id: None,
                reader_personality_instance_id: reader,
            };

            if matches!(mode, SearchMode::Semantic | SearchMode::Hybrid) {
                let engine = ctx.engine().ok_or_else(|| {
                    McpToolError::Other("engine required for semantic search".into())
                })?;
                let embed = engine.embed_client().ok_or_else(|| {
                    McpToolError::Other(
                        "semantic search unavailable: no embedding client is configured (set MISTRAL_API_KEY)"
                            .into(),
                    )
                })?;
                req.query_embedding = Some(
                    embed
                        .embed(query)
                        .await
                        .map_err(|err| McpToolError::Other(format!("embed query: {err}")))?,
                );
                req.embedding_model_id = Some(embed.model_id().to_string());
            }

            let storage = ctx
                .storage()
                .ok_or_else(|| McpToolError::Other("engine storage unavailable".into()))?;
            let rows = storage
                .search_memories(&req, ctx.registry.search_projections())
                .await?;
            let memory_ids: Vec<_> = rows.iter().map(|row| row.memory_id.into_inner()).collect();
            let payloads = load_graph_payloads(&ctx, &memory_ids).await?;
            let neighbor_edges = if args.include_neighbor_edges {
                neighbor_edges(&ctx, &memory_ids).await?
            } else {
                Vec::new()
            };
            let memories = rows
                .into_iter()
                .map(|row| {
                    let tags = payloads
                        .get(&row.memory_id.into_inner())
                        .and_then(|payload| payload.tags.clone())
                        .unwrap_or_default();
                    let class = super::get_memory::memory_class(row.kind.as_str())?;
                    Ok(SearchMemoryOutput {
                        memory: ctx.format_memory_with_class(row.memory_id, class),
                        kind: row.kind.as_str().to_string(),
                        schema_id: row.schema_id.as_str().to_string(),
                        authoring_personality_instance_id:
                            super::get_memory::format_authoring_personality(
                                &ctx,
                                row.authoring_personality_instance_id,
                            ),
                        created_at: format_rfc3339(row.created_at)?,
                        snippet: row.snippet,
                        score: row.score,
                        lexical_score: row.lexical_score,
                        similarity_score: row.similarity_score,
                        wake_chain_depth: row.wake_chain_depth.into_inner(),
                        tags,
                    })
                })
                .collect::<Result<Vec<_>, McpToolError>>()?;

            Ok(SearchMemoriesOutput {
                mode: format!("{mode:?}").to_lowercase(),
                memories,
                neighbor_edges,
            })
        })
    }
}

fn parse_rfc3339(
    raw: Option<&str>,
    field: &str,
) -> Result<Option<time::OffsetDateTime>, McpToolError> {
    raw.map(|value| {
        time::OffsetDateTime::parse(value, &Rfc3339).map_err(|err| {
            McpToolError::InvalidInput(format!("{field} must be an RFC3339 timestamp: {err}"))
        })
    })
    .transpose()
}

fn format_rfc3339(value: time::OffsetDateTime) -> Result<String, McpToolError> {
    value
        .format(&Rfc3339)
        .map_err(|err| McpToolError::Other(format!("format created_at: {err}")))
}

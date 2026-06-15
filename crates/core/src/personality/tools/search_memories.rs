//! `core/search_memories` substrate tool — owner-scoped hybrid memory
//! search.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use time::format_description::well_known::Rfc3339;

use crate::SchemaId;
use crate::error::ProtocolError;
use crate::mcp::schema::mcp_tool_schema;
use crate::personality::{PersonalityTool, PersonalityToolContext, PersonalityToolResult};
use crate::verbs::query::{EntityKind, MemorySearchRequest, SearchMode, SearchOrder, TagMatch};

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
            SearchMemoriesMode::Lexical => SearchMode::Lexical,
            SearchMemoriesMode::Semantic => SearchMode::Semantic,
            SearchMemoriesMode::Hybrid => SearchMode::Hybrid,
        }
    }
}

fn default_mode() -> SearchMemoriesMode {
    SearchMemoriesMode::Hybrid
}

fn default_limit() -> u32 {
    8
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SearchMemoriesKind {
    Fact,
    Abstraction,
    Perspective,
}

impl From<SearchMemoriesKind> for EntityKind {
    fn from(value: SearchMemoriesKind) -> Self {
        match value {
            SearchMemoriesKind::Fact => EntityKind::Fact,
            SearchMemoriesKind::Abstraction => EntityKind::Abstraction,
            SearchMemoriesKind::Perspective => EntityKind::Perspective,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchMemoriesArgs {
    #[schemars(description = "Search query over visible memories. 1 to 512 chars.")]
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
    #[schemars(
        description = "Optional schema_id filter, for example `proxima-code/commit-v1`. Omit or null for all schemas."
    )]
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
}

#[async_trait]
impl PersonalityTool for SearchMemoriesTool {
    fn tool_id(&self) -> &'static str {
        "core/search_memories"
    }

    fn description(&self) -> &'static str {
        "Search memories by lexical, semantic, or hybrid ranking. Returns \
         memory handles, snippets, scores, and wake_chain_depth. Use kind \
         and schema_id filters when known; broad all-schema searches scan \
         more visible memory text."
    }

    fn args_schema(&self) -> serde_json::Value {
        mcp_tool_schema::<SearchMemoriesArgs>()
    }

    async fn invoke(
        &self,
        ctx: &PersonalityToolContext<'_>,
        args: serde_json::Value,
    ) -> Result<PersonalityToolResult, ProtocolError> {
        let parsed: SearchMemoriesArgs = match serde_json::from_value(args) {
            Ok(v) => v,
            Err(e) => {
                return Ok(PersonalityToolResult::error(serde_json::json!({
                    "error": format!("invalid args: {e}"),
                })));
            }
        };

        let query = parsed.query.trim();
        if query.is_empty() || query.chars().count() > 512 {
            return Ok(PersonalityToolResult::error(serde_json::json!({
                "error": "query must be 1..=512 chars",
            })));
        }

        let mode = SearchMode::from(parsed.mode);
        let since = match parse_rfc3339(parsed.since.as_deref(), "since") {
            Ok(value) => value,
            Err(message) => {
                return Ok(PersonalityToolResult::error(serde_json::json!({
                    "error": message,
                })));
            }
        };
        let until = match parse_rfc3339(parsed.until.as_deref(), "until") {
            Ok(value) => value,
            Err(message) => {
                return Ok(PersonalityToolResult::error(serde_json::json!({
                    "error": message,
                })));
            }
        };
        let mut req = MemorySearchRequest {
            principal: ctx.owner.principal.clone(),
            query: query.to_string(),
            mode,
            limit: parsed.limit.clamp(1, 50),
            kind: parsed.kind.map(EntityKind::from),
            schema_id: parsed.schema_id.map(SchemaId::new),
            tags: parsed.tags,
            tag_match: parsed.tag_match,
            since,
            until,
            order: parsed.order,
            query_embedding: None,
            embedding_model_id: None,
            reader_personality_instance_id: Some(ctx.instance_id),
        };

        if matches!(mode, SearchMode::Semantic | SearchMode::Hybrid) {
            let embed = ctx
                .engine
                .embed_client()
                .ok_or_else(|| ProtocolError::internal("embedding client not wired into engine"))?;
            req.query_embedding = Some(
                embed
                    .embed(query)
                    .await
                    .map_err(|e| ProtocolError::internal(format!("embed query: {e}")))?,
            );
            req.embedding_model_id = Some(embed.model_id().to_string());
        }

        let rows = ctx
            .engine
            .storage()
            .search_memories(&req, ctx.engine.registry().search_projections())
            .await
            .map_err(|e| ProtocolError::internal(format!("search_memories: {e}")))?;
        ctx.record_read(rows.iter().map(|row| (row.memory_id, row.wake_chain_depth)))
            .await;

        let memories: Vec<_> = rows
            .into_iter()
            .map(|row| {
                let handle = ctx
                    .handles
                    .assign_memory_kind(row.memory_id, &format!("{:?}", row.kind));
                serde_json::json!({
                    "memory": handle.as_str(),
                    "kind": format!("{:?}", row.kind),
                    "schema_id": row.schema_id.as_str(),
                    "created_at": format_rfc3339(row.created_at),
                    "snippet": row.snippet,
                    "score": row.score,
                    "lexical_score": row.lexical_score,
                    "similarity_score": row.similarity_score,
                    "wake_chain_depth": row.wake_chain_depth.into_inner(),
                })
            })
            .collect();

        Ok(PersonalityToolResult::ok(serde_json::json!({
            "mode": format!("{mode:?}").to_lowercase(),
            "memories": memories,
        })))
    }
}

fn parse_rfc3339(raw: Option<&str>, field: &str) -> Result<Option<time::OffsetDateTime>, String> {
    raw.map(|value| {
        time::OffsetDateTime::parse(value, &Rfc3339)
            .map_err(|err| format!("{field} must be an RFC3339 timestamp: {err}"))
    })
    .transpose()
}

fn format_rfc3339(value: time::OffsetDateTime) -> String {
    value
        .format(&Rfc3339)
        .expect("OffsetDateTime formats as RFC3339")
}

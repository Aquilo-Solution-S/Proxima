//! `core/search_memories` substrate tool — owner-scoped hybrid memory
//! search.

use std::sync::OnceLock;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::SchemaId;
use crate::error::ProtocolError;
use crate::personality::{PersonalityTool, PersonalityToolContext, PersonalityToolResult};
use crate::verbs::query::{EntityKind, MemorySearchRequest, SearchMode};

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
    pub query: String,
    #[serde(default = "default_mode")]
    pub mode: SearchMemoriesMode,
    #[serde(default = "default_limit")]
    pub limit: u32,
    #[serde(default)]
    pub kind: Option<SearchMemoriesKind>,
    #[serde(default)]
    pub schema_id: Option<String>,
}

fn args_schema_value() -> &'static serde_json::Value {
    static SCHEMA: OnceLock<serde_json::Value> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        serde_json::to_value(schemars::schema_for!(SearchMemoriesArgs))
            .expect("SearchMemoriesArgs schema serializes")
    })
}

#[async_trait]
impl PersonalityTool for SearchMemoriesTool {
    fn tool_id(&self) -> &'static str {
        "core/search_memories"
    }

    fn description(&self) -> &'static str {
        "Search memories by lexical, semantic, or hybrid ranking. Returns \
         memory handles, snippets, scores, and wake_chain_depth."
    }

    fn args_schema(&self) -> serde_json::Value {
        args_schema_value().clone()
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
        let mut req = MemorySearchRequest {
            owner: ctx.owner.clone(),
            query: query.to_string(),
            mode,
            limit: parsed.limit.clamp(1, 50),
            kind: parsed.kind.map(EntityKind::from),
            schema_id: parsed.schema_id.map(SchemaId::new),
            query_embedding: None,
            embedding_model_id: None,
            embedding_dim: None,
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
            req.embedding_dim = Some(embed.dim());
        }

        let rows = ctx
            .engine
            .storage()
            .search_memories(&req, ctx.engine.registry().list().as_slice())
            .await
            .map_err(|e| ProtocolError::internal(format!("search_memories: {e}")))?;
        ctx.record_read(rows.iter().map(|row| (row.memory_id, row.wake_chain_depth)))
            .await;

        let memories: Vec<_> = rows
            .into_iter()
            .map(|row| {
                let handle = ctx.handles.assign_memory(row.memory_id);
                serde_json::json!({
                    "memory": handle.as_str(),
                    "kind": format!("{:?}", row.kind),
                    "schema_id": row.schema_id.as_str(),
                    "snippet": row.snippet,
                    "score": row.score,
                    "lexical_score": row.lexical_score,
                    "similarity_score": row.similarity_score,
                    "wake_chain_depth": row.wake_chain_depth.into_inner(),
                })
            })
            .collect();

        Ok(PersonalityToolResult::ok(serde_json::json!({
            "mode": format!("{:?}", mode).to_lowercase(),
            "memories": memories,
        })))
    }
}

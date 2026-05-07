//! `core/search_by_embedding` substrate tool — semantic search across
//! memories. v1: not implemented (returns a tool error so the loop
//! continues; the LLM will pick another tool).

use std::sync::OnceLock;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::error::ProtocolError;
use crate::personality::{PersonalityTool, PersonalityToolContext, PersonalityToolResult};

#[derive(Debug, Default)]
pub struct SearchByEmbeddingTool;

#[derive(Debug, Deserialize, JsonSchema)]
#[allow(dead_code)]
pub struct SearchByEmbeddingArgs {
    pub query: String,
    #[serde(default = "default_k")]
    pub k: u32,
    #[serde(default)]
    pub schema_filter: Option<String>,
}

fn default_k() -> u32 {
    8
}

fn args_schema_value() -> &'static serde_json::Value {
    static SCHEMA: OnceLock<serde_json::Value> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        serde_json::to_value(schemars::schema_for!(SearchByEmbeddingArgs))
            .expect("SearchByEmbeddingArgs schema serializes")
    })
}

#[async_trait]
impl PersonalityTool for SearchByEmbeddingTool {
    fn tool_id(&self) -> &'static str {
        "core/search_by_embedding"
    }

    fn description(&self) -> &'static str {
        "Semantic search over memories by embedding similarity. v1: not \
         yet available — pick another tool."
    }

    fn args_schema(&self) -> serde_json::Value {
        args_schema_value().clone()
    }

    async fn invoke(
        &self,
        _ctx: &PersonalityToolContext<'_>,
        _args: serde_json::Value,
    ) -> Result<PersonalityToolResult, ProtocolError> {
        Ok(PersonalityToolResult::error(serde_json::json!({
            "error": "core/search_by_embedding is not implemented in v1",
        })))
    }
}

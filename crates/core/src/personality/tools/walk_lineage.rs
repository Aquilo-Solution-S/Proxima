//! `core/walk_lineage` substrate tool — walk the provenance/supersedes
//! graph from a starting memory. v1: not implemented (returns a tool
//! error so the loop continues; the LLM will pick another tool).

use std::sync::OnceLock;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::error::ProtocolError;
use crate::personality::{PersonalityTool, PersonalityToolContext, PersonalityToolResult};

#[derive(Debug, Default)]
pub struct WalkLineageTool;

#[derive(Debug, Deserialize, JsonSchema)]
#[allow(dead_code)]
pub struct WalkLineageArgs {
    /// Handle of the memory to start walking from (e.g., `N1`).
    pub memory: String,
    #[serde(default = "default_direction")]
    pub direction: String,
    #[serde(default = "default_depth")]
    pub depth: u8,
}

fn default_direction() -> String {
    "ancestors".into()
}

fn default_depth() -> u8 {
    3
}

fn args_schema_value() -> &'static serde_json::Value {
    static SCHEMA: OnceLock<serde_json::Value> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        serde_json::to_value(schemars::schema_for!(WalkLineageArgs))
            .expect("WalkLineageArgs schema serializes")
    })
}

#[async_trait]
impl PersonalityTool for WalkLineageTool {
    fn tool_id(&self) -> &'static str {
        "core/walk_lineage"
    }

    fn description(&self) -> &'static str {
        "Walk the provenance/supersedes lineage from a memory. v1: not \
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
            "error": "core/walk_lineage is not implemented in v1",
        })))
    }
}

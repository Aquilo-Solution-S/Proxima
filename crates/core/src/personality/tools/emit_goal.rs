//! `core/emit_goal` substrate tool — author a Goal with auto-wired
//! Provenance. v1: not implemented (the Code flavor does not exercise
//! Goal authoring). Returns a tool error so the loop can continue.

use std::sync::OnceLock;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::error::ProtocolError;
use crate::personality::{PersonalityTool, PersonalityToolContext, PersonalityToolResult};

#[derive(Debug, Default)]
pub struct EmitGoalTool;

#[derive(Debug, Deserialize, JsonSchema)]
#[allow(dead_code)]
pub struct EmitGoalArgs {
    pub schema_id: String,
    pub schema_version: u32,
    pub payload: serde_json::Value,
    /// Handle of the target self-Perspective memory (e.g., `N2`).
    #[serde(default)]
    pub inspires_target_self_perspective: Option<String>,
    /// Memory handles that motivate this goal.
    #[serde(default)]
    pub motivated_by: Vec<String>,
}

fn args_schema_value() -> &'static serde_json::Value {
    static SCHEMA: OnceLock<serde_json::Value> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        serde_json::to_value(schemars::schema_for!(EmitGoalArgs))
            .expect("EmitGoalArgs schema serializes")
    })
}

#[async_trait]
impl PersonalityTool for EmitGoalTool {
    fn tool_id(&self) -> &'static str {
        "core/emit_goal"
    }

    fn description(&self) -> &'static str {
        "Emit one Goal with auto-wired Provenance. v1: not yet available — \
         pick another tool."
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
            "error": "core/emit_goal is not implemented in v1",
        })))
    }
}

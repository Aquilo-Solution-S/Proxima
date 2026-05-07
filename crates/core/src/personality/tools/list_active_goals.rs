//! `core/list_active_goals` substrate tool — list goals visible to the
//! personality. v1: not implemented (the Code flavor does not exercise
//! Goal authoring); the tool returns a typed error so the loop continues.

use std::sync::OnceLock;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::error::ProtocolError;
use crate::personality::{PersonalityTool, PersonalityToolContext, PersonalityToolResult};

#[derive(Debug, Default)]
pub struct ListActiveGoalsTool;

#[derive(Debug, Deserialize, JsonSchema)]
#[allow(dead_code)]
pub struct ListActiveGoalsArgs {
    #[serde(default = "default_scope")]
    pub scope: String,
}

fn default_scope() -> String {
    "linked_to_self".into()
}

fn args_schema_value() -> &'static serde_json::Value {
    static SCHEMA: OnceLock<serde_json::Value> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        serde_json::to_value(schemars::schema_for!(ListActiveGoalsArgs))
            .expect("ListActiveGoalsArgs schema serializes")
    })
}

#[async_trait]
impl PersonalityTool for ListActiveGoalsTool {
    fn tool_id(&self) -> &'static str {
        "core/list_active_goals"
    }

    fn description(&self) -> &'static str {
        "List active goals scoped to this personality or owner-wide. v1: \
         not yet available — pick another tool."
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
            "error": "core/list_active_goals is not implemented in v1",
        })))
    }
}

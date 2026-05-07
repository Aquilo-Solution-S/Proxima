//! `core/create_edge` substrate tool — append a typed/labeled edge.
//! v1: not implemented (the Code flavor declares an empty
//! `writeable_relations`). Returns a tool error so the loop continues.

use std::sync::OnceLock;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::error::ProtocolError;
use crate::personality::{
    PersonalityTool, PersonalityToolContext, PersonalityToolResult,
    authorization::{authorize_create_edge, AuthorizationError},
};

#[derive(Debug, Default)]
pub struct CreateEdgeTool;

#[derive(Debug, Deserialize, JsonSchema)]
#[allow(dead_code)]
pub struct CreateEdgeArgs {
    pub source_memory_id: uuid::Uuid,
    pub relation_id: String,
    pub target_memory_id: uuid::Uuid,
}

fn args_schema_value() -> &'static serde_json::Value {
    static SCHEMA: OnceLock<serde_json::Value> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        serde_json::to_value(schemars::schema_for!(CreateEdgeArgs))
            .expect("CreateEdgeArgs schema serializes")
    })
}

#[async_trait]
impl PersonalityTool for CreateEdgeTool {
    fn tool_id(&self) -> &'static str {
        "core/create_edge"
    }

    fn description(&self) -> &'static str {
        "Create a single edge between two memories. core/provenance and \
         core/supersedes are substrate-only and rejected here."
    }

    fn args_schema(&self) -> serde_json::Value {
        args_schema_value().clone()
    }

    async fn invoke(
        &self,
        ctx: &PersonalityToolContext<'_>,
        args: serde_json::Value,
    ) -> Result<PersonalityToolResult, ProtocolError> {
        let parsed: CreateEdgeArgs = match serde_json::from_value(args) {
            Ok(v) => v,
            Err(e) => {
                return Ok(PersonalityToolResult::error(serde_json::json!({
                    "error": format!("invalid args: {e}"),
                })));
            }
        };
        match authorize_create_edge(&parsed.relation_id, ctx.writeable_relations) {
            Ok(()) => {}
            Err(AuthorizationError::SubstrateOnlyRelation { relation_id }) => {
                return Ok(PersonalityToolResult::error(serde_json::json!({
                    "error": format!("relation {relation_id} is substrate-only"),
                })));
            }
            Err(err) => {
                return Ok(PersonalityToolResult::error(serde_json::json!({
                    "error": err.to_string(),
                })));
            }
        }
        Ok(PersonalityToolResult::error(serde_json::json!({
            "error": "core/create_edge write path is not implemented in v1",
        })))
    }
}

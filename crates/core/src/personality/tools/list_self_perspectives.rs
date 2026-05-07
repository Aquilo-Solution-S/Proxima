//! `core/list_self_perspectives` substrate tool — list this owner's
//! personality instances, each with its current self-Perspective memory id.

use std::sync::OnceLock;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::error::ProtocolError;
use crate::personality::{
    PersonalityTool, PersonalityToolContext, PersonalityToolResult, WakeChainDepth,
};

#[derive(Debug, Default)]
pub struct ListSelfPerspectivesTool;

#[derive(Debug, Deserialize, JsonSchema, Default)]
pub struct ListSelfPerspectivesArgs {}

fn args_schema_value() -> &'static serde_json::Value {
    static SCHEMA: OnceLock<serde_json::Value> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        serde_json::to_value(schemars::schema_for!(ListSelfPerspectivesArgs))
            .expect("ListSelfPerspectivesArgs schema serializes")
    })
}

#[async_trait]
impl PersonalityTool for ListSelfPerspectivesTool {
    fn tool_id(&self) -> &'static str {
        "core/list_self_perspectives"
    }

    fn description(&self) -> &'static str {
        "List the personality instances configured for this owner with each \
         instance's current self-Perspective memory id."
    }

    fn args_schema(&self) -> serde_json::Value {
        args_schema_value().clone()
    }

    async fn invoke(
        &self,
        ctx: &PersonalityToolContext<'_>,
        _args: serde_json::Value,
    ) -> Result<PersonalityToolResult, ProtocolError> {
        let rows = ctx
            .engine
            .storage()
            .list_personality_instances(ctx.owner, None, false)
            .await
            .map_err(|e| ProtocolError::internal(format!("list_personality_instances: {e}")))?;
        let entries: Vec<serde_json::Value> = rows
            .iter()
            .map(|row| {
                serde_json::json!({
                    "personality_type_id": row.personality_type_id,
                    "personality_instance_id": row.personality_instance_id.into_inner(),
                    "current_self_perspective_memory_id":
                        row.current_self_perspective_memory_id.into_inner(),
                    "display_name": row.display_name,
                    "status": row.status,
                })
            })
            .collect();
        ctx.record_read(
            rows.iter()
                .map(|row| (row.current_self_perspective_memory_id, WakeChainDepth::zero())),
        )
        .await;
        Ok(PersonalityToolResult::ok(serde_json::json!({
            "instances": entries,
        })))
    }
}

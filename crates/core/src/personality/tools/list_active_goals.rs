//! `core/list_active_goals` substrate tool — list active Goals linked to
//! the personality's current Self-Perspective through `core/inspires`.

use std::sync::OnceLock;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::ProtocolError;
use crate::personality::{PersonalityTool, PersonalityToolContext, PersonalityToolResult};
use crate::{GoalId, SchemaId, SchemaVersion};

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveGoalSummary {
    pub goal_id: GoalId,
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub title: String,
    pub text: String,
    pub payload: Vec<u8>,
}

#[async_trait]
impl PersonalityTool for ListActiveGoalsTool {
    fn tool_id(&self) -> &'static str {
        "core/list_active_goals"
    }

    fn description(&self) -> &'static str {
        "List active goals connected to this personality's current Self-Perspective."
    }

    fn args_schema(&self) -> serde_json::Value {
        args_schema_value().clone()
    }

    async fn invoke(
        &self,
        ctx: &PersonalityToolContext<'_>,
        _args: serde_json::Value,
    ) -> Result<PersonalityToolResult, ProtocolError> {
        let goals = ctx
            .engine
            .storage()
            .list_active_goals(ctx.owner, ctx.current_root_perspective_memory_id, 100)
            .await
            .map_err(|e| ProtocolError::internal(e.to_string()))?;
        Ok(PersonalityToolResult::ok(serde_json::json!({
            "goals": goals,
        })))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::auth::NoAuth;
    use crate::mcp::HandleTable;
    use crate::verbs::query::MemoryStore;
    use crate::{
        Engine, FlavorRegistry, MemoryId, OrgId, Owner, PersonalityInstanceId, Principal, UserId,
        WakeChainDepth,
    };

    #[tokio::test]
    async fn list_active_goals_returns_storage_payload() {
        let owner = Owner {
            principal: Principal::User(UserId::new(uuid::Uuid::now_v7())),
            org_id: OrgId::new(uuid::Uuid::now_v7()),
        };
        let engine = Engine::new(
            FlavorRegistry::new().freeze(),
            MemoryStore::new(),
            Box::new(NoAuth::new(owner.principal.clone(), owner.clone())),
        );
        let palette = Vec::new();
        let ctx = PersonalityToolContext::new(
            &engine,
            &owner,
            "test/personality",
            PersonalityInstanceId::new(uuid::Uuid::now_v7()),
            MemoryId::new(uuid::Uuid::now_v7()),
            MemoryId::new(uuid::Uuid::now_v7()),
            WakeChainDepth::new(0),
            Vec::new(),
            Vec::new(),
            &palette,
            Arc::new(HandleTable::new()),
        );

        let result = ListActiveGoalsTool
            .invoke(&ctx, serde_json::json!({}))
            .await
            .expect("tool succeeds");
        assert!(!result.is_error);
        assert_eq!(result.content, serde_json::json!({ "goals": [] }));
    }
}

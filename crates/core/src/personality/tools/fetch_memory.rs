//! `core/fetch_memory` substrate tool — read a single memory by handle.

use std::sync::OnceLock;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::SchemaId;
use crate::error::ProtocolError;
use crate::personality::{
    PersonalityTool, PersonalityToolContext, PersonalityToolResult, SidecarSpec,
};
use crate::verbs::schema::PayloadKind;

#[derive(Debug, Default)]
pub struct FetchMemoryTool;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FetchMemoryArgs {
    /// Handle of the memory to load (e.g., `N1`).
    pub memory: String,
}

fn args_schema_value() -> &'static serde_json::Value {
    static SCHEMA: OnceLock<serde_json::Value> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        serde_json::to_value(schemars::schema_for!(FetchMemoryArgs))
            .expect("FetchMemoryArgs schema serializes")
    })
}

#[async_trait]
impl PersonalityTool for FetchMemoryTool {
    fn tool_id(&self) -> &'static str {
        "core/fetch_memory"
    }

    fn description(&self) -> &'static str {
        "Fetch one memory by handle. Returns kind, schema_id, schema_version, \
         text, payload, and wake_chain_depth."
    }

    fn args_schema(&self) -> serde_json::Value {
        args_schema_value().clone()
    }

    async fn invoke(
        &self,
        ctx: &PersonalityToolContext<'_>,
        args: serde_json::Value,
    ) -> Result<PersonalityToolResult, ProtocolError> {
        let parsed: FetchMemoryArgs = match serde_json::from_value(args) {
            Ok(v) => v,
            Err(e) => {
                return Ok(PersonalityToolResult::error(serde_json::json!({
                    "error": format!("invalid args: {e}"),
                })));
            }
        };
        let memory_id = match ctx.handles.resolve_memory(&parsed.memory) {
            Ok(id) => id,
            Err(e) => {
                return Ok(PersonalityToolResult::error(serde_json::json!({
                    "error": e.to_string(),
                })));
            }
        };
        let sidecars: Vec<SidecarSpec> = ctx
            .engine
            .registry()
            .list()
            .iter()
            .filter(|s| {
                matches!(
                    s.kind,
                    PayloadKind::Fact | PayloadKind::Abstraction | PayloadKind::Perspective
                ) && s.sidecar_table.is_some()
            })
            .map(|s| SidecarSpec {
                schema_id: SchemaId::new(s.schema_id.as_str().to_string()),
                sidecar_table: s.sidecar_table.clone().unwrap(),
            })
            .collect();
        let snapshot = ctx
            .engine
            .storage()
            .load_memory_by_id(ctx.owner, memory_id, &sidecars)
            .await
            .map_err(|e| ProtocolError::internal(format!("load_memory_by_id: {e}")))?;
        let Some(snapshot) = snapshot else {
            return Ok(PersonalityToolResult::error(serde_json::json!({
                "error": "memory not found",
                "memory": parsed.memory,
            })));
        };
        ctx.record_read([(snapshot.memory_id, snapshot.wake_chain_depth)])
            .await;
        let handle = ctx.handles.assign_memory(snapshot.memory_id);
        Ok(PersonalityToolResult::ok(serde_json::json!({
            "memory": handle.as_str(),
            "kind": snapshot.kind,
            "schema_id": snapshot.schema_id.as_str(),
            "schema_version": snapshot.schema_version.into_inner(),
            "text": snapshot.text,
            "wake_chain_depth": snapshot.wake_chain_depth.into_inner(),
            "payload": snapshot.payload_json,
        })))
    }
}

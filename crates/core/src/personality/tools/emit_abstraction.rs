//! `core/emit_abstraction` substrate tool — write an Abstraction memory
//! with auto-wired Provenance + computed wake_chain_depth.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;

use super::shared::{
    PROMPT_VERSION, derive_text, emit_personality_memory, normalize_handle_refs_in_payload,
};
use crate::error::ProtocolError;
use crate::mcp::schema::mcp_tool_schema;
use crate::personality::{
    PersonalityMemoryDraft, PersonalityMemoryKind, PersonalityTool, PersonalityToolContext,
    PersonalityToolResult, authorization::authorize_emit,
};
use crate::verbs::schema::PayloadKind;
use crate::{SchemaId, SchemaVersion};

#[derive(Debug, Default)]
pub struct EmitAbstractionTool;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct EmitAbstractionArgs {
    #[schemars(
        description = "Registered Abstraction schema id to emit. Raw internal tool field; provider-facing wakes usually use typed emit wrappers instead."
    )]
    pub schema_id: String,
    #[schemars(
        description = "Registered Abstraction schema version to emit. Raw internal tool field; provider-facing wakes usually use typed emit wrappers instead."
    )]
    pub schema_version: u32,
    #[schemars(
        description = "Typed Abstraction payload object for the selected schema. Provenance is auto-wired from the wake trigger and reads."
    )]
    pub payload: serde_json::Value,
    #[serde(default)]
    #[schemars(
        description = "Optional authored memory text. Omit or null to derive text from the typed payload."
    )]
    pub text: Option<String>,
}

#[async_trait]
impl PersonalityTool for EmitAbstractionTool {
    fn tool_id(&self) -> &'static str {
        "core/emit_abstraction"
    }

    fn description(&self) -> &'static str {
        "Emit one Abstraction memory. Provenance and wake_chain_depth are \
         auto-wired from the triggering event and any memories the personality \
         read this wake."
    }

    fn args_schema(&self) -> serde_json::Value {
        mcp_tool_schema::<EmitAbstractionArgs>()
    }

    async fn invoke(
        &self,
        ctx: &PersonalityToolContext<'_>,
        args: serde_json::Value,
    ) -> Result<PersonalityToolResult, ProtocolError> {
        let mut parsed: EmitAbstractionArgs = match serde_json::from_value(args) {
            Ok(v) => v,
            Err(e) => {
                return Ok(PersonalityToolResult::error(serde_json::json!({
                    "error": format!("invalid args: {e}"),
                })));
            }
        };
        if let Err(err) = authorize_emit(&parsed.schema_id, &ctx.writeable_schemas) {
            return Ok(PersonalityToolResult::error(serde_json::json!({
                "error": err.to_string(),
            })));
        }
        let schema_id = SchemaId::new(parsed.schema_id.clone());
        let schema_version = SchemaVersion::new(parsed.schema_version);
        let info = ctx
            .engine
            .registry()
            .lookup_payload(&schema_id, schema_version, PayloadKind::Abstraction)
            .ok_or_else(|| {
                ProtocolError::internal(format!(
                    "schema {} v{} not registered as Abstraction",
                    parsed.schema_id, parsed.schema_version,
                ))
            })?;
        let sidecar_table = info.sidecar_table.as_deref().ok_or_else(|| {
            ProtocolError::internal(format!("schema {} has no sidecar", parsed.schema_id,))
        })?;
        normalize_handle_refs_in_payload(ctx, &mut parsed.payload);
        ctx.engine
            .registry()
            .validate_payload(
                &schema_id,
                schema_version,
                PayloadKind::Abstraction,
                &parsed.payload,
            )
            .map_err(|e| ProtocolError::internal(format!("invalid payload: {e}")))?;
        let text = parsed
            .text
            .clone()
            .unwrap_or_else(|| derive_text(&parsed.payload));
        let embed = ctx
            .engine
            .embed_client()
            .ok_or_else(|| ProtocolError::internal("embedding client not wired into engine"))?;
        let embedding = embed
            .embed(&text)
            .await
            .map_err(|e| ProtocolError::internal(format!("embed: {e}")))?;
        let (provenance, depth) = ctx.snapshot_provenance().await;
        let draft = PersonalityMemoryDraft {
            kind: PersonalityMemoryKind::Abstraction,
            schema_id,
            schema_version,
            text,
            typed_payload: parsed.payload,
            provenance,
            embedding,
            embedding_model_id: embed.model_id().to_string(),
        };
        let memory_id =
            emit_personality_memory(ctx, sidecar_table, depth, PROMPT_VERSION, &draft).await?;
        let handle = ctx.handles.assign_abstraction_memory(memory_id);
        Ok(PersonalityToolResult::ok(serde_json::json!({
            "memory": handle.as_str(),
            "wake_chain_depth": depth.into_inner(),
        })))
    }
}

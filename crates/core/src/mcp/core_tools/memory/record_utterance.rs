use crate::mcp::{McpTool, McpToolCtx, McpToolError};
use crate::verbs::event_ingest::EventDraft;
use crate::{
    FactPayload, Role, SchemaId, SchemaVersion, SourceBatchId, SourceId, canonical_json_bytes,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{Speaker, UtteranceV1};

const SOURCE_ID: &str = "core/conversation";
const UTTERANCE_NAMESPACE: uuid::Uuid = uuid::Uuid::from_bytes([
    0x23, 0xa5, 0x64, 0x58, 0x2b, 0x71, 0x49, 0x29, 0x8b, 0x7d, 0xb9, 0x49, 0xd2, 0x52, 0xe2, 0x11,
]);

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RecordUtteranceArgs {
    pub speaker: Speaker,
    pub conversation_id: String,
    pub text: String,
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RecordUtteranceOutput {
    pub handle: String,
    pub idempotent_replay: bool,
}

#[derive(Debug)]
pub struct RecordUtteranceTool;

impl McpTool for RecordUtteranceTool {
    const NAME: &'static str = "core/record_utterance";
    const DESCRIPTION: &'static str =
        "Append one conversation utterance as a personality-authored Fact.";
    type Args = RecordUtteranceArgs;
    type Output = RecordUtteranceOutput;

    fn call(
        ctx: McpToolCtx,
        args: RecordUtteranceArgs,
    ) -> futures::future::BoxFuture<'static, Result<RecordUtteranceOutput, McpToolError>> {
        Box::pin(async move {
            let conversation_id = args.conversation_id.trim();
            if conversation_id.is_empty() {
                return Err(McpToolError::InvalidInput(
                    "conversation_id must be non-empty".into(),
                ));
            }
            let text = args.text.trim();
            if text.is_empty() || text.chars().count() > 20_000 {
                return Err(McpToolError::InvalidInput(
                    "text must be 1..=20000 chars".into(),
                ));
            }

            let payload = UtteranceV1 {
                speaker: args.speaker,
                conversation_id: conversation_id.to_string(),
                text: text.to_string(),
            };
            let payload_value = serde_json::to_value(&payload)
                .map_err(|err| McpToolError::InvalidInput(err.to_string()))?;
            let payload_bytes = canonical_json_bytes(&payload_value);
            let source_instance_id = args
                .idempotency_key
                .as_deref()
                .map_or_else(uuid::Uuid::now_v7, |key| {
                    uuid::Uuid::new_v5(&UTTERANCE_NAMESPACE, key.as_bytes())
                });
            let source_id = format!("{SOURCE_ID}/{source_instance_id}");

            let observed_at = time::OffsetDateTime::now_utc();
            let draft = EventDraft {
                source_id: SourceId::new(source_id),
                source_batch_id: SourceBatchId::new(uuid::Uuid::now_v7()),
                principal: ctx.owner.principal.clone(),
                org_id: Some(ctx.owner.org_id),
                author_personality_instance_id: ctx.author.personality_instance_id,
                schema_id: SchemaId::new(UtteranceV1::SCHEMA_ID.into()),
                schema_version: SchemaVersion::new(UtteranceV1::SCHEMA_VERSION),
                payload: payload_bytes,
                rendered_text: None,
                observed_at,
                occurred_at: observed_at,
                citation: None,
            };

            let engine = ctx
                .engine()
                .ok_or_else(|| McpToolError::InvalidInput("engine required".into()))?;
            let authorized = engine
                .authorize_event_ingest(&ctx.authz, Role::GraphWrite, draft)
                .map_err(|err| McpToolError::Other(err.to_string()))?;
            let outcome = engine
                .storage()
                .ingest_event_with_sidecar(
                    &authorized,
                    UtteranceV1::sidecar_table().expect("UtteranceV1 has a sidecar table"),
                    &payload_value,
                )
                .await?;
            if let Err(err) = engine
                .ensure_fact_embedding(&ctx.owner, outcome.memory_id)
                .await
            {
                tracing::warn!(
                    memory_id = %outcome.memory_id.into_inner(),
                    error = %err,
                    "best-effort Fact embedding failed after core/record_utterance",
                );
            }

            Ok(RecordUtteranceOutput {
                handle: ctx.format_fact_memory(outcome.memory_id),
                idempotent_replay: outcome.idempotent_replay,
            })
        })
    }
}

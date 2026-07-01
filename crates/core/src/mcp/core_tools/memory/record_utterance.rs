use crate::mcp::{McpTool, McpToolCtx, McpToolError};
use crate::protocol::tool as protocol_tool;
use crate::verbs::fact_ingest::FactWriteCommand;
use crate::{Relation, SourceBatchId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{SidecarPayload, Speaker, UtteranceV1};

const SOURCE_ID: &str = "core/conversation";
const UTTERANCE_NAMESPACE: uuid::Uuid = uuid::Uuid::from_bytes([
    0x23, 0xa5, 0x64, 0x58, 0x2b, 0x71, 0x49, 0x29, 0x8b, 0x7d, 0xb9, 0x49, 0xd2, 0x52, 0xe2, 0x11,
]);

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RecordUtteranceArgs {
    #[schemars(description = "Who produced the utterance: `user` or `agent`.")]
    pub speaker: Speaker,
    #[schemars(description = "Stable id grouping the utterances of one conversation.")]
    pub conversation_id: String,
    #[schemars(description = "The utterance text, 1 to 20000 chars.")]
    pub text: String,
    #[schemars(
        description = "Optional stable idempotency key; replaying the same key is a no-op, not a duplicate."
    )]
    pub idempotency_key: Option<String>,
    #[serde(default)]
    #[schemars(description = "Memory space key from core_memory_spaces. Omit for current owner.")]
    pub space: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RecordUtteranceOutput {
    pub handle: String,
    pub idempotent_replay: bool,
}

#[derive(Debug)]
pub struct RecordUtteranceTool;

impl McpTool for RecordUtteranceTool {
    const NAME: &'static str = protocol_tool::CORE_RECORD_UTTERANCE;
    const DESCRIPTION: &'static str = "Append one raw conversation turn (utterance) as a Fact. Use `core_remember` for distilled observations rather than verbatim transcript.";
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

            let space = super::super::memory_spaces::resolve_space_owner(
                &ctx,
                args.space.as_deref(),
                super::super::memory_spaces::SpaceDefault::Current,
            )?;
            let authz = ctx
                .authz
                .clone()
                .narrowed_to_owner(space.owner)
                .ok_or_else(|| McpToolError::NotAuthorized("memory space write".into()))?;
            let payload = UtteranceV1 {
                speaker: args.speaker,
                conversation_id: conversation_id.to_string(),
                text: text.to_string(),
            };
            let source_instance_id = args
                .idempotency_key
                .as_deref()
                .map_or_else(uuid::Uuid::now_v7, |key| {
                    uuid::Uuid::new_v5(&UTTERANCE_NAMESPACE, key.as_bytes())
                });
            let source_id = format!("{SOURCE_ID}/{source_instance_id}");

            let observed_at = time::OffsetDateTime::now_utc();
            let draft = FactWriteCommand::from_payload(
                source_id,
                SourceBatchId::new(uuid::Uuid::now_v7()),
                &payload,
                observed_at,
            );

            let engine = ctx
                .engine()
                .ok_or_else(|| McpToolError::InvalidInput("engine required".into()))?;
            let embedding_client = engine.embed_client();
            let embedding_model_id = embedding_client.as_ref().map(|client| client.model_id());
            let authorized = engine
                .authorize_fact_ingest(&authz, Relation::Editor, draft)
                .await
                .map_err(|err| McpToolError::Other(err.to_string()))?;
            let outcome = engine
                .ingest_fact_with_typed_sidecar(
                    &authorized,
                    &SidecarPayload::fact(payload.clone()),
                    embedding_model_id,
                )
                .await?;

            Ok(RecordUtteranceOutput {
                handle: ctx.format_fact_memory(outcome.memory_id),
                idempotent_replay: outcome.idempotent_replay,
            })
        })
    }
}

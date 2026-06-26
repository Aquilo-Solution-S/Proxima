use crate::mcp::{McpTool, McpToolCtx, McpToolError};
use crate::verbs::event_ingest::EventDraft;
use crate::{MemoryAction, Role, SourceBatchId};
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
    const NAME: &'static str = "core_record_utterance";
    const DESCRIPTION: &'static str = "Append one raw conversation turn (utterance) as a personality-authored Fact. Use `core_remember` for distilled observations rather than verbatim transcript.";
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
            if !ctx
                .authz
                .allows_memory_action(&space.owner, MemoryAction::Write)
            {
                return Err(crate::error::ProtocolError::forbidden(format!(
                    "requires memory.write on space {}",
                    space.key
                ))
                .into());
            }

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
            let mut draft = EventDraft::from_payload(
                &space.owner,
                source_id,
                SourceBatchId::new(uuid::Uuid::now_v7()),
                &payload,
                observed_at,
            );
            if let Some(author) = ctx.author.personality_instance_id {
                draft = draft.author_personality(author);
            }

            let engine = ctx
                .engine()
                .ok_or_else(|| McpToolError::InvalidInput("engine required".into()))?;
            let embedding_client = engine.embed_client();
            let embedding_model_id = embedding_client.as_ref().map(|client| client.model_id());
            let authorized = engine
                .authorize_event_ingest(&ctx.authz, Role::GraphWrite, draft)
                .map_err(|err| McpToolError::Other(err.to_string()))?;
            let outcome = engine
                .storage()
                .ingest_event_with_typed_sidecar(
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

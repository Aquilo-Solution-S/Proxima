use crate::mcp::{McpTool, McpToolCtx, McpToolError};
use crate::verbs::event_ingest::{EventDraft, InlineCitationMappingDraft, InlineCitedObjectDraft};
use crate::{Role, SchemaId, SchemaVersion, SourceBatchId, canonical_json_bytes};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{AgentNoteV1, SidecarPayload};

use super::util::normalize_tags;

const SOURCE_ID: &str = "core/agent";
const NOTE_NAMESPACE: uuid::Uuid = uuid::Uuid::from_bytes([
    0x91, 0x3e, 0xa1, 0x4c, 0x12, 0x9b, 0x4f, 0xa1, 0x86, 0x2c, 0xb7, 0x2e, 0x18, 0x5d, 0xc7, 0x77,
]);

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RememberArgs {
    #[schemars(description = "Short title for the agent-observed Fact, 1 to 160 chars.")]
    pub title: String,
    #[schemars(description = "Body text for the agent-observed Fact, 1 to 20000 chars.")]
    pub body: String,
    #[serde(default)]
    #[schemars(
        description = "Optional normalized tags for later search. Use `[]` when no tags are needed."
    )]
    pub tags: Vec<String>,
    #[schemars(
        description = "Optional stable idempotency key for replay-safe Fact creation. Omit or null for a fresh Fact."
    )]
    pub idempotency_key: Option<String>,
    #[serde(default)]
    #[schemars(description = "Optional typed inline citation for an external artifact.")]
    pub citation: Option<RememberCitation>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RememberCitation {
    pub object_schema_id: String,
    pub object_schema_version: u32,
    pub object_payload: serde_json::Value,
    pub mapping_schema_id: String,
    pub mapping_schema_version: u32,
    pub mapping_payload: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct RememberOutput {
    pub handle: String,
    pub idempotent_replay: bool,
}

#[derive(Debug)]
pub struct RememberTool;

impl McpTool for RememberTool {
    const NAME: &'static str = "core/remember";
    const DESCRIPTION: &'static str =
        "Append an agent-observed Fact. Optional idempotency_key makes replay stable.";
    type Args = RememberArgs;
    type Output = RememberOutput;

    fn call(
        ctx: McpToolCtx,
        args: RememberArgs,
    ) -> futures::future::BoxFuture<'static, Result<RememberOutput, McpToolError>> {
        Box::pin(async move {
            let title = args.title.trim();
            let body = args.body.trim();
            if title.is_empty() || title.chars().count() > 160 {
                return Err(McpToolError::InvalidInput(
                    "title must be 1..=160 chars".into(),
                ));
            }
            if body.is_empty() || body.chars().count() > 20_000 {
                return Err(McpToolError::InvalidInput(
                    "body must be 1..=20000 chars".into(),
                ));
            }
            let tags = normalize_tags(args.tags)?;
            let note_id = args
                .idempotency_key
                .as_deref()
                .map_or_else(uuid::Uuid::now_v7, |key| {
                    uuid::Uuid::new_v5(&NOTE_NAMESPACE, key.as_bytes())
                });
            let payload = AgentNoteV1 {
                note_id,
                title: title.to_string(),
                body: body.to_string(),
                tags,
                idempotency_key: args.idempotency_key,
            };
            let observed_at = time::OffsetDateTime::now_utc();
            let mut draft = EventDraft::from_payload(
                &ctx.owner,
                SOURCE_ID,
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
            let outcome = if let Some(citation) = args.citation {
                let cited_object = InlineCitedObjectDraft {
                    schema_id: SchemaId::new(citation.object_schema_id),
                    schema_version: SchemaVersion::new(citation.object_schema_version),
                    payload_bytes: encode_json_payload(&citation.object_payload),
                };
                let mapping = InlineCitationMappingDraft {
                    schema_id: SchemaId::new(citation.mapping_schema_id),
                    schema_version: SchemaVersion::new(citation.mapping_schema_version),
                    payload_bytes: encode_json_payload(&citation.mapping_payload),
                };
                let authorized = engine
                    .authorize_fact_with_citation(
                        &ctx.authz,
                        Role::GraphWrite,
                        draft,
                        cited_object,
                        mapping,
                    )
                    .map_err(|err| McpToolError::Other(err.to_string()))?;
                engine
                    .storage()
                    .ingest_fact_with_citation_and_typed_sidecar(
                        &authorized,
                        &SidecarPayload::fact(payload.clone()),
                        embedding_model_id,
                    )
                    .await?
            } else {
                let authorized = engine
                    .authorize_event_ingest(&ctx.authz, Role::GraphWrite, draft)
                    .map_err(|err| McpToolError::Other(err.to_string()))?;
                engine
                    .storage()
                    .ingest_event_with_typed_sidecar(
                        &authorized,
                        &SidecarPayload::fact(payload.clone()),
                        embedding_model_id,
                    )
                    .await?
            };

            Ok(RememberOutput {
                handle: ctx.format_fact_memory(outcome.memory_id),
                idempotent_replay: outcome.idempotent_replay,
            })
        })
    }
}

fn encode_json_payload(value: &serde_json::Value) -> Vec<u8> {
    canonical_json_bytes(value)
}

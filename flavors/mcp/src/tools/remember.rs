use proxima_core::mcp::{McpTool, McpToolCtx, McpToolError};
use proxima_core::verbs::event_ingest::{CitationMappingHint, CitedObjectHint, EventDraft};
use proxima_core::{FactPayload, SchemaId, SchemaVersion, SourceBatchId, SourceId};
use proxima_storage_pg::verbs::event_ingest::ingest_event_in_tx;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::AgentNoteV1;

use super::util::{map_storage, normalize_tags};

const SOURCE_ID: &str = "proxima-mcp/agent";
const NOTE_CITED_OBJECT_SCHEMA: &str = "proxima-mcp/agent-note-object-v1";
const NOTE_CITATION_MAPPING_SCHEMA: &str = "proxima-mcp/agent-note-whole-v1";
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
}

#[derive(Debug, Serialize)]
pub struct RememberOutput {
    pub handle: String,
    pub idempotent_replay: bool,
}

#[derive(Debug)]
pub struct RememberTool;

impl McpTool for RememberTool {
    const NAME: &'static str = "proxima-mcp/proxima_remember";
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
            let mut payload_bytes = Vec::new();
            ciborium::ser::into_writer(&payload, &mut payload_bytes)
                .map_err(|err| McpToolError::InvalidInput(err.to_string()))?;
            let observed_at = time::OffsetDateTime::now_utc();
            let draft = EventDraft {
                source_id: SourceId::new(SOURCE_ID),
                source_batch_id: SourceBatchId::new(uuid::Uuid::now_v7()),
                owner: ctx.owner.clone(),
                schema_id: SchemaId::new(AgentNoteV1::SCHEMA_ID.into()),
                schema_version: SchemaVersion::new(AgentNoteV1::SCHEMA_VERSION),
                payload: payload_bytes,
                observed_at,
                occurred_at: observed_at,
                cited_object: CitedObjectHint {
                    schema_id: SchemaId::new(NOTE_CITED_OBJECT_SCHEMA.into()),
                    schema_version: SchemaVersion::new(1),
                    content_hash: *blake3::hash(body.as_bytes()).as_bytes(),
                },
                citation_mapping: CitationMappingHint {
                    schema_id: SchemaId::new(NOTE_CITATION_MAPPING_SCHEMA.into()),
                    schema_version: SchemaVersion::new(1),
                },
            };

            let mut tx = ctx.pool.begin().await.map_err(map_storage)?;
            let outcome = ingest_event_in_tx(&mut tx, &draft)
                .await
                .map_err(McpToolError::Storage)?;
            if !outcome.idempotent_replay {
                sqlx::query(
                    "INSERT INTO proxima_mcp.agent_note_v1
                       (memory_id, note_id, title, body, tags, idempotency_key)
                     VALUES ($1, $2, $3, $4, $5, $6)",
                )
                .bind(outcome.memory_id.into_inner())
                .bind(payload.note_id)
                .bind(&payload.title)
                .bind(&payload.body)
                .bind(&payload.tags)
                .bind(&payload.idempotency_key)
                .execute(&mut *tx)
                .await
                .map_err(map_storage)?;
            }
            tx.commit().await.map_err(map_storage)?;

            Ok(RememberOutput {
                handle: ctx.format_fact_memory(outcome.memory_id),
                idempotent_replay: outcome.idempotent_replay,
            })
        })
    }
}

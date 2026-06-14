use proxima_core::mcp::{McpTool, McpToolCtx, McpToolError};
use proxima_core::verbs::event_ingest::{
    EventDraft, InlineCitationMappingDraft, InlineCitedObjectDraft,
};
use proxima_core::{
    EventIngestOutcome, FactPayload, Role, SchemaId, SchemaVersion, SourceBatchId, SourceId,
    StorageError, canonical_json_bytes,
};
use proxima_storage_pg::verbs::event_ingest::{
    ingest_event_with_sidecar_in_tx, ingest_fact_with_citation_in_tx,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, Transaction};

use crate::AgentNoteV1;

use super::util::{map_storage, normalize_tags};

const SOURCE_ID: &str = "proxima-agent-memory/agent";
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
    const NAME: &'static str = "proxima-agent-memory/proxima_remember";
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
            let sidecar = payload.clone();
            let payload_value = serde_json::to_value(payload)
                .map_err(|err| McpToolError::InvalidInput(err.to_string()))?;
            let payload_bytes = canonical_json_bytes(&payload_value);
            let observed_at = time::OffsetDateTime::now_utc();
            let draft = EventDraft {
                source_id: SourceId::new(SOURCE_ID),
                source_batch_id: SourceBatchId::new(uuid::Uuid::now_v7()),
                principal: ctx.owner.principal.clone(),
                org_id: Some(ctx.owner.org_id),
                author_personality_instance_id: ctx.author.personality_instance_id,
                schema_id: SchemaId::new(AgentNoteV1::SCHEMA_ID.into()),
                schema_version: SchemaVersion::new(AgentNoteV1::SCHEMA_VERSION),
                payload: payload_bytes,
                rendered_text: None,
                observed_at,
                occurred_at: observed_at,
                citation: None,
            };

            let engine = ctx
                .engine
                .as_ref()
                .ok_or_else(|| McpToolError::InvalidInput("engine required".into()))?;
            let mut tx = ctx.pool.begin().await.map_err(map_storage)?;
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
                ingest_fact_with_citation_in_tx(&mut tx, &authorized, |tx, outcome| {
                    Box::pin(async move { insert_agent_note_sidecar(tx, outcome, &sidecar).await })
                })
                .await
                .map_err(McpToolError::Storage)?
            } else {
                let authorized = engine
                    .authorize_event_ingest(&ctx.authz, Role::GraphWrite, draft)
                    .map_err(|err| McpToolError::Other(err.to_string()))?;
                ingest_event_with_sidecar_in_tx(&mut tx, &authorized, |tx, outcome| {
                    Box::pin(async move { insert_agent_note_sidecar(tx, outcome, &sidecar).await })
                })
                .await
                .map_err(McpToolError::Storage)?
            };
            tx.commit().await.map_err(map_storage)?;
            ensure_fact_embedding_best_effort(engine, &ctx.owner, &outcome).await;

            Ok(RememberOutput {
                handle: ctx.format_fact_memory(outcome.memory_id),
                idempotent_replay: outcome.idempotent_replay,
            })
        })
    }
}

async fn ensure_fact_embedding_best_effort(
    engine: &proxima_core::Engine,
    owner: &proxima_core::Owner,
    outcome: &EventIngestOutcome,
) {
    if let Err(err) = engine.ensure_fact_embedding(owner, outcome.memory_id).await {
        tracing::warn!(
            memory_id = %outcome.memory_id.into_inner(),
            error = %err,
            "best-effort Fact embedding failed after proxima_remember",
        );
    }
}

fn encode_json_payload(value: &serde_json::Value) -> Vec<u8> {
    canonical_json_bytes(value)
}

async fn insert_agent_note_sidecar(
    tx: &mut Transaction<'_, Postgres>,
    outcome: &EventIngestOutcome,
    payload: &AgentNoteV1,
) -> Result<(), StorageError> {
    sqlx::query(
        "INSERT INTO proxima_agent_memory.agent_note_v1
           (memory_id, note_id, title, body, tags, idempotency_key)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(outcome.memory_id.into_inner())
    .bind(payload.note_id)
    .bind(&payload.title)
    .bind(&payload.body)
    .bind(&payload.tags)
    .bind(&payload.idempotency_key)
    .execute(&mut **tx)
    .await
    .map_err(|err| StorageError::Internal(err.to_string()))?;
    Ok(())
}

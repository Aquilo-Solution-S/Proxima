use crate::mcp::{McpTool, McpToolCtx, McpToolError};
use crate::protocol::tool as protocol_tool;
use crate::verbs::fact_ingest::{
    FactWriteCommand, InlineCitationMappingDraft, InlineCitedObjectDraft,
};
use crate::{Relation, SchemaId, SchemaVersion, SourceBatchId, canonical_json_bytes};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{AgentNoteV1, SidecarPayload};

use super::util::{normalize_tags, validate_idempotency_key};

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
        description = "Optional stable note key. An exact replay (same title/body/tags) returns the existing Fact. Reusing the key with changed content appends a new Fact version and advances the note head; it does not overwrite."
    )]
    pub idempotency_key: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Optional typed inline citation linking this Fact to an external artifact; the object/mapping schemas must be registered (`CitedObject`/`CitationMapping` kinds — discover them via the `proxima://schemas{?kind}` resource)."
    )]
    pub citation: Option<RememberCitation>,
    #[serde(default)]
    #[schemars(description = "Memory space key from core_memory_spaces. Omit for current owner.")]
    pub space: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RememberCitation {
    #[schemars(
        description = "Schema id of the cited external object (a registered `CitedObject` schema — discover via `proxima://schemas{?kind}`)."
    )]
    pub object_schema_id: String,
    #[schemars(description = "Version of the cited-object schema.")]
    pub object_schema_version: u32,
    #[schemars(description = "The cited object payload as JSON, conforming to its schema.")]
    pub object_payload: serde_json::Value,
    #[schemars(
        description = "Schema id of the citation mapping (a registered `CitationMapping` schema — discover via `proxima://schemas{?kind}`)."
    )]
    pub mapping_schema_id: String,
    #[schemars(description = "Version of the citation-mapping schema.")]
    pub mapping_schema_version: u32,
    #[schemars(
        description = "The citation mapping payload as JSON (e.g. a locator within the cited object), conforming to its schema."
    )]
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
    const NAME: &'static str = protocol_tool::CORE_REMEMBER;
    const DESCRIPTION: &'static str = "Append an agent-observed Fact. Optional idempotency_key collapses only exact replays with the same content; changed content with the same key writes a new version and advances the note head pointer. core_search_memories returns heads by default; pass supersession=all for full history.";
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
            validate_idempotency_key(args.idempotency_key.as_deref())?;
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
            let source_batch_id = SourceBatchId::new(uuid::Uuid::now_v7());
            let draft =
                FactWriteCommand::from_payload(SOURCE_ID, source_batch_id, &payload, observed_at);

            let engine = ctx
                .engine()
                .ok_or_else(|| McpToolError::InvalidInput("engine required".into()))?;
            let embedding_client = engine.embed_client();
            let embedding_model_id = embedding_client.as_ref().map(|client| client.model_id());
            let outcome = if let Some(citation) = args.citation {
                let (cited_object, mapping) = remember_citation_drafts(citation);
                let authorized = engine
                    .authorize_fact_with_citation(
                        &authz,
                        Relation::Editor,
                        draft,
                        cited_object,
                        mapping,
                    )
                    .await
                    .map_err(|err| McpToolError::Other(err.to_string()))?;
                engine
                    .ingest_fact_with_citation_and_typed_sidecar(
                        &authorized,
                        &SidecarPayload::fact(payload.clone()),
                        embedding_model_id,
                    )
                    .await?
            } else {
                let authorized = engine
                    .authorize_fact_ingest(&authz, Relation::Editor, draft)
                    .await
                    .map_err(|err| McpToolError::Other(err.to_string()))?;
                engine
                    .ingest_fact_with_typed_sidecar(
                        &authorized,
                        &SidecarPayload::fact(payload.clone()),
                        embedding_model_id,
                    )
                    .await?
            };

            if !outcome.idempotent_replay {
                engine
                    .close_batch(&authz, space.owner, source_batch_id)
                    .await
                    .map_err(|err| McpToolError::Other(err.to_string()))?;
            }

            Ok(RememberOutput {
                handle: ctx.format_fact_memory(outcome.memory_id),
                idempotent_replay: outcome.idempotent_replay,
            })
        })
    }
}

fn remember_citation_drafts(
    citation: RememberCitation,
) -> (InlineCitedObjectDraft, InlineCitationMappingDraft) {
    (
        InlineCitedObjectDraft {
            schema_id: SchemaId::new(citation.object_schema_id),
            schema_version: SchemaVersion::new(citation.object_schema_version),
            payload_bytes: encode_json_payload(&citation.object_payload),
        },
        InlineCitationMappingDraft {
            schema_id: SchemaId::new(citation.mapping_schema_id),
            schema_version: SchemaVersion::new(citation.mapping_schema_version),
            payload_bytes: encode_json_payload(&citation.mapping_payload),
        },
    )
}

fn encode_json_payload(value: &serde_json::Value) -> Vec<u8> {
    canonical_json_bytes(value)
}

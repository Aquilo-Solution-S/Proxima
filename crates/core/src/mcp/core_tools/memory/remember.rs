use crate::mcp::{McpTool, McpToolCtx, McpToolError};
use crate::protocol::tool as protocol_tool;
use crate::tool::validate_trimmed_len;
use crate::verbs::fact_ingest::{
    FactWriteCommand, InlineCitationMappingDraft, InlineCitedObjectDraft,
};
use crate::{Relation, SchemaId, SchemaVersion, canonical_json_bytes};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{AgentNoteV1, SidecarPayload};

use super::util::{normalize_idempotency_key, normalize_tags};

const SOURCE_ID: &str = "core/agent";
const NOTE_NAMESPACE: uuid::Uuid = uuid::Uuid::from_bytes([
    0x91, 0x3e, 0xa1, 0x4c, 0x12, 0x9b, 0x4f, 0xa1, 0x86, 0x2c, 0xb7, 0x2e, 0x18, 0x5d, 0xc7, 0x77,
]);

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RememberArgs {
    #[schemars(
        length(max = 240),
        description = "Short title for the agent-observed Fact, 1 to 240 chars. Leading and trailing whitespace is removed before the length check."
    )]
    pub title: String,
    #[schemars(
        length(max = 20000),
        description = "Body text for the agent-observed Fact, 1 to 20000 chars. Leading and trailing whitespace is removed before the length check."
    )]
    pub body: String,
    #[serde(default)]
    #[schemars(
        length(max = 16),
        description = "Optional tags for later search, at most 16. Each is stored trimmed and lowercased, so `Rust` is stored and matched as `rust`. Use `[]` when no tags are needed."
    )]
    pub tags: Vec<String>,
    #[schemars(
        description = "Optional stable note key. An exact replay (same title/body/tags) returns the existing Fact. Reusing the key with changed content appends a new Fact version and advances the note head; it does not overwrite."
    )]
    pub idempotency_key: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Optional typed citation linking this Fact to an external artifact. Either inline (object_schema_id + object_schema_version + object_payload describing the artifact) or by reference (cited_object_id of an already-stored object, e.g. from core_upload's complete action). The object/mapping schemas must be registered (`CitedObject`/`CitationMapping` kinds — discover them via the `proxima://schemas{?kind}` resource)."
    )]
    pub citation: Option<RememberCitation>,
    #[serde(default)]
    #[schemars(description = "Memory space key from core_memory_spaces. Omit for current owner.")]
    pub space: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Optional RFC3339 timestamp of when this Fact was originally observed, for importing historical material (must not be in the future). Recorded as receipt provenance (observed_at/occurred_at); recency ordering and the note-head pointer still follow ingestion time. Omit for 'now'."
    )]
    pub observed_at: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Optional lexical language of the content: a PostgreSQL text-search configuration name (e.g. 'german'), an ISO 639 / BCP-47 code (e.g. 'de', 'de-DE'), or 'auto' to detect it from title+body (an unreliable detection falls back to the database default). Affects lexical search tokenisation only; embeddings are language-agnostic. Omit for the database default."
    )]
    pub language: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RememberCitation {
    #[serde(default)]
    #[schemars(
        description = "Id of an ALREADY-STORED cited object to cite by reference (e.g. from core_upload's complete action or a citation read-back), optionally prefixed `C:`. Mutually exclusive with the three object_* fields; the mapping fields stay required. The object must belong to the Fact's owner and carry the schema the mapping targets."
    )]
    pub cited_object_id: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Schema id of the cited external object (a registered `CitedObject` schema — discover via `proxima://schemas{?kind}`). For an inline citation, required together with object_schema_version and object_payload; omit all three when citing by cited_object_id."
    )]
    pub object_schema_id: Option<String>,
    #[serde(default)]
    #[schemars(description = "Version of the cited-object schema (inline citation only).")]
    pub object_schema_version: Option<u32>,
    #[serde(default)]
    #[schemars(
        description = "The cited object payload as JSON, conforming to its schema (inline citation only)."
    )]
    pub object_payload: Option<serde_json::Value>,
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

#[derive(Debug, Serialize, JsonSchema)]
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
            // 240 matches the goal-title cap: same-named field, same bound
            // on every authoring surface.
            let title = validate_trimmed_len("title", &args.title, 240)?;
            let body = validate_trimmed_len("body", &args.body, 20_000)?;
            let idempotency_key = normalize_idempotency_key(args.idempotency_key)?;
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
            let note_id = idempotency_key
                .as_deref()
                .map_or_else(uuid::Uuid::now_v7, |key| {
                    uuid::Uuid::new_v5(&NOTE_NAMESPACE, key.as_bytes())
                });
            let payload = AgentNoteV1 {
                note_id,
                title: title.to_string(),
                body: body.to_string(),
                tags,
                idempotency_key,
            };
            let observed_at = super::util::parse_observed_at(args.observed_at.as_deref())?
                .unwrap_or_else(time::OffsetDateTime::now_utc);
            let lexical_language = crate::lexical_language::resolve_lexical_language(
                args.language.as_deref(),
                &format!("{title}\n{body}"),
            )
            .map_err(|err| McpToolError::InvalidInput(err.to_string()))?;
            let draft = FactWriteCommand::from_payload(SOURCE_ID, &payload, observed_at)
                .with_lexical_language(Some(lexical_language));

            let engine = ctx.require_engine()?;
            let embedding_client = engine.embed_client();
            let embedding_model_id = embedding_client.as_ref().map(|client| client.model_id());
            let outcome = if let Some(citation) = args.citation {
                ingest_cited_fact(
                    engine,
                    &authz,
                    draft,
                    remember_citation_drafts(citation)?,
                    std::slice::from_ref(&SidecarPayload::fact(payload.clone())),
                    embedding_model_id,
                )
                .await?
            } else {
                let sidecars = [SidecarPayload::fact(payload.clone())];
                let authorized = engine
                    .authorize_fact_ingest(&authz, Relation::Editor, draft, &sidecars)
                    .await?;
                engine
                    .ingest_fact_with_typed_sidecar(&authorized, &sidecars, embedding_model_id)
                    .await?
            };

            Ok(RememberOutput {
                handle: ctx.format_fact_memory(outcome.memory_id),
                idempotent_replay: outcome.idempotent_replay,
            })
        })
    }
}

/// Authorize + ingest one cited Fact through the shape-appropriate
/// engine pair: inline payload or by-ref to an already-stored object.
async fn ingest_cited_fact(
    engine: &crate::Engine,
    authz: &crate::AuthzContext,
    draft: FactWriteCommand,
    drafts: RememberCitationDrafts,
    sidecars: &[SidecarPayload],
    embedding_model_id: Option<&str>,
) -> Result<crate::FactIngestOutcome, McpToolError> {
    match drafts {
        RememberCitationDrafts::Inline {
            cited_object,
            mapping,
        } => {
            let authorized = engine
                .authorize_fact_with_citation(
                    authz,
                    Relation::Editor,
                    draft,
                    cited_object,
                    mapping,
                    sidecars,
                )
                .await?;
            Ok(engine
                .ingest_fact_with_citation_and_typed_sidecar(
                    &authorized,
                    sidecars,
                    embedding_model_id,
                )
                .await?)
        }
        RememberCitationDrafts::ByRef {
            cited_object_id,
            mapping,
        } => {
            let authorized = engine
                .authorize_fact_with_citation_by_ref(
                    authz,
                    Relation::Editor,
                    draft,
                    cited_object_id,
                    mapping,
                    sidecars,
                )
                .await?;
            Ok(engine
                .ingest_fact_with_citation_ref_and_typed_sidecar(
                    &authorized,
                    sidecars,
                    embedding_model_id,
                )
                .await?)
        }
    }
}

/// Validated citation input: inline artifact payload, or a reference to
/// an already-stored cited object.
#[derive(Debug)]
enum RememberCitationDrafts {
    Inline {
        cited_object: InlineCitedObjectDraft,
        mapping: InlineCitationMappingDraft,
    },
    ByRef {
        cited_object_id: uuid::Uuid,
        mapping: InlineCitationMappingDraft,
    },
}

/// Enforce the citation arity: exactly one of `cited_object_id` XOR the
/// full inline triple (`object_schema_id` + `object_schema_version` +
/// `object_payload`); the mapping fields are always required (by the
/// struct) and shared by both shapes.
fn remember_citation_drafts(
    citation: RememberCitation,
) -> Result<RememberCitationDrafts, McpToolError> {
    let mapping = InlineCitationMappingDraft {
        schema_id: SchemaId::new(citation.mapping_schema_id),
        schema_version: SchemaVersion::new(citation.mapping_schema_version),
        payload_bytes: encode_json_payload(&citation.mapping_payload),
    };
    let has_inline_field = citation.object_schema_id.is_some()
        || citation.object_schema_version.is_some()
        || citation.object_payload.is_some();
    match (citation.cited_object_id, has_inline_field) {
        (Some(reference), false) => {
            let cited_object_id =
                super::super::facts_citing_object::parse_cited_object_id(&reference)?;
            Ok(RememberCitationDrafts::ByRef {
                cited_object_id,
                mapping,
            })
        }
        (None, true) => {
            let (Some(schema_id), Some(schema_version), Some(payload)) = (
                citation.object_schema_id,
                citation.object_schema_version,
                citation.object_payload,
            ) else {
                return Err(McpToolError::InvalidInput(
                    "an inline citation requires object_schema_id, object_schema_version, \
                     and object_payload together"
                        .into(),
                ));
            };
            Ok(RememberCitationDrafts::Inline {
                cited_object: InlineCitedObjectDraft {
                    schema_id: SchemaId::new(schema_id),
                    schema_version: SchemaVersion::new(schema_version),
                    payload_bytes: encode_json_payload(&payload),
                },
                mapping,
            })
        }
        (Some(_), true) => Err(McpToolError::InvalidInput(
            "citation accepts either cited_object_id or the inline object_* fields, not both"
                .into(),
        )),
        (None, false) => Err(McpToolError::InvalidInput(
            "citation requires cited_object_id or the inline object_* fields \
             (object_schema_id, object_schema_version, object_payload)"
                .into(),
        )),
    }
}

fn encode_json_payload(value: &serde_json::Value) -> Vec<u8> {
    canonical_json_bytes(value)
}

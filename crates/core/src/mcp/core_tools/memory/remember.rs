use crate::mcp::{McpTool, McpToolCtx, McpToolError};
use crate::mcp::{Replay, ToolEffect};
use crate::protocol::tool as protocol_tool;
use crate::tool::validate_trimmed_len;
use crate::verbs::fact_ingest::{FactWriteCommand, InlineCitedObjectDraft};
use crate::{SchemaId, SchemaVersion, canonical_json_bytes};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{AgentNoteV1, FactPayload, SidecarPayload};

use super::util::{normalize_idempotency_key, normalize_tags};

pub(crate) const SOURCE_ID: &str = "core/agent";
pub(crate) const NOTE_NAMESPACE: uuid::Uuid = uuid::Uuid::from_bytes([
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
        description = "Search tags, 1 to 48 chars each; stored and matched lowercase."
    )]
    pub tags: Vec<String>,
    #[schemars(
        description = "Optional key, 1 to 180 chars, naming this one request: same key and content replays; changed content is `idempotency_conflict`. Use `supersedes` for a new version."
    )]
    pub idempotency_key: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Optional memory handle of a live agent-note Fact to revise. Requires a new `idempotency_key`. The new version reuses that note's identity and replaces it in default search."
    )]
    pub supersedes: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Optional typed citation linking this Fact to an external artifact. Either inline (object_schema_id + object_schema_version + object_payload describing the artifact) or by reference (cited_object_id of an already-stored object, e.g. from core_upload's complete action). The object schema must be registered (`CitedObject` kind — discover it via the `proxima://schemas{?kind}` resource)."
    )]
    pub citation: Option<RememberCitation>,
    #[serde(default)]
    #[schemars(description = "Memory space key from core_memory_spaces. Omit for current owner.")]
    pub space: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "RFC3339 time this was originally observed, for imports. Stored as the Fact's occurred_at and returned on every read; at most 5 minutes in the future. Ordering and versioning still use write time. Omit for now."
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
        description = "Id of an ALREADY-STORED cited object to cite by reference (e.g. from core_upload's complete action or a citation read-back), as a bare UUID. Mutually exclusive with the three object_* fields. The object must belong to the Fact's owner."
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
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RememberOutput {
    pub handle: String,
    pub idempotent_replay: bool,
    /// Present (true) only on a replay of a memory that has been forgotten:
    /// the write was not repeated and the memory stays forgotten.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub forgotten: bool,
}

#[derive(Debug)]
pub struct RememberTool;

impl McpTool for RememberTool {
    const NAME: &'static str = protocol_tool::CORE_REMEMBER;
    const TITLE: &'static str = "Remember a fact";
    const DESCRIPTION: &'static str =
        "Save an observation as a Fact. For a raw conversation turn use core_record_utterance.";
    const EFFECT: Option<ToolEffect> = Some(ToolEffect::Additive(Replay::NonIdempotent));
    type Args = RememberArgs;
    type Output = RememberOutput;

    fn call(
        ctx: McpToolCtx,
        args: RememberArgs,
    ) -> futures::future::BoxFuture<'static, Result<RememberOutput, McpToolError>> {
        Box::pin(async move {
            let prepared = prepare_note_write(
                &ctx,
                NoteWriteInput {
                    title: &args.title,
                    body: &args.body,
                    tags: args.tags,
                    idempotency_key: args.idempotency_key,
                    supersedes: args.supersedes.as_deref(),
                    citation: args.citation,
                    space: args.space.as_deref(),
                    observed_at: args.observed_at.as_deref(),
                    language: args.language.as_deref(),
                },
            )
            .await?;
            let outcome = execute_note_write(&ctx, prepared).await?;
            Ok(RememberOutput {
                handle: outcome.memory_id.into_inner().to_string(),
                idempotent_replay: outcome.idempotent_replay,
                forgotten: outcome.forgotten,
            })
        })
    }
}

/// Inputs shared by `core_remember` and episode remember items.
pub(crate) struct NoteWriteInput<'a> {
    pub title: &'a str,
    pub body: &'a str,
    pub tags: Vec<String>,
    pub idempotency_key: Option<String>,
    pub supersedes: Option<&'a str>,
    pub citation: Option<RememberCitation>,
    pub space: Option<&'a str>,
    pub observed_at: Option<&'a str>,
    pub language: Option<&'a str>,
}

pub(crate) struct PreparedNoteWrite {
    pub authz: crate::AuthzContext,
    pub draft: FactWriteCommand,
    pub payload: AgentNoteV1,
    pub citation: Option<RememberCitation>,
}

/// Validate and assemble one agent-note write for remember or episode.
pub(crate) async fn prepare_note_write(
    ctx: &McpToolCtx,
    input: NoteWriteInput<'_>,
) -> Result<PreparedNoteWrite, McpToolError> {
    let title = validate_trimmed_len("title", input.title, 240)?;
    let body = validate_trimmed_len("body", input.body, 20_000)?;
    let idempotency_key = normalize_idempotency_key(input.idempotency_key)?;
    let (space, authz) = super::super::memory_spaces::resolve_space_for_write(ctx, input.space)?;
    let tags = normalize_tags(input.tags)?;

    let (note_id, series_handle, supersedes_id) = resolve_note_identity(
        ctx,
        &authz,
        space.owner,
        idempotency_key.as_deref(),
        input.supersedes,
    )
    .await?;

    let payload = AgentNoteV1 {
        note_id,
        title: title.to_string(),
        body: body.to_string(),
        tags: tags.clone(),
        idempotency_key: idempotency_key.clone(),
    };
    let occurred_at = super::util::parse_observed_at(input.observed_at)?;
    let lexical_language = crate::lexical_language::resolve_lexical_language(
        input.language,
        &format!("{title}\n{body}"),
    )
    .map_err(|err| McpToolError::InvalidInput(err.to_string()))?;

    let mut draft = FactWriteCommand {
        occurred_at,
        ..FactWriteCommand::from_payload(SOURCE_ID, &payload)
    }
    .with_lexical_language(Some(lexical_language));
    if let Some(series) = series_handle {
        draft = draft.with_handle(Some(series));
    }

    if let Some(key_str) = idempotency_key.as_deref() {
        let key =
            crate::IdempotencyKey::new(key_str.to_string()).map_err(McpToolError::InvalidInput)?;
        let mut fp = crate::ContentFingerprint::builder(crate::KeyScope::Note)
            .text("title", title)
            .text("body", body)
            .tags(tags.iter().map(String::as_str));
        fp = match &input.citation {
            None => fp.absent_bytes("citation"),
            Some(citation) => fingerprint_citation(fp, citation)?,
        };
        fp = match supersedes_id {
            Some(id) => fp.uuid_set("supersedes", [id]),
            None => fp.absent_uuid_set("supersedes"),
        };
        draft = draft.request_key(crate::RequestKey::new(
            crate::KeyScope::Note,
            key,
            fp.build(),
        ));
    } else if input.supersedes.is_some() {
        return Err(McpToolError::InvalidInput(
            "supersedes requires a new idempotency_key".into(),
        ));
    }

    Ok(PreparedNoteWrite {
        authz,
        draft,
        payload,
        citation: input.citation,
    })
}

async fn resolve_note_identity(
    ctx: &McpToolCtx,
    authz: &crate::AuthzContext,
    write_owner: crate::OwnerId,
    idempotency_key: Option<&str>,
    supersedes: Option<&str>,
) -> Result<(uuid::Uuid, Option<uuid::Uuid>, Option<uuid::Uuid>), McpToolError> {
    let Some(raw) = supersedes else {
        let note_id = idempotency_key.map_or_else(uuid::Uuid::now_v7, |key| {
            uuid::Uuid::new_v5(&NOTE_NAMESPACE, key.as_bytes())
        });
        return Ok((note_id, None, None));
    };
    let prior_id = ctx.resolve_memory(raw)?;
    let engine = ctx.require_engine()?;
    let response = engine
        .get_memory(
            authz,
            &crate::GetMemoryReadRequest {
                memory_id: prior_id,
                include_neighbor_edges: false,
                at: crate::storage_ports::other_stores::ReadAt::Version,
            },
        )
        .await?;
    let snapshot = response.memory.ok_or_else(|| {
        McpToolError::InvalidInput(format!(
            "supersedes must name a live core/agent-note-v1 Fact; {raw} was not found"
        ))
    })?;
    if snapshot.kind != crate::EntityKind::Fact
        || snapshot.schema_id.as_str() != AgentNoteV1::SCHEMA_ID
    {
        return Err(McpToolError::InvalidInput(
            "supersedes must name a live core/agent-note-v1 Fact".into(),
        ));
    }
    if snapshot.owner != write_owner {
        return Err(McpToolError::InvalidInput(
            "supersedes must name a Fact in the same memory space".into(),
        ));
    }
    let payload_json = super::super::get_memory::snapshot_payload_value(snapshot.payload.as_ref())?;
    let note: AgentNoteV1 = serde_json::from_value(payload_json).map_err(|err| {
        McpToolError::InvalidInput(format!("supersedes target is not an agent note: {err}"))
    })?;
    let schema_id = AgentNoteV1::schema_id();
    let series = engine
        .owned_series_handle(
            authz,
            write_owner,
            &schema_id,
            AgentNoteV1::sidecar_table().expect("agent note has a sidecar"),
            &[(
                "note_id",
                crate::verbs::query::SidecarAtom::Uuid(note.note_id),
            )],
        )
        .await?
        .ok_or_else(|| {
            McpToolError::InvalidInput("supersedes must name a live core/agent-note-v1 Fact".into())
        })?;
    Ok((note.note_id, Some(series), Some(prior_id.into_inner())))
}

fn fingerprint_citation(
    fp: crate::verbs::idempotency::ContentFingerprintBuilder,
    citation: &RememberCitation,
) -> Result<crate::verbs::idempotency::ContentFingerprintBuilder, McpToolError> {
    if let Some(ref id) = citation.cited_object_id {
        let uuid = super::super::facts_citing_object::parse_cited_object_id(id)?;
        return Ok(fp.bytes("citation", uuid.as_bytes()));
    }
    let schema_id = citation.object_schema_id.as_deref().unwrap_or("");
    let version = citation.object_schema_version.unwrap_or(0);
    let payload = citation
        .object_payload
        .clone()
        .unwrap_or(serde_json::Value::Null);
    Ok(fp.schema_payload("citation", schema_id, version, &payload))
}

pub(crate) async fn execute_note_write(
    ctx: &McpToolCtx,
    prepared: PreparedNoteWrite,
) -> Result<crate::FactIngestOutcome, McpToolError> {
    let engine = ctx.require_engine()?;
    let PreparedNoteWrite {
        authz,
        draft,
        payload,
        citation,
    } = prepared;
    if let Some(citation) = citation {
        ingest_cited_fact(
            engine,
            &authz,
            draft,
            remember_citation_drafts(citation)?,
            std::slice::from_ref(&SidecarPayload::fact(payload)),
        )
        .await
    } else {
        let sidecars = [SidecarPayload::fact(payload)];
        let authorized = engine
            .authorize_fact_ingest(&authz, draft, &sidecars)
            .await?;
        Ok(engine.ingest_fact_with_typed_sidecar(&authorized).await?)
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
) -> Result<crate::FactIngestOutcome, McpToolError> {
    match drafts {
        RememberCitationDrafts::Inline { cited_object } => {
            let authorized = engine
                .authorize_fact_with_citation(authz, draft, cited_object, sidecars)
                .await?;
            Ok(engine
                .ingest_fact_with_citation_and_typed_sidecar(&authorized)
                .await?)
        }
        RememberCitationDrafts::ByRef { cited_object_id } => {
            let authorized = engine
                .authorize_fact_with_citation_by_ref(authz, draft, cited_object_id, sidecars)
                .await?;
            Ok(engine
                .ingest_fact_with_citation_ref_and_typed_sidecar(&authorized)
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
    },
    ByRef {
        cited_object_id: uuid::Uuid,
    },
}

/// Enforce the citation arity: exactly one of `cited_object_id` XOR the
/// full inline triple (`object_schema_id` + `object_schema_version` +
/// `object_payload`).
fn remember_citation_drafts(
    citation: RememberCitation,
) -> Result<RememberCitationDrafts, McpToolError> {
    let has_inline_field = citation.object_schema_id.is_some()
        || citation.object_schema_version.is_some()
        || citation.object_payload.is_some();
    match (citation.cited_object_id, has_inline_field) {
        (Some(reference), false) => {
            let cited_object_id =
                super::super::facts_citing_object::parse_cited_object_id(&reference)?;
            Ok(RememberCitationDrafts::ByRef { cited_object_id })
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

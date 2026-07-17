//! `core/facts_citing_object` — owner-scoped citation-to-Fact read-back,
//! newest first with keyset pagination.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::MemoryId;
use crate::engine::FactsCitingObjectReadRequest;
use crate::mcp::cursor as wire_cursor;
use crate::mcp::{McpToolCtx, McpToolError};
use crate::verbs::query::FactCitationCursor;

use super::get_memory::{GetMemoryOutput, project_memory_snapshot};

const MAX_CITATION_PAGE_LIMIT: u32 = 200;
const DEFAULT_CITATION_PAGE_LIMIT: u32 = 50;

/// Opaque cursor codec: the shared `{v, fp, c}` envelope with the
/// `(created_at, memory_id)` keyset under `c`. The fingerprint binds the
/// cited object.
const CITATION_CURSOR: wire_cursor::FingerprintedCursor = wire_cursor::FingerprintedCursor {
    version: 1,
    source: "core_fact facts_citing_object response",
    rebind_hint: "repeat the cited_object_id that produced it",
};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FactsCitingObjectArgs {
    /// Cited object uuid, optionally prefixed as `C:<uuid>`.
    pub cited_object_id: String,
    /// Max citing Facts per page; clamped to 1..=200, default 50.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Opaque pagination cursor from a previous response's `next_cursor`.
    /// The `cited_object_id` must stay unchanged between pages; `limit`
    /// may vary.
    #[serde(default)]
    pub cursor: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct FactsCitingObjectOutput {
    pub cited_object_id: String,
    pub facts: Vec<GetMemoryOutput>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

/// Keyset resume point carried inside the opaque citation cursor.
#[derive(Debug, Serialize, Deserialize)]
struct CitationCursorPos {
    created_at_nanos: i128,
    memory_id: uuid::Uuid,
}

fn citation_fingerprint(cited_object_id: uuid::Uuid) -> String {
    let canon = serde_json::to_string(&cited_object_id).expect("fingerprint canon serializes");
    wire_cursor::fingerprint(&canon)
}

pub(super) async fn facts_citing_object(
    ctx: McpToolCtx,
    args: FactsCitingObjectArgs,
) -> Result<FactsCitingObjectOutput, McpToolError> {
    let cited_object_id = parse_cited_object_id(&args.cited_object_id)?;
    let limit = args
        .limit
        .unwrap_or(DEFAULT_CITATION_PAGE_LIMIT)
        .clamp(1, MAX_CITATION_PAGE_LIMIT);
    let fingerprint = citation_fingerprint(cited_object_id);
    let after = args
        .cursor
        .as_deref()
        .map(|raw| {
            let pos: CitationCursorPos = CITATION_CURSOR.decode(&fingerprint, raw)?;
            let created_at = time::OffsetDateTime::from_unix_timestamp_nanos(pos.created_at_nanos)
                .map_err(|_| wire_cursor::malformed_cursor(CITATION_CURSOR.source))?;
            Ok::<_, McpToolError>(FactCitationCursor {
                created_at,
                memory_id: MemoryId::new(pos.memory_id),
            })
        })
        .transpose()?;
    let engine = ctx
        .engine()
        .ok_or_else(|| McpToolError::Other("engine unavailable".into()))?;
    let page = engine
        .facts_citing_object(
            &ctx.authz,
            &FactsCitingObjectReadRequest {
                cited_object_id,
                limit,
                after,
            },
        )
        .await?;
    let next_cursor = page.next_cursor.map(|cursor| {
        CITATION_CURSOR.encode(
            &fingerprint,
            &CitationCursorPos {
                created_at_nanos: cursor.created_at.unix_timestamp_nanos(),
                memory_id: cursor.memory_id.into_inner(),
            },
        )
    });
    let facts = page
        .facts
        .into_iter()
        .map(|snapshot| project_memory_snapshot(&ctx, snapshot, "current".into(), None))
        .collect::<Result<Vec<_>, McpToolError>>()?;
    Ok(FactsCitingObjectOutput {
        cited_object_id: cited_object_id.to_string(),
        facts,
        next_cursor,
        has_more: page.has_more,
    })
}

pub(super) fn parse_cited_object_id(raw: &str) -> Result<uuid::Uuid, McpToolError> {
    let uuid_part = raw.strip_prefix("C:").unwrap_or(raw);
    uuid_part
        .parse()
        .map_err(|err| McpToolError::InvalidInput(format!("not a cited_object_id uuid: {err}")))
}

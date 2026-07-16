//! Edge reads backing `proxima://edges` and `proxima://edge/{id}`:
//! relation/endpoint-filtered listing with keyset pagination and a
//! single-edge dereference, both with typed sidecar payload read-back.
//! Until this surface, edges were write-only on the wire — `core_link`
//! returned an `E:<uuid>` handle nothing could dereference.

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use time::format_description::well_known::Rfc3339;

use crate::mcp::{McpToolCtx, McpToolError};
use crate::verbs::query::{
    EdgeFilter, EdgeReadCursor, EdgeReadRequest, EdgeRow, EdgeTargetProjection, EntityKind,
};
use crate::{EdgeId, EntityRef, MemoryId};

use super::get_memory::snapshot_payload_value;

const MAX_EDGE_PAGE_LIMIT: u32 = 200;
const DEFAULT_EDGE_PAGE_LIMIT: u32 = 50;
const EDGE_CURSOR_VERSION: u8 = 1;

#[derive(Debug, Deserialize)]
pub struct ListEdgesArgs {
    /// Relation filter (e.g. `core/agent-link-refers-to`); must be a
    /// registered edge type (see `proxima://edge-types`).
    pub relation: Option<String>,
    /// Source endpoint filter: `F:`/`A:`/`P:`/`G:` prefixed id.
    pub source: Option<String>,
    /// Target endpoint filter: `F:`/`A:`/`P:`/`G:` prefixed id.
    pub target: Option<String>,
    /// Max edges per page; clamped to 1..=200, default 50.
    pub limit: Option<u32>,
    /// Opaque pagination cursor from a previous response's `next_cursor`.
    pub cursor: Option<String>,
    /// Include typed edge payloads (e.g. agent-link reason/confidence).
    /// Default true; pass `payloads=false` for lean results.
    pub payloads: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct ListEdgesOutput {
    pub edges: Vec<EdgeItem>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

#[derive(Debug, Serialize)]
pub struct EdgeItem {
    /// Edge reference (`E:<uuid>`), as returned by `core_link`.
    pub edge: String,
    pub relation: String,
    pub relation_class: String,
    /// Source endpoint handle (`F:`/`A:`/`P:`/`G:` prefixed id).
    pub source: String,
    /// Target endpoint handle, or `redacted target`/`unavailable target`
    /// when the caller may not see it.
    pub target: String,
    pub created_at: String,
    /// Typed sidecar payload (e.g. agent-link `reason`/`confidence`);
    /// `null` when the relation has no payload schema or payloads were
    /// not requested.
    pub payload: serde_json::Value,
}

/// Opaque wire cursor: version + filter binding + edge keyset. The filter
/// tags bind the cursor to the query that produced it so a replay under a
/// different filter fails closed instead of returning a wrong page.
#[derive(Debug, Serialize, Deserialize)]
struct WireEdgeCursor {
    v: u8,
    relation: Option<String>,
    source: Option<String>,
    target: Option<String>,
    created_at_nanos: i128,
    edge_id: uuid::Uuid,
}

/// # Errors
///
/// Returns invalid relation/endpoint/cursor arguments, authorization, or
/// storage failures.
pub async fn list_edges(
    ctx: McpToolCtx,
    args: ListEdgesArgs,
) -> Result<ListEdgesOutput, McpToolError> {
    if args.relation.is_none() && args.source.is_none() && args.target.is_none() {
        return Err(McpToolError::InvalidInput(
            "provide at least one filter: relation, source, or target \
             (proxima://edges does not dump the whole graph)"
                .into(),
        ));
    }
    if let Some(relation) = args.relation.as_deref()
        && ctx.registry.resolve_relation(relation).is_none()
    {
        return Err(McpToolError::InvalidInput(format!(
            "unknown relation '{relation}'; list registered edge types via proxima://edge-types"
        )));
    }
    let source = args
        .source
        .as_deref()
        .map(|raw| resolve_endpoint(&ctx, "source", raw))
        .transpose()?;
    let target = args
        .target
        .as_deref()
        .map(|raw| resolve_endpoint(&ctx, "target", raw))
        .transpose()?;
    let cursor = args
        .cursor
        .as_deref()
        .map(|raw| decode_edge_cursor(raw, &args))
        .transpose()?;
    let limit = args
        .limit
        .unwrap_or(DEFAULT_EDGE_PAGE_LIMIT)
        .clamp(1, MAX_EDGE_PAGE_LIMIT);

    let engine = ctx
        .engine()
        .ok_or_else(|| McpToolError::Other("engine unavailable".into()))?;
    let response = engine
        .read_edges(
            &ctx.authz,
            &EdgeReadRequest {
                owner: ctx.owner,
                edge_ids: Vec::new(),
                filter: EdgeFilter {
                    relation: args.relation.clone(),
                    source,
                    target,
                },
                limit,
                cursor,
                include_payloads: args.payloads.unwrap_or(true),
            },
        )
        .await?;

    let edges = response
        .edges
        .into_iter()
        .map(|row| edge_item(&ctx, row))
        .collect::<Result<Vec<_>, _>>()?;
    let next_cursor = response
        .next_cursor
        .map(|cursor| encode_edge_cursor(cursor, &args));
    let has_more = next_cursor.is_some();
    Ok(ListEdgesOutput {
        edges,
        next_cursor,
        has_more,
    })
}

/// # Errors
///
/// Returns malformed/unknown edge references, authorization, or storage
/// failures.
pub async fn get_edge(ctx: McpToolCtx, raw: &str) -> Result<EdgeItem, McpToolError> {
    let edge_id = ctx.resolve_edge(raw)?;
    let engine = ctx
        .engine()
        .ok_or_else(|| McpToolError::Other("engine unavailable".into()))?;
    let response = engine
        .read_edges(
            &ctx.authz,
            &EdgeReadRequest {
                owner: ctx.owner,
                edge_ids: vec![edge_id],
                filter: EdgeFilter::default(),
                limit: 1,
                cursor: None,
                include_payloads: true,
            },
        )
        .await?;
    let row = response
        .edges
        .into_iter()
        .next()
        .ok_or_else(|| McpToolError::InvalidInput(format!("edge not found: {raw}")))?;
    edge_item(&ctx, row)
}

fn edge_item(ctx: &McpToolCtx, row: EdgeRow) -> Result<EdgeItem, McpToolError> {
    let source = format_entity_ref(ctx, &row.source, Some(row.source_kind));
    let target = match &row.target {
        EdgeTargetProjection::Visible { target } => format_entity_ref(ctx, target, row.target_kind),
        EdgeTargetProjection::Redacted => "redacted target".into(),
        EdgeTargetProjection::Unavailable => "unavailable target".into(),
    };
    let created_at = row
        .created_at
        .format(&Rfc3339)
        .map_err(|err| McpToolError::Other(format!("format edge created_at: {err}")))?;
    Ok(EdgeItem {
        edge: ctx.format_edge(EdgeId::new(row.id)),
        relation: row.relation,
        relation_class: row.relation_class,
        source,
        target,
        created_at,
        payload: snapshot_payload_value(row.payload.as_ref())?,
    })
}

fn format_entity_ref(ctx: &McpToolCtx, entity: &EntityRef, kind: Option<EntityKind>) -> String {
    match entity {
        EntityRef::Memory(memory_id) => format_memory_endpoint(ctx, *memory_id, kind),
        EntityRef::Goal(goal_id) => ctx.format_goal(*goal_id),
        EntityRef::FactEntity(fact_entity_id) => {
            format!("fact_entity:{}", fact_entity_id.into_inner())
        }
    }
}

fn format_memory_endpoint(
    ctx: &McpToolCtx,
    memory_id: MemoryId,
    kind: Option<EntityKind>,
) -> String {
    match kind {
        Some(EntityKind::Abstraction) => ctx.format_abstraction_memory(memory_id),
        Some(EntityKind::Perspective) => ctx.format_perspective_memory(memory_id),
        Some(EntityKind::Fact | EntityKind::Goal) | None => ctx.format_fact_memory(memory_id),
    }
}

fn resolve_endpoint(ctx: &McpToolCtx, field: &str, raw: &str) -> Result<EntityRef, McpToolError> {
    if let Ok(goal_id) = ctx.resolve_goal(raw) {
        return Ok(EntityRef::Goal(goal_id));
    }
    ctx.resolve_memory(raw).map(EntityRef::Memory).map_err(|_| {
        McpToolError::InvalidInput(format!(
            "{field} must be an `F:`/`A:`/`P:`/`G:` prefixed id; got '{raw}'"
        ))
    })
}

fn encode_edge_cursor(cursor: EdgeReadCursor, args: &ListEdgesArgs) -> String {
    let json = serde_json::to_vec(&WireEdgeCursor {
        v: EDGE_CURSOR_VERSION,
        relation: args.relation.clone(),
        source: args.source.clone(),
        target: args.target.clone(),
        created_at_nanos: cursor.created_at.unix_timestamp_nanos(),
        edge_id: cursor.edge_id.into_inner(),
    })
    .expect("edge cursor serializes");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json)
}

fn decode_edge_cursor(raw: &str, args: &ListEdgesArgs) -> Result<EdgeReadCursor, McpToolError> {
    let malformed = || {
        McpToolError::InvalidInput(
            "malformed cursor: pass next_cursor from a previous proxima://edges page".into(),
        )
    };
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(raw.as_bytes())
        .map_err(|_| malformed())?;
    let wire: WireEdgeCursor = serde_json::from_slice(&bytes).map_err(|_| malformed())?;
    if wire.v != EDGE_CURSOR_VERSION {
        return Err(malformed());
    }
    if wire.relation != args.relation || wire.source != args.source || wire.target != args.target {
        return Err(McpToolError::InvalidInput(
            "cursor does not match this query: repeat the relation/source/target filter that \
             produced it"
                .into(),
        ));
    }
    let created_at = time::OffsetDateTime::from_unix_timestamp_nanos(wire.created_at_nanos)
        .map_err(|_| malformed())?;
    Ok(EdgeReadCursor {
        created_at,
        edge_id: EdgeId::new(wire.edge_id),
    })
}

#[cfg(test)]
mod tests {
    use super::{ListEdgesArgs, decode_edge_cursor, encode_edge_cursor};
    use crate::McpToolError;
    use crate::verbs::query::EdgeReadCursor;

    fn args(relation: Option<&str>, source: Option<&str>) -> ListEdgesArgs {
        ListEdgesArgs {
            relation: relation.map(str::to_string),
            source: source.map(str::to_string),
            target: None,
            limit: None,
            cursor: None,
            payloads: None,
        }
    }

    #[test]
    fn edge_cursor_round_trips_and_binds_to_filter() {
        let edge_id = uuid::Uuid::now_v7();
        let created_at = time::OffsetDateTime::UNIX_EPOCH + time::Duration::days(20_000);
        let query = args(Some("core/agent-link-refers-to"), Some("A:abc"));
        let token = encode_edge_cursor(
            EdgeReadCursor {
                created_at,
                edge_id: crate::EdgeId::new(edge_id),
            },
            &query,
        );
        let decoded = decode_edge_cursor(&token, &query).unwrap();
        assert_eq!(decoded.created_at, created_at);
        assert_eq!(decoded.edge_id.into_inner(), edge_id);

        // Replay under a different (or missing) filter fails closed.
        assert!(matches!(
            decode_edge_cursor(&token, &args(Some("core/derived-from"), Some("A:abc"))),
            Err(McpToolError::InvalidInput(message)) if message.contains("does not match")
        ));
        assert!(decode_edge_cursor(&token, &args(None, None)).is_err());
        assert!(matches!(
            decode_edge_cursor("garbage!!", &query),
            Err(McpToolError::InvalidInput(message)) if message.contains("malformed cursor")
        ));
    }
}

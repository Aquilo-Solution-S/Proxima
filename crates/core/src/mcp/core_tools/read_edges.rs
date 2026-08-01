//! Edge reads backing `proxima://edges`: kind/endpoint-filtered listing
//! with keyset pagination.
//!
//! An edge is four fields — source, target, kind, `created_at` — and that
//! is the whole of what this surface can return. There is no
//! `proxima://edge/{id}` companion because an edge has no id to
//! dereference: its content is its identity, so the way to ask about one
//! edge is to filter for its endpoints.

use serde::{Deserialize, Serialize};
use time::format_description::well_known::Rfc3339;

use crate::mcp::cursor as wire_cursor;
use crate::mcp::{McpToolCtx, McpToolError};
use crate::verbs::query::{EdgeFilter, EdgeReadCursor, EdgeReadRequest};
use crate::{Edge, EdgeKind, EntityRef};

/// Opaque cursor codec: the shared `{v, fp, c}` envelope with the edge
/// keyset under `c`. The fingerprint binds the kind/source/target filter;
/// `limit` stays out so it may vary between pages.
const EDGE_CURSOR: wire_cursor::FingerprintedCursor = wire_cursor::FingerprintedCursor {
    version: 1,
    source: "proxima://edges page",
    rebind_hint: "repeat the kind/source/target filter that produced it",
};

#[derive(Debug, Deserialize)]
pub struct ListEdgesArgs {
    /// Edge kind filter: `origin` (what a memory was made from) or
    /// `reference` (what a payload points at). The vocabulary is closed
    /// at these two.
    pub kind: Option<String>,
    /// Source endpoint filter: `F:`/`A:`/`P:`/`G:` prefixed id.
    pub source: Option<String>,
    /// Target endpoint filter: `F:`/`A:`/`P:`/`G:` prefixed id.
    pub target: Option<String>,
    /// Max edges per page; values above 200 are clamped, 0 is rejected,
    /// default 50.
    pub limit: Option<u32>,
    /// Opaque pagination cursor from a previous response's `next_cursor`.
    pub cursor: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ListEdgesOutput {
    pub edges: Vec<EdgeItem>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

/// One edge on the wire. Deliberately four fields: there is no handle,
/// no relation, and no payload, because an edge carries no information
/// beyond its existence.
#[derive(Debug, Serialize)]
pub struct EdgeItem {
    /// Source endpoint handle (`F:`/`A:`/`P:`/`G:` prefixed id).
    pub source: String,
    /// Target endpoint handle, or `redacted target`/`unavailable target`
    /// when the caller may not see it.
    pub target: String,
    /// `origin` or `reference`.
    pub kind: String,
    pub created_at: String,
}

/// Keyset resume point carried inside the opaque edge cursor. The whole
/// primary key travels, because that is what makes the order total once
/// there is no id to break ties with.
#[derive(Debug, Serialize, Deserialize)]
struct EdgeCursorPos {
    created_at_nanos: i128,
    source: EntityRef,
    target: EntityRef,
    kind: EdgeKind,
}

fn edge_fingerprint(args: &ListEdgesArgs) -> String {
    let canon = serde_json::to_string(&(&args.kind, &args.source, &args.target))
        .expect("fingerprint canon serializes");
    wire_cursor::fingerprint(&canon)
}

fn parse_kind(raw: &str) -> Result<EdgeKind, McpToolError> {
    match raw {
        "origin" => Ok(EdgeKind::Origin),
        "reference" => Ok(EdgeKind::Reference),
        other => Err(McpToolError::InvalidInput(format!(
            "unknown edge kind '{other}'; the vocabulary is closed at 'origin' and 'reference'"
        ))),
    }
}

/// # Errors
///
/// Returns invalid kind/endpoint/cursor arguments, authorization, or
/// storage failures.
pub async fn list_edges(
    ctx: McpToolCtx,
    args: ListEdgesArgs,
) -> Result<ListEdgesOutput, McpToolError> {
    if args.kind.is_none() && args.source.is_none() && args.target.is_none() {
        return Err(McpToolError::InvalidInput(
            "provide at least one filter: kind, source, or target \
             (proxima://edges does not dump the whole graph)"
                .into(),
        ));
    }
    let kind = args.kind.as_deref().map(parse_kind).transpose()?;
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
    let limit = super::resolve_page_limit(args.limit)?;

    let engine = ctx.require_engine()?;
    let response = engine
        .read_edges(
            &ctx.authz,
            &EdgeReadRequest {
                owner: ctx.owner,
                filter: EdgeFilter {
                    kind,
                    source,
                    target,
                },
                limit,
                cursor,
            },
        )
        .await?;

    let edges = response
        .edges
        .into_iter()
        .map(|edge| edge_item(&ctx, edge))
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

fn edge_item(ctx: &McpToolCtx, edge: Edge) -> Result<EdgeItem, McpToolError> {
    let created_at = edge
        .created_at
        .format(&Rfc3339)
        .map_err(|err| McpToolError::Other(format!("format edge created_at: {err}")))?;
    Ok(EdgeItem {
        source: super::wire_ref::format_endpoint(ctx, edge.source),
        target: super::wire_ref::format_target_projection(ctx, edge.target),
        kind: edge.kind.as_str().to_string(),
        created_at,
    })
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
    EDGE_CURSOR.encode(
        &edge_fingerprint(args),
        &EdgeCursorPos {
            created_at_nanos: cursor.created_at.unix_timestamp_nanos(),
            source: cursor.source,
            target: cursor.target,
            kind: cursor.kind,
        },
    )
}

fn decode_edge_cursor(raw: &str, args: &ListEdgesArgs) -> Result<EdgeReadCursor, McpToolError> {
    let pos: EdgeCursorPos = EDGE_CURSOR.decode(&edge_fingerprint(args), raw)?;
    let created_at = time::OffsetDateTime::from_unix_timestamp_nanos(pos.created_at_nanos)
        .map_err(|_| wire_cursor::malformed_cursor(EDGE_CURSOR.source))?;
    Ok(EdgeReadCursor {
        created_at,
        source: pos.source,
        target: pos.target,
        kind: pos.kind,
    })
}

#[cfg(test)]
mod tests {
    use super::{ListEdgesArgs, decode_edge_cursor, encode_edge_cursor, parse_kind};
    use crate::verbs::query::EdgeReadCursor;
    use crate::{EdgeKind, EntityRef, McpToolError, MemoryId};

    fn args(kind: Option<&str>, source: Option<&str>) -> ListEdgesArgs {
        ListEdgesArgs {
            kind: kind.map(str::to_string),
            source: source.map(str::to_string),
            target: None,
            limit: None,
            cursor: None,
        }
    }

    /// The wire vocabulary is the document's vocabulary. Anything else is
    /// refused rather than silently ignored, because a filter nobody
    /// applies reads as "no edges of that kind exist".
    #[test]
    fn only_the_two_kinds_parse() {
        assert_eq!(parse_kind("origin").unwrap(), EdgeKind::Origin);
        assert_eq!(parse_kind("reference").unwrap(), EdgeKind::Reference);
        for unknown in ["provenance", "structural", "supersedes", "Origin", ""] {
            assert!(
                parse_kind(unknown).is_err(),
                "{unknown} must not parse as an edge kind"
            );
        }
    }

    #[test]
    fn edge_cursor_round_trips_and_binds_to_filter() {
        let created_at = time::OffsetDateTime::UNIX_EPOCH + time::Duration::days(20_000);
        let position = EdgeReadCursor {
            created_at,
            source: EntityRef::Memory(MemoryId::new(uuid::Uuid::now_v7())),
            target: EntityRef::Memory(MemoryId::new(uuid::Uuid::now_v7())),
            kind: EdgeKind::Origin,
        };
        let query = args(Some("origin"), Some("A:abc"));
        let token = encode_edge_cursor(position, &query);
        let decoded = decode_edge_cursor(&token, &query).unwrap();
        assert_eq!(decoded, position);

        // Replay under a different (or missing) filter fails closed.
        assert!(matches!(
            decode_edge_cursor(&token, &args(Some("reference"), Some("A:abc"))),
            Err(McpToolError::InvalidInput(message)) if message.contains("does not match")
        ));
        assert!(decode_edge_cursor(&token, &args(None, None)).is_err());
        assert!(matches!(
            decode_edge_cursor("garbage!!", &query),
            Err(McpToolError::InvalidInput(message)) if message.contains("malformed cursor")
        ));
    }
}

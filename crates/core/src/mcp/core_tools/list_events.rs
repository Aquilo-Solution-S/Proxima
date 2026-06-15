//! `core/list_events` - forward pull-log poll over `change_event`.
//!
//! This is the forward, opaque-seq-cursor mirror of backward
//! `EventHistory`: clients pass the prior `next_since` as `since` and
//! receive owner-scoped events in ascending `seq` order. Edge endpoint
//! memory subkind is recovered from `proxima_core.edges` in one batched
//! lookup; if the edge row is absent, memory endpoints fall back to the
//! Fact prefix. The id still resolves regardless of the display prefix.

use std::collections::HashMap;

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::change_event::{ChangeEventKind, EntityRef};
use crate::mcp::{McpToolCtx, McpToolError};
use crate::personality::ChangeEventForWake;
use crate::{EdgeId, EntityKind, McpTool};

#[derive(Debug, Default)]
pub struct ListEventsTool;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListEventsArgs {
    /// Opaque cursor from a prior `next_since`. Omit to read from the beginning.
    pub since: Option<String>,
    /// Max events; clamped to 1..=1000, default 100.
    pub limit: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct ListEventsOutput {
    pub events: Vec<EventItem>,
    pub next_since: Option<String>,
    pub has_more: bool,
}

#[derive(Debug, Serialize)]
pub struct EventItem {
    pub seq: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authoring_personality: Option<String>,
    pub wake_chain_depth: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edge: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

impl McpTool for ListEventsTool {
    const NAME: &'static str = "core/list_events";
    const DESCRIPTION: &'static str = "Forward, owner-scoped poll of the change-event pull log. Returns events with seq > `since`, ascending, so a harness wake loop can advance a durable cursor. Pass the prior `next_since` back as `since`; omit `since` to read from the start. `has_more` is true when more events may be waiting.";
    type Args = ListEventsArgs;
    type Output = ListEventsOutput;

    fn call(
        ctx: McpToolCtx,
        args: ListEventsArgs,
    ) -> BoxFuture<'static, Result<ListEventsOutput, McpToolError>> {
        Box::pin(async move {
            let storage = ctx
                .storage()
                .ok_or_else(|| McpToolError::Other("engine storage unavailable".into()))?;
            let after = match &args.since {
                None => uuid::Uuid::nil(),
                Some(since) => since.parse::<uuid::Uuid>().map_err(|err| {
                    McpToolError::InvalidInput(format!("since is not a valid seq cursor: {err}"))
                })?,
            };
            let limit = args.limit.unwrap_or(100).clamp(1, 1000) as usize;
            let rows = storage
                .list_change_events_after(&ctx.owner, after, limit)
                .await?;
            let edge_kinds = load_edge_endpoint_kinds(&ctx, &rows).await?;

            let events = rows
                .into_iter()
                .map(|row| event_item(&ctx, row, &edge_kinds))
                .collect::<Vec<_>>();
            let has_more = events.len() == limit;
            let next_since = events.last().map(|event| event.seq.clone()).or(args.since);

            Ok(ListEventsOutput {
                events,
                next_since,
                has_more,
            })
        })
    }
}

async fn load_edge_endpoint_kinds(
    ctx: &McpToolCtx,
    rows: &[ChangeEventForWake],
) -> Result<HashMap<uuid::Uuid, (EntityKind, EntityKind)>, McpToolError> {
    let edge_ids = rows
        .iter()
        .filter_map(|row| match &row.event.kind {
            ChangeEventKind::EdgeAppend { edge_id, .. } => Some(*edge_id),
            ChangeEventKind::EntityAppend { .. } | ChangeEventKind::EntityDelete { .. } => None,
        })
        .collect::<Vec<_>>();
    if edge_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let rows: Vec<EdgeKindRow> = sqlx::query_as(
        "SELECT edge_id, source_kind, target_kind
         FROM proxima_core.edges
         WHERE edge_id = ANY($1)",
    )
    .bind(&edge_ids)
    .fetch_all(&ctx.pool)
    .await
    .map_err(|err| map_storage(&err))?;

    Ok(rows
        .into_iter()
        .map(|row| (row.edge_id, (row.source_kind, row.target_kind)))
        .collect())
}

fn event_item(
    ctx: &McpToolCtx,
    row: ChangeEventForWake,
    edge_kinds: &HashMap<uuid::Uuid, (EntityKind, EntityKind)>,
) -> EventItem {
    let seq = row.event.seq.to_string();
    let authoring_personality = row
        .authoring_personality_instance_id
        .map(|id| ctx.format_personality(id));
    match row.event.kind {
        ChangeEventKind::EntityAppend {
            entity_kind,
            entity,
            schema_id,
            schema_version,
            supersedes,
        } => EventItem {
            seq,
            kind: "entity_append".into(),
            authoring_personality,
            wake_chain_depth: row.event.wake_chain_depth,
            entity: Some(format_ref(ctx, &entity, entity_kind)),
            entity_kind: Some(entity_kind.as_str().into()),
            schema_id: Some(schema_id.as_str().to_string()),
            schema_version: Some(schema_version.into_inner()),
            supersedes: supersedes.as_ref().map(|r| format_ref(ctx, r, entity_kind)),
            edge: None,
            relation: None,
            source: None,
            target: None,
        },
        ChangeEventKind::EntityDelete {
            entity_kind,
            entity,
            schema_id,
            schema_version,
        } => EventItem {
            seq,
            kind: "entity_delete".into(),
            authoring_personality,
            wake_chain_depth: row.event.wake_chain_depth,
            entity: Some(format_ref(ctx, &entity, entity_kind)),
            entity_kind: Some(entity_kind.as_str().into()),
            schema_id: Some(schema_id.as_str().to_string()),
            schema_version: Some(schema_version.into_inner()),
            supersedes: None,
            edge: None,
            relation: None,
            source: None,
            target: None,
        },
        ChangeEventKind::EdgeAppend {
            edge_id,
            relation,
            source,
            target,
        } => {
            let (source_kind, target_kind) = edge_kinds
                .get(&edge_id)
                .copied()
                .unwrap_or((EntityKind::Fact, EntityKind::Fact));
            EventItem {
                seq,
                kind: "edge_append".into(),
                authoring_personality,
                wake_chain_depth: row.event.wake_chain_depth,
                entity: None,
                entity_kind: None,
                schema_id: None,
                schema_version: None,
                supersedes: None,
                edge: Some(ctx.format_edge(EdgeId::new(edge_id))),
                relation: Some(relation),
                source: Some(format_ref(ctx, &source, source_kind)),
                target: Some(format_ref(ctx, &target, target_kind)),
            }
        }
    }
}

#[derive(Debug, sqlx::FromRow)]
struct EdgeKindRow {
    edge_id: uuid::Uuid,
    source_kind: EntityKind,
    target_kind: EntityKind,
}

fn format_ref(ctx: &McpToolCtx, r: &EntityRef, kind: EntityKind) -> String {
    match r {
        EntityRef::Goal(g) => ctx.format_goal(*g),
        EntityRef::Memory(m) => match kind {
            EntityKind::Abstraction => ctx.format_abstraction_memory(*m),
            EntityKind::Perspective => ctx.format_perspective_memory(*m),
            EntityKind::Fact | EntityKind::Goal => ctx.format_fact_memory(*m),
        },
    }
}

fn map_storage(error: &sqlx::Error) -> McpToolError {
    McpToolError::Storage(crate::StorageError::Internal(error.to_string()))
}

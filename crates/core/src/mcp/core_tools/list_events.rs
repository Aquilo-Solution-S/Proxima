//! `core/list_events` - forward pull-log poll over `change_event`.
//!
//! This is the forward, opaque-seq-cursor mirror of backward
//! `EventHistory`: clients pass the prior `next_since` as `since` and
//! receive owner-scoped events in ascending `seq` order. Edge endpoint
//! memory subkind is recovered from `proxima_core.edges` in one batched
//! lookup; if the edge row is absent, memory endpoints fall back to the
//! Fact prefix. The id still resolves regardless of the display prefix.

use std::collections::HashMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::change_event::{ChangeEventKind, EntityRef};
use crate::engine::ListEventsReadRequest;
use crate::mcp::{McpToolCtx, McpToolError};
use crate::personality::ChangeEventForWake;
use crate::{EdgeId, EntityKind};

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

/// # Errors
///
/// Returns invalid cursor, storage, or projection failures.
pub async fn list_events(
    ctx: McpToolCtx,
    args: ListEventsArgs,
) -> Result<ListEventsOutput, McpToolError> {
    let after = match &args.since {
        None => uuid::Uuid::nil(),
        Some(since) => since.parse::<uuid::Uuid>().map_err(|err| {
            McpToolError::InvalidInput(format!("since is not a valid seq cursor: {err}"))
        })?,
    };
    let limit = args.limit.unwrap_or(100).clamp(1, 1000) as usize;
    let engine = ctx
        .engine()
        .ok_or_else(|| McpToolError::Other("engine unavailable".into()))?;
    let response = engine
        .list_events(
            &ctx.authz,
            &ListEventsReadRequest {
                principal: ctx.owner,
                after,
                limit: overfetch_limit(limit),
            },
        )
        .await?;
    let edge_kinds = response
        .edge_endpoint_kinds
        .into_iter()
        .map(|row| (row.edge_id.into_inner(), (row.source_kind, row.target_kind)))
        .collect::<HashMap<_, _>>();

    let (rows, has_more) = page_rows(response.events, limit);
    let events = rows
        .into_iter()
        .map(|row| event_item(&ctx, row, &edge_kinds))
        .collect::<Vec<_>>();
    let next_since = events.last().map(|event| event.seq.clone()).or(args.since);

    Ok(ListEventsOutput {
        events,
        next_since,
        has_more,
    })
}

fn overfetch_limit(limit: usize) -> usize {
    limit.saturating_add(1)
}

fn page_rows(mut rows: Vec<ChangeEventForWake>, limit: usize) -> (Vec<ChangeEventForWake>, bool) {
    let has_more = rows.len() > limit;
    rows.truncate(limit);
    (rows, has_more)
}

fn event_item(
    ctx: &McpToolCtx,
    row: ChangeEventForWake,
    edge_kinds: &HashMap<uuid::Uuid, (EntityKind, Option<EntityKind>)>,
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
                .unwrap_or((EntityKind::Fact, None));
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
                target: Some(format_target_projection(ctx, target, target_kind)),
            }
        }
        ChangeEventKind::EdgeDelete {
            edge_id,
            relation,
            source,
            target,
        } => {
            let (source_kind, target_kind) = edge_kinds
                .get(&edge_id)
                .copied()
                .unwrap_or((kind_from_ref(&source), None));
            EventItem {
                seq,
                kind: "edge_delete".into(),
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
                target: Some(format_target_projection(ctx, target, target_kind)),
            }
        }
    }
}

fn kind_from_ref(r: &EntityRef) -> EntityKind {
    match r {
        EntityRef::Goal(_) => EntityKind::Goal,
        EntityRef::FactEntity(_) | EntityRef::Memory(_) => EntityKind::Fact,
    }
}

fn format_target_projection(
    ctx: &McpToolCtx,
    target: crate::EdgeTargetProjection,
    target_kind: Option<EntityKind>,
) -> String {
    match target {
        crate::EdgeTargetProjection::Visible { target } => format_ref(
            ctx,
            &target,
            target_kind.unwrap_or_else(|| kind_from_ref(&target)),
        ),
        crate::EdgeTargetProjection::Redacted => "redacted target".into(),
        crate::EdgeTargetProjection::Unavailable => "unavailable target".into(),
    }
}

fn format_ref(ctx: &McpToolCtx, r: &EntityRef, kind: EntityKind) -> String {
    match r {
        EntityRef::Goal(g) => ctx.format_goal(*g),
        EntityRef::FactEntity(fe) => format!("fact_entity:{}", fe.into_inner()),
        EntityRef::Memory(m) => match kind {
            EntityKind::Abstraction => ctx.format_abstraction_memory(*m),
            EntityKind::Perspective => ctx.format_perspective_memory(*m),
            EntityKind::Fact | EntityKind::Goal => ctx.format_fact_memory(*m),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{overfetch_limit, page_rows};
    use crate::change_event::{ChangeEvent, ChangeEventKind};
    use crate::personality::{ChangeEventForWake, WakeChainDepth};
    use crate::{EntityKind, EntityRef, MemoryId, SchemaId, SchemaVersion};

    fn row(seq: uuid::Uuid) -> ChangeEventForWake {
        ChangeEventForWake {
            event: ChangeEvent {
                seq,
                owner: crate::OwnerRef::Personal(crate::UserId::new(uuid::Uuid::now_v7())),
                kind: ChangeEventKind::EntityDelete {
                    entity_kind: EntityKind::Fact,
                    entity: EntityRef::Memory(MemoryId::new(uuid::Uuid::now_v7())),
                    schema_id: SchemaId::new("test/schema".into()),
                    schema_version: SchemaVersion::new(1),
                },
                authoring_personality_instance_id: None,
                wake_chain_depth: 0,
            },
            authoring_personality_instance_id: None,
            wake_chain_depth: WakeChainDepth::new(0),
        }
    }

    #[test]
    fn page_rows_reports_no_more_on_exact_final_page() {
        let rows = vec![row(uuid::Uuid::now_v7()), row(uuid::Uuid::now_v7())];
        let (page, has_more) = page_rows(rows, 2);
        assert_eq!(page.len(), 2);
        assert!(!has_more);
    }

    #[test]
    fn page_rows_reports_more_only_from_extra_row() {
        let rows = vec![
            row(uuid::Uuid::now_v7()),
            row(uuid::Uuid::now_v7()),
            row(uuid::Uuid::now_v7()),
        ];
        let (page, has_more) = page_rows(rows, 2);
        assert_eq!(page.len(), 2);
        assert!(has_more);
    }

    #[test]
    fn overfetch_limit_adds_one() {
        assert_eq!(overfetch_limit(1000), 1001);
    }
}

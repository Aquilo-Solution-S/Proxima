//! Goal introspection reads backing `proxima://goals` and
//! `proxima://goal/{id}`: list-by-state with keyset pagination and a
//! single-goal read, both with wake-config read-back. Goals stay
//! write-only through `core_goal`; these are pull verbs for the same
//! external harnesses that drive the wake loop.

use serde::{Deserialize, Serialize};

use crate::GoalId;
use crate::mcp::cursor as wire_cursor;
use crate::mcp::handles::{PrefixedUuidClass, format_prefixed_uuid, parse_prefixed_uuid};
use crate::mcp::{McpToolCtx, McpToolError};
use crate::read_models::GoalWakeConfigRow;
use crate::verbs::goal_write::GoalState;
use crate::verbs::query::{
    EntityKind, QueryCursor, QueryPage, QueryRequest, SupersessionStatus, TombstoneFilter,
};

/// Opaque cursor codec: the shared `{v, fp, c}` envelope with the goal
/// keyset under `c`. The fingerprint binds the state filter; `limit`
/// stays out so it may vary between pages.
const GOAL_CURSOR: wire_cursor::FingerprintedCursor = wire_cursor::FingerprintedCursor {
    version: 1,
    source: "proxima://goals page",
    rebind_hint: "repeat the state filter that produced it",
};

#[derive(Debug, Deserialize)]
pub struct ListGoalsArgs {
    /// Goal state filter: Active, Paused, Achieved, or Abandoned
    /// (case-insensitive). Omit for all states.
    pub state: Option<String>,
    /// Max goals per page; values above 200 are clamped, 0 is rejected,
    /// default 50.
    pub limit: Option<u32>,
    /// Opaque pagination cursor from a previous response's `next_cursor`.
    pub cursor: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ListGoalsOutput {
    pub goals: Vec<GoalItem>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

#[derive(Debug, Serialize)]
pub struct GoalItem {
    /// Goal reference (`G:<uuid>`), as accepted by `core_goal`.
    pub goal: String,
    pub title: String,
    pub text: String,
    pub state: String,
    pub schema_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<String>,
    /// `core/depends-on` targets (`G:<uuid>`).
    pub dependencies: Vec<String>,
    /// Stored wake configuration; absent when the goal is not armed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wake: Option<WakeConfigItem>,
}

#[derive(Debug, Serialize)]
pub struct WakeConfigItem {
    /// Concrete trigger Fact reference (`F:<uuid>`), when fact-triggered.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger_fact: Option<String>,
    /// Schema trigger, when schema-triggered.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger_schema_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger_schema_version: Option<u32>,
    pub tool_ids: Vec<String>,
    pub prompt: String,
    /// Pinned wake-context references (`F:`/`A:`/`P:` prefixed ids).
    pub hard_memories: Vec<String>,
}

/// Keyset resume point carried inside the opaque goal cursor.
#[derive(Debug, Serialize, Deserialize)]
struct GoalCursorPos {
    created_at_nanos: i128,
    goal_id: uuid::Uuid,
}

fn goal_fingerprint(state_tag: Option<&str>) -> String {
    let canon = serde_json::to_string(&state_tag).expect("fingerprint canon serializes");
    wire_cursor::fingerprint(&canon)
}

/// # Errors
///
/// Returns invalid state/cursor arguments, authorization, or storage
/// failures.
pub async fn list_goals(
    ctx: McpToolCtx,
    args: ListGoalsArgs,
) -> Result<ListGoalsOutput, McpToolError> {
    let state = args.state.as_deref().map(parse_goal_state).transpose()?;
    let state_tag = state.map(state_str).map(str::to_string);
    let after = args
        .cursor
        .as_deref()
        .map(|raw| decode_goal_cursor(raw, state_tag.as_deref()))
        .transpose()?;
    let limit = super::resolve_page_limit(args.limit)?;

    let engine = ctx.require_engine()?;
    let mut req = QueryRequest::for_owner(ctx.owner);
    req.entity_kind = Some(EntityKind::Goal);
    req.goal_state = state;
    req.limit = limit;
    req.page = QueryPage { after };
    req.include_payloads = false;
    req.tombstones = TombstoneFilter::PresentOnly;
    req.supersession = SupersessionStatus::HeadsOnly;
    let response = engine.query(&ctx.authz, &req).await?;

    let goal_ids: Vec<GoalId> = response.goals.iter().map(|row| row.id).collect();
    let wake_configs = engine.read_goal_wake_configs(&ctx.authz, &goal_ids).await?;
    let goals = project_goals(&ctx, response.goals, wake_configs);
    let next_cursor = response.next_cursor.and_then(|cursor| match cursor {
        QueryCursor::Goal {
            created_at,
            goal_id,
        } => Some(encode_goal_cursor(
            created_at,
            goal_id.into_inner(),
            state_tag.as_deref(),
        )),
        QueryCursor::Memory { .. } => None,
    });
    let has_more = next_cursor.is_some();
    Ok(ListGoalsOutput {
        goals,
        next_cursor,
        has_more,
    })
}

/// # Errors
///
/// Returns malformed/unknown goal references, authorization, or storage
/// failures.
pub async fn get_goal(ctx: McpToolCtx, raw: &str) -> Result<GoalItem, McpToolError> {
    let goal_id = parse_prefixed_uuid(raw, PrefixedUuidClass::Goal)
        .map(GoalId::new)
        .map_err(|err| McpToolError::InvalidInput(err.to_string()))?;
    let engine = ctx.require_engine()?;
    let mut req = QueryRequest::for_owner(ctx.owner);
    req.entity_kind = Some(EntityKind::Goal);
    req.goal_ids = vec![goal_id];
    req.limit = 1;
    req.include_payloads = false;
    let response = engine.query(&ctx.authz, &req).await?;
    let wake_configs = engine
        .read_goal_wake_configs(&ctx.authz, &[goal_id])
        .await?;
    project_goals(&ctx, response.goals, wake_configs)
        .into_iter()
        .next()
        .ok_or_else(|| McpToolError::NotFound(format!("goal {raw} not found")))
}

fn project_goals(
    ctx: &McpToolCtx,
    rows: Vec<crate::verbs::query::GoalRow>,
    wake_configs: Vec<GoalWakeConfigRow>,
) -> Vec<GoalItem> {
    let mut wake_by_goal: std::collections::BTreeMap<uuid::Uuid, GoalWakeConfigRow> = wake_configs
        .into_iter()
        .map(|row| (row.goal_id.into_inner(), row))
        .collect();
    rows.into_iter()
        .map(|row| {
            let wake = wake_by_goal
                .remove(&row.id.into_inner())
                .map(|config| wake_config_item(ctx, config));
            GoalItem {
                goal: format_prefixed_uuid(row.id.into_inner(), PrefixedUuidClass::Goal),
                title: row.title,
                text: row.text,
                state: state_str(row.state).to_string(),
                schema_id: row.schema_id.as_str().to_string(),
                supersedes: row
                    .supersedes
                    .map(|id| format_prefixed_uuid(id.into_inner(), PrefixedUuidClass::Goal)),
                dependencies: row
                    .dependency_goal_ids
                    .into_iter()
                    .map(|id| format_prefixed_uuid(id.into_inner(), PrefixedUuidClass::Goal))
                    .collect(),
                wake,
            }
        })
        .collect()
}

fn wake_config_item(ctx: &McpToolCtx, config: GoalWakeConfigRow) -> WakeConfigItem {
    WakeConfigItem {
        trigger_fact: config
            .trigger_memory_id
            .map(|id| format_prefixed_uuid(id.into_inner(), PrefixedUuidClass::Fact)),
        trigger_schema_id: config
            .trigger_schema_id
            .map(|schema| schema.as_str().to_string()),
        trigger_schema_version: config
            .trigger_schema_version
            .map(crate::SchemaVersion::into_inner),
        tool_ids: config.tool_ids,
        prompt: config.prompt,
        hard_memories: config
            .hard_memories
            .iter()
            .map(|hard| {
                super::wire_ref::format_memory_by_kind(ctx, hard.memory_id, Some(hard.kind))
            })
            .collect(),
    }
}

fn parse_goal_state(raw: &str) -> Result<GoalState, McpToolError> {
    match raw.to_ascii_lowercase().as_str() {
        "active" => Ok(GoalState::Active),
        "paused" => Ok(GoalState::Paused),
        "achieved" => Ok(GoalState::Achieved),
        "abandoned" => Ok(GoalState::Abandoned),
        _ => Err(McpToolError::InvalidInput(format!(
            "state must be one of Active, Paused, Achieved, Abandoned; got '{raw}'"
        ))),
    }
}

fn state_str(state: GoalState) -> &'static str {
    match state {
        GoalState::Active => "Active",
        GoalState::Paused => "Paused",
        GoalState::Achieved => "Achieved",
        GoalState::Abandoned => "Abandoned",
    }
}

fn encode_goal_cursor(
    created_at: time::OffsetDateTime,
    goal_id: uuid::Uuid,
    state_tag: Option<&str>,
) -> String {
    GOAL_CURSOR.encode(
        &goal_fingerprint(state_tag),
        &GoalCursorPos {
            created_at_nanos: created_at.unix_timestamp_nanos(),
            goal_id,
        },
    )
}

fn decode_goal_cursor(raw: &str, state_tag: Option<&str>) -> Result<QueryCursor, McpToolError> {
    let pos: GoalCursorPos = GOAL_CURSOR.decode(&goal_fingerprint(state_tag), raw)?;
    let created_at = time::OffsetDateTime::from_unix_timestamp_nanos(pos.created_at_nanos)
        .map_err(|_| wire_cursor::malformed_cursor(GOAL_CURSOR.source))?;
    Ok(QueryCursor::Goal {
        created_at,
        goal_id: GoalId::new(pos.goal_id),
    })
}

#[cfg(test)]
mod tests {
    use super::{decode_goal_cursor, encode_goal_cursor, parse_goal_state};
    use crate::McpToolError;
    use crate::verbs::query::QueryCursor;

    #[test]
    fn goal_cursor_round_trips_and_binds_to_state() {
        let goal_id = uuid::Uuid::now_v7();
        let created_at = time::OffsetDateTime::UNIX_EPOCH + time::Duration::days(20_000);
        let token = encode_goal_cursor(created_at, goal_id, Some("Active"));
        match decode_goal_cursor(&token, Some("Active")).unwrap() {
            QueryCursor::Goal {
                created_at: decoded_at,
                goal_id: decoded_id,
            } => {
                assert_eq!(decoded_at, created_at);
                assert_eq!(decoded_id.into_inner(), goal_id);
            }
            other @ QueryCursor::Memory { .. } => panic!("expected goal cursor, got {other:?}"),
        }

        // Replay under a different (or missing) state filter fails closed.
        assert!(matches!(
            decode_goal_cursor(&token, Some("Paused")),
            Err(McpToolError::InvalidInput(message)) if message.contains("does not match")
        ));
        assert!(decode_goal_cursor(&token, None).is_err());
        assert!(matches!(
            decode_goal_cursor("garbage!!", Some("Active")),
            Err(McpToolError::InvalidInput(message)) if message.contains("malformed cursor")
        ));
    }

    #[test]
    fn goal_state_parse_is_case_insensitive_and_closed() {
        assert!(parse_goal_state("ACTIVE").is_ok());
        assert!(parse_goal_state("paused").is_ok());
        assert!(matches!(
            parse_goal_state("Superseded"),
            Err(McpToolError::InvalidInput(message)) if message.contains("must be one of")
        ));
    }
}

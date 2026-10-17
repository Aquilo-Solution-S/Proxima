//! Goal introspection reads backing `proxima://goals` and
//! `proxima://goal/{id}`: list-by-state with keyset pagination and a
//! single-goal read. Goals stay write-only through `core_goal`.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::GoalId;
use crate::mcp::cursor as wire_cursor;
use crate::mcp::{McpToolCtx, McpToolError};
use crate::verbs::goal_write::GoalState;
use crate::verbs::query::{EntityKind, QueryCursor, QueryPage, QueryRequest, SupersessionStatus};

/// Opaque cursor codec: the shared `{v, fp, c}` envelope with the goal
/// keyset under `c`. The fingerprint binds the state filter; `limit`
/// stays out so it may vary between pages.
const GOAL_CURSOR: wire_cursor::FingerprintedCursor = wire_cursor::FingerprintedCursor {
    version: 2,
    source: "proxima://goals page",
    rebind_hint: "repeat the state filter that produced it",
};

#[derive(Debug, Deserialize)]
pub struct ListGoalsArgs {
    /// Goal state union: comma-separated Active, Paused, Achieved, or
    /// Abandoned members (case-insensitive); omit for all states.
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
    /// Goal reference (`<uuid>`), as accepted by `core_goal`.
    pub goal: String,
    /// Stable series handle (`<uuid>`). `goal` is this version's `t` and is
    /// retained for write references.
    pub handle: String,
    pub by: uuid::Uuid,
    pub via: Option<uuid::Uuid>,
    pub title: String,
    /// The goal's prompt text as of this version; `null` when unknown (a
    /// version written before the text was stored, with no recorded
    /// declaration to recover it from).
    pub text: Option<String>,
    pub state: String,
    pub schema_id: String,
    /// When the goal was set: the time of its first version (RFC 3339).
    #[serde(with = "time::serde::rfc3339")]
    pub set_at: time::OffsetDateTime,
    /// When this version was written (RFC 3339); equal to `set_at` on the
    /// first version.
    #[serde(with = "time::serde::rfc3339")]
    pub changed_at: time::OffsetDateTime,
    /// Goals this one depends on (`<uuid>`), as declared on its own row.
    pub dependencies: Vec<String>,
}

/// Keyset resume point carried inside the opaque goal cursor.
#[derive(Debug, Serialize, Deserialize)]
struct GoalCursorPos {
    handle: uuid::Uuid,
    goal_id: uuid::Uuid,
}

fn goal_fingerprint(states: &BTreeSet<GoalState>) -> String {
    let canon = states
        .iter()
        .map(|state| state_str(*state))
        .collect::<Vec<_>>();
    let canon = serde_json::to_string(&canon).expect("fingerprint canon serializes");
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
    let states = parse_goal_states(args.state.as_deref())?;
    let after = args
        .cursor
        .as_deref()
        .map(|raw| decode_goal_cursor(raw, &states))
        .transpose()?;
    let limit = super::resolve_page_limit(args.limit)?;

    let engine = ctx.require_engine()?;
    let mut req = QueryRequest::readable();
    req.entity_kind = Some(EntityKind::Goal);
    req.goal_states = states.clone();
    req.limit = limit;
    req.page = QueryPage { after };
    req.include_payloads = false;
    req.supersession = SupersessionStatus::HeadsOnly;
    let response = engine.query(&ctx.authz, &req).await?;

    let goals = project_goals(response.goals);
    let next_cursor = response.next_cursor.and_then(|cursor| match cursor {
        QueryCursor::Goal { handle, goal_id } => {
            Some(encode_goal_cursor(handle, goal_id.into_inner(), &states))
        }
        QueryCursor::Memory { .. } => None,
    });
    let has_more = next_cursor.is_some();
    Ok(ListGoalsOutput {
        goals,
        next_cursor,
        has_more,
    })
}

/// Read a Goal series' current head. `raw` names the series or any of its
/// versions: `core_goal`, listed `goal` and `dependencies` values and Fact
/// Goal references all carry version ids, and only the listing carries series
/// handles.
///
/// # Errors
///
/// Returns malformed/unknown goal references, authorization, or storage
/// failures.
pub async fn get_goal(ctx: McpToolCtx, raw: &str) -> Result<GoalItem, McpToolError> {
    let id = ctx.resolve_goal(raw)?.into_inner();
    let engine = ctx.require_engine()?;
    let handle = series_handle(&ctx, engine, id).await?;
    let mut req = QueryRequest::readable();
    req.entity_kind = Some(EntityKind::Goal);
    req.goal_handles = vec![handle];
    req.limit = 1;
    req.include_payloads = false;
    let response = engine.query(&ctx.authz, &req).await?;
    project_goals(response.goals)
        .into_iter()
        .next()
        .ok_or_else(|| McpToolError::NotFound(format!("goal {raw} not found")))
}

/// The series a `<uuid>` names. A readable version resolves to its series;
/// anything else is taken as a series handle. Version ids and handles are
/// distinct UUIDs, so the version lookup cannot select another series.
async fn series_handle(
    ctx: &McpToolCtx,
    engine: &crate::Engine,
    id: uuid::Uuid,
) -> Result<uuid::Uuid, McpToolError> {
    let mut req = QueryRequest::readable();
    req.entity_kind = Some(EntityKind::Goal);
    req.goal_ids = vec![GoalId::new(id)];
    req.limit = 1;
    req.include_payloads = false;
    let response = engine.query(&ctx.authz, &req).await?;
    Ok(response.goals.first().map_or(id, |version| version.handle))
}

fn project_goals(rows: Vec<crate::verbs::query::GoalRow>) -> Vec<GoalItem> {
    rows.into_iter()
        .map(|row| GoalItem {
            goal: row.id.into_inner().to_string(),
            handle: row.handle.to_string(),
            by: row.writer.by,
            via: row.writer.via,
            title: row.title,
            text: row.text,
            state: state_str(row.state).to_string(),
            schema_id: row.schema_id.as_str().to_string(),
            set_at: row.set_at,
            changed_at: row.changed_at,
            dependencies: row
                .dependency_goal_ids
                .into_iter()
                .map(|id| id.into_inner().to_string())
                .collect(),
        })
        .collect()
}

fn parse_goal_states(raw: Option<&str>) -> Result<BTreeSet<GoalState>, McpToolError> {
    let Some(raw) = raw else {
        return Ok(BTreeSet::new());
    };
    let mut states = BTreeSet::new();
    for member in raw.split(',') {
        let member = member.trim();
        if member.is_empty() {
            return Err(McpToolError::InvalidInput(
                "state must not contain empty members".into(),
            ));
        }
        let state = match member.to_ascii_lowercase().as_str() {
            "active" => GoalState::Active,
            "paused" => GoalState::Paused,
            "achieved" => GoalState::Achieved,
            "abandoned" => GoalState::Abandoned,
            _ => {
                return Err(McpToolError::InvalidInput(format!(
                    "state must be one of Active, Paused, Achieved, Abandoned; got '{member}'"
                )));
            }
        };
        states.insert(state);
    }
    Ok(states)
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
    handle: uuid::Uuid,
    goal_id: uuid::Uuid,
    states: &BTreeSet<GoalState>,
) -> String {
    GOAL_CURSOR.encode(
        &goal_fingerprint(states),
        &GoalCursorPos { handle, goal_id },
    )
}

fn decode_goal_cursor(
    raw: &str,
    states: &BTreeSet<GoalState>,
) -> Result<QueryCursor, McpToolError> {
    let pos: GoalCursorPos = GOAL_CURSOR.decode(&goal_fingerprint(states), raw)?;
    Ok(QueryCursor::Goal {
        handle: pos.handle,
        goal_id: GoalId::new(pos.goal_id),
    })
}

#[cfg(test)]
mod tests {
    use super::{decode_goal_cursor, encode_goal_cursor, parse_goal_states, wire_cursor};
    use crate::McpToolError;
    use crate::verbs::query::QueryCursor;
    use std::collections::BTreeSet;

    #[test]
    fn goal_reads_keep_each_versions_recorded_writer() {
        let handle = crate::new_memory_uuid(0);
        let writers = [
            crate::WriterAttribution {
                by: uuid::Uuid::from_u128(1),
                via: Some(uuid::Uuid::from_u128(2)),
            },
            crate::WriterAttribution {
                by: uuid::Uuid::from_u128(3),
                via: None,
            },
        ];
        let rows = writers
            .into_iter()
            .map(|writer| crate::verbs::query::GoalRow {
                handle,
                id: crate::GoalId::new(crate::new_memory_uuid(0)),
                schema_id: crate::SchemaId::new("core/goal-v1".into()),
                owner: crate::OwnerId::new(uuid::Uuid::from_u128(4)),
                writer,
                title: "stored goal".into(),
                text: Some("stored prompt".into()),
                state: super::GoalState::Active,
                dependency_goal_ids: Vec::new(),
                assignment: None,
                evidence: Vec::new(),
                set_at: time::OffsetDateTime::UNIX_EPOCH,
                changed_at: time::OffsetDateTime::UNIX_EPOCH,
            })
            .collect();
        let goals = super::project_goals(rows);
        let single = serde_json::to_value(&goals[0]).expect("single goal output");
        let list = serde_json::to_value(super::ListGoalsOutput {
            goals,
            next_cursor: None,
            has_more: false,
        })
        .expect("goal list output");
        let reads = list["goals"].as_array().expect("goal versions");
        assert_eq!(reads.len(), writers.len());
        assert_eq!(single, reads[0]);
        for (read, writer) in reads.iter().zip(writers) {
            assert_eq!(read["by"], serde_json::json!(writer.by));
            assert_eq!(read.get("via"), Some(&serde_json::json!(writer.via)));
        }
    }

    #[test]
    fn goal_cursor_round_trips_and_binds_to_state() {
        let goal_id = uuid::Uuid::now_v7();
        let handle = uuid::Uuid::now_v7();
        let states = parse_goal_states(Some("paused, active,ACTIVE ")).unwrap();
        let token = encode_goal_cursor(handle, goal_id, &states);
        let reordered = parse_goal_states(Some("ACTIVE,PAUSED")).unwrap();
        assert_eq!(token, encode_goal_cursor(handle, goal_id, &reordered));
        match decode_goal_cursor(&token, &states).unwrap() {
            QueryCursor::Goal {
                handle: decoded_handle,
                goal_id: decoded_id,
            } => {
                assert_eq!(decoded_handle, handle);
                assert_eq!(decoded_id.into_inner(), goal_id);
            }
            other @ QueryCursor::Memory { .. } => panic!("expected goal cursor, got {other:?}"),
        }

        // Replay under a different (or missing) state filter fails closed.
        assert!(matches!(
            decode_goal_cursor(&token, &parse_goal_states(Some("Paused")).unwrap()),
            Err(McpToolError::InvalidInput(message)) if message.contains("does not match")
        ));
        assert!(decode_goal_cursor(&token, &BTreeSet::new()).is_err());
        assert!(matches!(
            decode_goal_cursor("garbage!!", &states),
            Err(McpToolError::InvalidInput(message)) if message.contains("malformed cursor")
        ));
        let old = wire_cursor::FingerprintedCursor {
            version: 1,
            source: "proxima://goals page",
            rebind_hint: "repeat the state filter that produced it",
        }
        .encode(
            &super::goal_fingerprint(&states),
            &super::GoalCursorPos { handle, goal_id },
        );
        assert!(matches!(
            decode_goal_cursor(&old, &states),
            Err(McpToolError::InvalidInput(message)) if message.contains("malformed cursor")
        ));
    }

    #[test]
    fn goal_state_parse_is_case_insensitive_and_closed() {
        assert_eq!(
            parse_goal_states(Some("ACTIVE, paused,active"))
                .unwrap()
                .len(),
            2
        );
        assert!(parse_goal_states(Some("paused")).is_ok());
        assert!(matches!(
            parse_goal_states(Some("Superseded")),
            Err(McpToolError::InvalidInput(message)) if message.contains("must be one of")
        ));
        assert!(parse_goal_states(Some(",active")).is_err());
        for raw in ["", " ", "Active,", "Active,,Paused"] {
            assert!(parse_goal_states(Some(raw)).is_err(), "{raw:?}");
        }
    }
}

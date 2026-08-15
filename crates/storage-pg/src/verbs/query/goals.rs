use std::fmt::Write as _;

use proxima_core::verbs::goal_write::GoalState;
use proxima_core::verbs::query::{
    EntityKind, GoalRow, QueryCursor, QueryRequest, SupersessionStatus,
};
use proxima_core::StorageError;
use sqlx::PgPool;

use crate::error::map_err;

use super::rows::{GoalRowDb, goal_row_from_db};

pub(super) async fn query_goals(
    pool: &PgPool,
    req: &QueryRequest,
    owner_ids: &[uuid::Uuid],
    schema_id_filter: Option<&str>,
) -> Result<(Vec<GoalRow>, Option<QueryCursor>), StorageError> {
    let goal_ids: Vec<uuid::Uuid> = req.goal_ids.iter().map(|id| id.into_inner()).collect();
    let id_hydration = !req.memory_ids.is_empty() || !req.goal_ids.is_empty();
    if id_hydration && goal_ids.is_empty() {
        return Ok((Vec::new(), None));
    }
    let cursor = match &req.page.after {
        Some(QueryCursor::Goal { goal_id, .. }) => Some(goal_id.into_inner()),
        _ => None,
    };
    let single_goal_stream = matches!(req.entity_kind, Some(EntityKind::Goal));
    let sql = goal_page_sql(req, schema_id_filter.is_some());

    // SQL-POLICY: fixed-fragment
    let mut q = sqlx::query_as::<_, GoalRowDb>(sqlx::AssertSqlSafe(sql)).bind(owner_ids);
    if let Some(sid) = schema_id_filter {
        q = q.bind(sid.to_string());
    }
    if !goal_ids.is_empty() {
        q = q.bind(goal_ids);
    }
    if let Some(goal_id) = cursor {
        q = q.bind(goal_id);
    }
    let mut rows = q.fetch_all(pool).await.map_err(map_err)?;
    let limit = usize::try_from(req.limit)
        .map_err(|_| StorageError::Internal("query limit does not fit usize".into()))?;
    let next_cursor = if single_goal_stream && rows.len() > limit {
        rows.truncate(limit);
        rows.last().map(|row| QueryCursor::Goal {
            created_at: row.created_at,
            goal_id: proxima_core::GoalId::new(row.goal_id),
        })
    } else {
        None
    };
    let goals = rows
        .into_iter()
        .map(goal_row_from_db)
        .collect::<Result<Vec<_>, _>>()?;
    Ok((goals, next_cursor))
}

fn goal_page_sql(req: &QueryRequest, has_schema_filter: bool) -> String {
    let has_goal_ids = !req.goal_ids.is_empty();
    let has_cursor = matches!(&req.page.after, Some(QueryCursor::Goal { .. }));
    let single_goal_stream = matches!(req.entity_kind, Some(EntityKind::Goal));
    let fetch_limit = if single_goal_stream {
        u64::from(req.limit) + 1
    } else {
        u64::from(req.limit)
    };
    let from = if matches!(req.supersession, SupersessionStatus::HeadsOnly) && !has_goal_ids {
        "FROM proxima_core.goal_head h \
         JOIN proxima_core.goal g ON g.handle = h.handle AND g.t = h.t"
    } else {
        "FROM proxima_core.goal g \
         JOIN proxima_core.goal_head h ON h.handle = g.handle"
    };
    let mut sql = format!(
        "SELECT g.handle, g.t AS goal_id, \
                COALESCE(uuid_extract_timestamp(g.t), TIMESTAMPTZ '1970-01-01') AS created_at, \
                h.schema_id, 1::int4 AS schema_version, \
                o.kind::text::proxima_core.owner_ref_kind AS owner_kind, \
                g.owner_id, g.title, ''::text AS text, g.state, \
                NULL::uuid AS supersedes, ''::bytea AS payload, \
                g.dependency_t AS dependency_goal_ids \
         {from} \
         JOIN proxima_core.owners o ON o.owner_id = g.owner_id \
         WHERE g.owner_id = ANY($1::uuid[])"
    );
    let mut next = 2_u32;
    if has_schema_filter {
        let _ = write!(sql, " AND h.schema_id = ${next}");
        next += 1;
    }
    if let Some(state) = req.goal_state {
        sql.push_str(match state {
            GoalState::Active => " AND g.state = 'Active'",
            GoalState::Paused => " AND g.state = 'Paused'",
            GoalState::Achieved => " AND g.state = 'Achieved'",
            GoalState::Abandoned => " AND g.state = 'Abandoned'",
        });
    }
    if has_goal_ids {
        let _ = write!(sql, " AND g.t = ANY(${next}::uuid[])");
        next += 1;
    }
    if has_cursor {
        let _ = write!(sql, " AND g.t < ${next}");
    }
    let _ = write!(sql, " ORDER BY g.t DESC LIMIT {fetch_limit}");
    sql
}

/// Emit the exact page statement [`query_goals`] would run for `req` — the
/// golden-pin surface for the tuning arms, compiled only for tests. Same
/// cfg gate as the search `*_sql_for_tests` exports.
#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
#[doc(hidden)]
#[must_use]
pub fn goal_page_sql_for_tests(req: &QueryRequest) -> String {
    goal_page_sql(req, req.schema_id.is_some())
}

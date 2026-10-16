use std::fmt::Write as _;

use proxima_core::StorageError;
use proxima_core::verbs::goal_write::GoalState;
use proxima_core::verbs::query::{
    EntityKind, GoalRow, QueryCursor, QueryRequest, SupersessionStatus,
};
use sqlx::PgConnection;

use crate::error::map_err;

use super::rows::{GoalRowDb, goal_row_from_db};

pub(super) async fn query_goals_on_connection(
    connection: &mut PgConnection,
    req: &QueryRequest,
    owner_ids: &[uuid::Uuid],
) -> Result<(Vec<GoalRow>, Option<QueryCursor>), StorageError> {
    let id_hydration = !req.memory_ids.is_empty() || !req.goal_ids.is_empty();
    if id_hydration && req.goal_ids.is_empty() {
        return Ok((Vec::new(), None));
    }
    let single_goal_stream = matches!(req.entity_kind, Some(EntityKind::Goal));
    let filters = goal_filters(req);
    let sql = goal_page_sql(req, &filters);
    // SQL-POLICY: fixed-fragment
    let mut q = sqlx::query_as::<_, GoalRowDb>(sqlx::AssertSqlSafe(sql)).bind(owner_ids);
    for filter in filters {
        q = match filter {
            GoalFilter::Schema(value) => q.bind(value),
            GoalFilter::States(value) => q.bind(value),
            GoalFilter::Assignment(value) | GoalFilter::Evidence(value) => q.bind(value),
            GoalFilter::Ids(value) | GoalFilter::Handles(value) => q.bind(value),
            GoalFilter::Cursor(handle, goal_id) => q.bind(handle).bind(goal_id),
        };
    }
    let mut rows = q.fetch_all(&mut *connection).await.map_err(map_err)?;
    let limit = usize::try_from(req.limit)
        .map_err(|_| StorageError::Internal("query limit does not fit usize".into()))?;
    let next_cursor = if single_goal_stream && rows.len() > limit {
        rows.truncate(limit);
        rows.last().map(|row| QueryCursor::Goal {
            handle: row.handle,
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

/// The time a Goal `t` was minted at: the timestamp inside the `UUIDv7`, or the
/// epoch for a `t` that carries none.
fn t_timestamp_sql(t: &str) -> String {
    format!("COALESCE(uuid_extract_timestamp({t}), TIMESTAMPTZ '1970-01-01')")
}

fn goal_page_sql(req: &QueryRequest, filters: &[GoalFilter]) -> String {
    let has_goal_ids = !req.goal_ids.is_empty();
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
    // `created_at` is this version's time (`changed_at`); `set_at` is
    // the time of the series' first version, the
    // least `t` among its rows. Both come from the same `t` derivation.
    let created_at = t_timestamp_sql("g.t");
    let set_at = t_timestamp_sql(
        "(SELECT series.t FROM proxima_core.goal series \
           WHERE series.handle = g.handle ORDER BY series.t LIMIT 1)",
    );
    let mut sql = format!(
        "SELECT g.handle, g.t AS goal_id, \
                {created_at} AS created_at, \
                {set_at} AS set_at, \
                h.schema_id, \
                g.owner_id, g.title, g.text, g.state, \
                g.dependency_t AS dependency_goal_ids, \
                g.assignment_t AS assignment, \
                g.evidence_t AS evidence \
         {from} \
         JOIN proxima_core.owners o ON o.owner_id = g.owner_id \
         WHERE g.owner_id = ANY($1::uuid[])"
    );
    let heads_only = matches!(req.supersession, SupersessionStatus::HeadsOnly) && !has_goal_ids;
    let mut next = 2_u32;
    for filter in filters {
        // SQL-POLICY: fixed-fragment — GoalFilter predicates contain only
        // literals and numbered placeholders; outside values are binds below.
        sql.push_str(&filter.predicate(next, heads_only));
        next += filter.bind_count();
    }
    let _ = write!(sql, " ORDER BY g.handle DESC, g.t DESC LIMIT {fetch_limit}");
    sql
}

enum GoalFilter {
    Schema(String),
    States(Vec<GoalState>),
    Assignment(uuid::Uuid),
    Evidence(uuid::Uuid),
    Ids(Vec<uuid::Uuid>),
    Handles(Vec<uuid::Uuid>),
    Cursor(uuid::Uuid, uuid::Uuid),
}

impl GoalFilter {
    fn bind_count(&self) -> u32 {
        match self {
            Self::Cursor(..) => 2,
            Self::Schema(_)
            | Self::States(_)
            | Self::Assignment(_)
            | Self::Evidence(_)
            | Self::Ids(_)
            | Self::Handles(_) => 1,
        }
    }

    fn predicate(&self, placeholder: u32, heads_only: bool) -> String {
        match self {
            Self::Schema(_) => format!(" AND h.schema_id = ${placeholder}"),
            Self::States(_) => {
                format!(" AND g.state = ANY(${placeholder}::proxima_core.goal_state[])")
            }
            Self::Assignment(_) => format!(" AND g.assignment_t = ${placeholder}"),
            Self::Evidence(_) => format!(" AND ${placeholder} = ANY(g.evidence_t)"),
            Self::Ids(_) => format!(" AND g.t = ANY(${placeholder}::uuid[])"),
            Self::Handles(_) => format!(
                " AND {}.handle = ANY(${placeholder}::uuid[])",
                if heads_only { "h" } else { "g" }
            ),
            Self::Cursor(_, _) => format!(
                " AND (g.handle, g.t) < (${placeholder}, ${})",
                placeholder + 1
            ),
        }
    }
}

fn goal_filters(req: &QueryRequest) -> Vec<GoalFilter> {
    let mut filters = Vec::new();
    if let Some(schema) = &req.schema_id {
        filters.push(GoalFilter::Schema(schema.as_str().to_owned()));
    }
    if !req.goal_states.is_empty() {
        filters.push(GoalFilter::States(
            req.goal_states.iter().copied().collect(),
        ));
    }
    if let Some(assignment) = req.assignment {
        filters.push(GoalFilter::Assignment(assignment.into_inner()));
    }
    if let Some(evidence) = req.evidence_contains {
        filters.push(GoalFilter::Evidence(evidence.into_inner()));
    }
    if !req.goal_ids.is_empty() {
        filters.push(GoalFilter::Ids(
            req.goal_ids.iter().map(|id| id.into_inner()).collect(),
        ));
    }
    if !req.goal_handles.is_empty() {
        filters.push(GoalFilter::Handles(req.goal_handles.clone()));
    }
    if let Some(QueryCursor::Goal { handle, goal_id }) = &req.page.after {
        filters.push(GoalFilter::Cursor(*handle, goal_id.into_inner()));
    }
    filters
}

/// One Active head per matching Goal for the given memory `t`s.
///
/// A row matches when `assignment_t` is in `targets` or `evidence_t`
/// overlaps `targets`. Empty `targets` returns no rows and does not query.
///
/// # Errors
///
/// Returns `StorageError::Internal` on query failure.
pub async fn active_goals_for_memory_targets_on_connection(
    connection: &mut PgConnection,
    owner_id: uuid::Uuid,
    targets: &[uuid::Uuid],
) -> Result<Vec<ActiveGoalTargetRow>, StorageError> {
    if targets.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_as(
        "SELECT g.t AS goal_id, g.assignment_t, g.evidence_t
           FROM proxima_core.goal_head h
           JOIN proxima_core.goal g ON g.handle = h.handle AND g.t = h.t
          WHERE h.owner_id = $1
            AND g.state = 'Active'
            AND (g.assignment_t = ANY($2::uuid[]) OR g.evidence_t && $2::uuid[])",
    )
    .bind(owner_id)
    .bind(targets)
    .fetch_all(&mut *connection)
    .await
    .map_err(map_err)
}

/// One Active Goal head that names a work-item neighbour.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ActiveGoalTargetRow {
    pub goal_id: uuid::Uuid,
    pub assignment_t: Option<uuid::Uuid>,
    pub evidence_t: Vec<uuid::Uuid>,
}

#[cfg(test)]
mod tests {

    #[test]
    fn batch_activation_sql_is_one_shot() {
        let src = include_str!("goals.rs");
        assert!(
            src.contains("g.assignment_t = ANY($2::uuid[])"),
            "assignment must be batched"
        );
        assert!(
            src.contains("g.evidence_t && $2::uuid[]"),
            "evidence must be batched"
        );
        assert!(
            src.contains("if targets.is_empty()"),
            "empty target set must not query"
        );
    }
}

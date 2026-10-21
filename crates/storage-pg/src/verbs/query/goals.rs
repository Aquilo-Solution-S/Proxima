use std::fmt::Write as _;

use proxima_core::StorageError;
use proxima_core::verbs::goal_write::GoalState;
use proxima_core::verbs::query::{
    EntityKind, GoalAssignmentFilter, GoalRow, QueryCursor, QueryRequest, SupersessionStatus,
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
            GoalFilter::Evidence(value) => q.bind(value),
            GoalFilter::Unassigned => q,
            GoalFilter::Assignments(value)
            | GoalFilter::Ids(value)
            | GoalFilter::Handles(value) => q.bind(value),
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

/// The time a Goal `t` was minted at, decoded by the shared UUID timestamp function.
fn t_timestamp_sql(t: &str) -> String {
    format!("proxima_core.memory_created_at({t})")
}

/// The columns of a goal row, as [`GoalRowDb`] reads them. `g` is the goal
/// version and `h` its series head.
fn goal_columns() -> String {
    // `created_at` is this version's time (`changed_at`); `set_at` is
    // the time of the series' first version, the
    // least `t` among its rows. Both come from the same `t` derivation.
    let created_at = t_timestamp_sql("g.t");
    let set_at = t_timestamp_sql(
        "(SELECT series.t FROM proxima_core.goal series \
           WHERE series.handle = g.handle ORDER BY series.t LIMIT 1)",
    );
    format!(
        "g.handle, g.t AS goal_id, \
         {created_at} AS created_at, \
         {set_at} AS set_at, \
         h.schema_id, \
         g.owner_id, g.\"by\", g.via, g.title, g.text, g.state, \
         g.dependency_t AS dependency_goal_ids, \
         g.assignment_t AS assignment, \
         g.evidence_t AS evidence"
    )
}

fn goal_page_sql(req: &QueryRequest, filters: &[GoalFilter]) -> String {
    let has_goal_ids = !req.goal_ids.is_empty();
    let single_goal_stream = matches!(req.entity_kind, Some(EntityKind::Goal));
    let fetch_limit = if single_goal_stream {
        u64::from(req.limit) + 1
    } else {
        u64::from(req.limit)
    };
    let heads_only = matches!(req.supersession, SupersessionStatus::HeadsOnly) && !has_goal_ids;
    if heads_only
        && filters.iter().all(|filter| {
            !matches!(
                filter,
                GoalFilter::Unassigned | GoalFilter::Assignments(_) | GoalFilter::Evidence(_)
            )
        })
    {
        return owner_head_page_sql(filters, fetch_limit);
    }
    let from = if heads_only {
        "FROM proxima_core.goal_head h \
         JOIN proxima_core.goal g ON g.handle = h.handle AND g.t = h.t"
    } else {
        "FROM proxima_core.goal g \
         JOIN proxima_core.goal_head h ON h.handle = g.handle"
    };
    let columns = goal_columns();
    let mut sql = format!(
        "SELECT {columns} \
         {from} \
         JOIN proxima_core.owners o ON o.owner_id = g.owner_id \
         WHERE g.owner_id = ANY($1::uuid[])"
    );
    append_goal_filters(&mut sql, filters, |filter, next| {
        filter.predicate(next, heads_only)
    });
    let _ = write!(sql, " ORDER BY g.handle DESC, g.t DESC LIMIT {fetch_limit}");
    sql
}

/// Append each filter's predicate, numbering its binds from `$2` (`$1` is the
/// owner list).
fn append_goal_filters(
    sql: &mut String,
    filters: &[GoalFilter],
    predicate: impl Fn(&GoalFilter, u32) -> String,
) {
    let mut next = 2_u32;
    for filter in filters {
        // SQL-POLICY: fixed-fragment — GoalFilter predicates contain only
        // literals and numbered placeholders; outside values are binds below.
        sql.push_str(&predicate(filter, next));
        next += filter.bind_count();
    }
}

/// Current goals by series handle, newest first, filtered by what the head or
/// the goal's state says: each owner's heads are read in order from the
/// owner-leading head index, each head's version is probed by its unique `t`,
/// and an owner stops at the page. The top of the union is contained in the
/// union of each owner's top, so the owners' pages merge into one. A page
/// reads the goals it returns however many goals the store holds, and a reader
/// with few goals does not read the other owners'.
///
/// The join is on `t` alone: it is unique, and a second equality on `handle`
/// makes the planner estimate the join at one row and choose a scan of both
/// tables.
fn owner_head_page_sql(filters: &[GoalFilter], fetch_limit: u64) -> String {
    let columns = goal_columns();
    let mut sql = format!(
        "SELECT page.* \
         FROM proxima_core.owners o CROSS JOIN LATERAL ( \
           SELECT {columns} \
           FROM proxima_core.goal_head h \
           JOIN proxima_core.goal g ON g.t = h.t \
           WHERE h.owner_id = o.owner_id AND g.owner_id = o.owner_id"
    );
    append_goal_filters(&mut sql, filters, GoalFilter::head_predicate);
    let _ = write!(
        sql,
        " ORDER BY h.handle DESC LIMIT {fetch_limit} \
         ) page WHERE o.owner_id = ANY($1::uuid[]) \
         ORDER BY page.handle DESC, page.goal_id DESC LIMIT {fetch_limit}"
    );
    sql
}

/// The statement [`query_goals_on_connection`] runs for `req`: `$1` is the
/// owner list and the filter binds follow in [`goal_filters`] order.
#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
#[doc(hidden)]
#[must_use]
pub fn goal_page_sql_for_tests(req: &QueryRequest) -> String {
    goal_page_sql(req, &goal_filters(req))
}

enum GoalFilter {
    Schema(String),
    States(Vec<GoalState>),
    Unassigned,
    Assignments(Vec<uuid::Uuid>),
    Evidence(uuid::Uuid),
    Ids(Vec<uuid::Uuid>),
    Handles(Vec<uuid::Uuid>),
    Cursor(uuid::Uuid, uuid::Uuid),
}

impl GoalFilter {
    fn bind_count(&self) -> u32 {
        match self {
            Self::Cursor(..) => 2,
            Self::Unassigned => 0,
            Self::Schema(_)
            | Self::States(_)
            | Self::Assignments(_)
            | Self::Evidence(_)
            | Self::Ids(_)
            | Self::Handles(_) => 1,
        }
    }

    /// The predicate in a page that reads heads first (`h`) and probes the
    /// version (`g`): the cursor starts the head scan, the rest is as
    /// [`Self::predicate`] for heads.
    fn head_predicate(&self, placeholder: u32) -> String {
        match self {
            Self::Cursor(_, _) => format!(
                " AND (h.handle, h.t) < (${placeholder}, ${})",
                placeholder + 1
            ),
            _ => self.predicate(placeholder, true),
        }
    }

    fn predicate(&self, placeholder: u32, heads_only: bool) -> String {
        match self {
            Self::Schema(_) => format!(" AND h.schema_id = ${placeholder}"),
            Self::States(_) => {
                format!(" AND g.state = ANY(${placeholder}::proxima_core.goal_state[])")
            }
            Self::Unassigned => " AND g.assignment_t IS NULL".to_owned(),
            Self::Assignments(_) => format!(" AND g.assignment_t = ANY(${placeholder}::uuid[])"),
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
    match &req.assignment {
        GoalAssignmentFilter::Any => {}
        GoalAssignmentFilter::Unassigned => filters.push(GoalFilter::Unassigned),
        GoalAssignmentFilter::AssignedTo(perspectives) => filters.push(GoalFilter::Assignments(
            perspectives.iter().map(|id| id.into_inner()).collect(),
        )),
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

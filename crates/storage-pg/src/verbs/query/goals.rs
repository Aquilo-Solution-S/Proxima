use std::fmt::Write as _;

use proxiima_core::StorageError;
use proxiima_core::verbs::goal_write::GoalState;
use proxiima_core::verbs::query::{
    EntityKind, GoalMotivationFilter, GoalRow, QueryCursor, QueryRequest, SupersessionStatus,
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
    let visible_motivations = match &req.motivated_by {
        GoalMotivationFilter::MotivatedBy(ids) => {
            let candidates = ids.iter().map(|id| id.into_inner()).collect::<Vec<_>>();
            Some(load_visible_perspective_ids(connection, owner_ids, &candidates).await?)
        }
        GoalMotivationFilter::Any | GoalMotivationFilter::Unmotivated => None,
    };
    if visible_motivations.as_ref().is_some_and(Vec::is_empty) {
        return Ok((Vec::new(), None));
    }
    let filters = goal_filters(req, visible_motivations.as_deref());
    let sql = goal_page_sql(req, &filters);
    // SQL-POLICY: fixed-fragment
    let mut q = sqlx::query_as::<_, GoalRowDb>(sqlx::AssertSqlSafe(sql)).bind(owner_ids);
    for filter in filters {
        q = match filter {
            GoalFilter::Schema(value) => q.bind(value),
            GoalFilter::States(value) => q.bind(value),
            GoalFilter::Evidence(value) => q.bind(value),
            GoalFilter::Unmotivated => q,
            GoalFilter::MotivatedBy(value)
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
            goal_id: proxiima_core::GoalId::new(row.goal_id),
        })
    } else {
        None
    };
    let mut goals = rows
        .into_iter()
        .map(goal_row_from_db)
        .collect::<Result<Vec<_>, _>>()?;
    filter_goal_memory_references(connection, owner_ids, &mut goals).await?;
    Ok((goals, next_cursor))
}

/// Goal causal metadata is stored as exact Memory ids, but a readable Goal
/// does not make an erased or foreign Perspective readable.  Apply the same
/// owner-scoped visibility boundary used by Memory reads before projecting
/// either the new motivation field or the historical assignment pin.
async fn filter_goal_memory_references(
    connection: &mut PgConnection,
    owner_ids: &[uuid::Uuid],
    rows: &mut [GoalRow],
) -> Result<(), StorageError> {
    let candidates: Vec<uuid::Uuid> = rows
        .iter()
        .flat_map(|row| {
            row.historical_assignment
                .map(proxiima_core::MemoryId::into_inner)
                .into_iter()
                .chain(row.motivated_by.iter().map(|id| id.into_inner()))
        })
        .collect();
    if candidates.is_empty() || owner_ids.is_empty() {
        return Ok(());
    }

    let visible: std::collections::HashSet<_> =
        load_visible_perspective_ids(connection, owner_ids, &candidates)
            .await?
            .into_iter()
            .collect();

    for row in rows {
        if row
            .historical_assignment
            .is_some_and(|id| !visible.contains(&id.into_inner()))
        {
            row.historical_assignment = None;
        }
        row.motivated_by
            .retain(|id| visible.contains(&id.into_inner()));
    }
    Ok(())
}

async fn load_visible_perspective_ids(
    connection: &mut PgConnection,
    owner_ids: &[uuid::Uuid],
    candidates: &[uuid::Uuid],
) -> Result<Vec<uuid::Uuid>, StorageError> {
    if candidates.is_empty() || owner_ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows: Vec<uuid::Uuid> = sqlx::query_scalar(
        "SELECT t
           FROM proxiima_core.memory
          WHERE t = ANY($1::uuid[])
            AND owner_id = ANY($2::uuid[])
            AND kind = 'perspective'::proxiima_core.memory_kind",
    )
    .bind(candidates)
    .bind(owner_ids)
    .fetch_all(&mut *connection)
    .await
    .map_err(map_err)?
    .into_iter()
    .collect();
    Ok(rows)
}

/// The time a Goal `t` was minted at, decoded by the shared UUID timestamp function.
fn t_timestamp_sql(t: &str) -> String {
    format!("proxiima_core.memory_created_at({t})")
}

/// The columns of a goal row, as [`GoalRowDb`] reads them. `g` is the goal
/// version and `h` its series head.
fn goal_columns() -> String {
    // `created_at` is this version's time (`changed_at`); `set_at` is
    // the time of the series' first version, the
    // least `t` among its rows. Both come from the same `t` derivation.
    let created_at = t_timestamp_sql("g.t");
    let set_at = t_timestamp_sql(
        "(SELECT series.t FROM proxiima_core.goal series \
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
         g.motivated_by, \
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
                GoalFilter::Unmotivated | GoalFilter::MotivatedBy(_) | GoalFilter::Evidence(_)
            )
        })
    {
        return audience_head_page_sql(filters, fetch_limit);
    }
    let from = if heads_only {
        "FROM proxiima_core.goal_head h \
         JOIN proxiima_core.goal g ON g.handle = h.handle AND g.t = h.t"
    } else {
        "FROM proxiima_core.goal g \
         JOIN proxiima_core.goal_head h ON h.handle = g.handle"
    };
    let columns = goal_columns();
    let mut sql = format!(
        "SELECT {columns} \
         {from} \
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
         FROM proxiima_core.owners o CROSS JOIN LATERAL ( \
           SELECT {columns} \
           FROM proxiima_core.goal_head h \
           JOIN proxiima_core.goal g ON g.t = h.t \
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

/// Keep the indexed owner stream and merge only explicitly granted Goal heads.
/// Goal controller owners remain selectors, never owner-role entries.
fn audience_head_page_sql(filters: &[GoalFilter], fetch_limit: u64) -> String {
    let owner = owner_head_page_sql(filters, fetch_limit);
    let columns = goal_columns();
    let mut guests = format!(
        "SELECT {columns} FROM proxiima_core.goal_access_grant a \
         JOIN proxiima_core.goal_head h ON h.handle=a.goal_handle \
         JOIN proxiima_core.goal g ON g.t=h.t \
         WHERE a.subject_id=NULLIF(current_setting('app.goal_subject',true),'')::uuid \
           AND a.controller_generation=h.authority_generation AND a.read_ceiling>=4 \
           AND (a.expires_at IS NULL OR a.expires_at>statement_timestamp()) \
           AND g.owner_id=ANY($1::uuid[])"
    );
    append_goal_filters(&mut guests, filters, GoalFilter::head_predicate);
    let _ = write!(guests, " ORDER BY h.handle DESC LIMIT {fetch_limit}");
    format!(
        "SELECT * FROM (({owner}) UNION ({guests})) visible ORDER BY handle DESC,goal_id DESC LIMIT {fetch_limit}"
    )
}

/// The statement [`query_goals_on_connection`] runs for `req`: `$1` is the
/// owner list and the filter binds follow in [`goal_filters`] order.
#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
#[doc(hidden)]
#[must_use]
pub fn goal_page_sql_for_tests(req: &QueryRequest) -> String {
    goal_page_sql(req, &goal_filters(req, None))
}

enum GoalFilter {
    Schema(String),
    States(Vec<GoalState>),
    Unmotivated,
    MotivatedBy(Vec<uuid::Uuid>),
    Evidence(uuid::Uuid),
    Ids(Vec<uuid::Uuid>),
    Handles(Vec<uuid::Uuid>),
    Cursor(uuid::Uuid, uuid::Uuid),
}

impl GoalFilter {
    fn bind_count(&self) -> u32 {
        match self {
            Self::Cursor(..) => 2,
            Self::Unmotivated => 0,
            Self::Schema(_)
            | Self::States(_)
            | Self::MotivatedBy(_)
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
                format!(" AND g.state = ANY(${placeholder}::proxiima_core.goal_state[])")
            }
            Self::Unmotivated => " AND g.motivated_by = '{}'::uuid[]".to_owned(),
            Self::MotivatedBy(_) => format!(
                " AND g.t IN (SELECT mi.goal_t \
                    FROM proxiima_core.goal_motivation_index mi \
                   WHERE mi.owner_id = ANY($1::uuid[]) \
                     AND mi.perspective_t = ANY(${placeholder}::uuid[]))"
            ),
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

fn goal_filters(req: &QueryRequest, visible_motivations: Option<&[uuid::Uuid]>) -> Vec<GoalFilter> {
    let mut filters = Vec::new();
    if let Some(schema) = &req.schema_id {
        filters.push(GoalFilter::Schema(schema.as_str().to_owned()));
    }
    if !req.goal_states.is_empty() {
        filters.push(GoalFilter::States(
            req.goal_states.iter().copied().collect(),
        ));
    }
    match &req.motivated_by {
        GoalMotivationFilter::Any => {}
        GoalMotivationFilter::Unmotivated => filters.push(GoalFilter::Unmotivated),
        GoalMotivationFilter::MotivatedBy(perspectives) => {
            filters.push(GoalFilter::MotivatedBy(visible_motivations.map_or_else(
                || perspectives.iter().map(|id| id.into_inner()).collect(),
                <[uuid::Uuid]>::to_vec,
            )));
        }
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

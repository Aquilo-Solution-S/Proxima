use std::fmt::Write as _;

use proxima_core::verbs::goal_write::GoalState;
use proxima_core::verbs::query::{
    EntityKind, GoalRow, QueryCursor, QueryRequest, SupersessionStatus,
};
use proxima_core::{OwnerRefKind, StorageError};
use sqlx::PgPool;

use crate::error::map_err;

use super::rows::{GoalRowDb, goal_row_from_db};

pub(super) async fn query_goals(
    pool: &PgPool,
    req: &QueryRequest,
    read_owner_kinds: &[OwnerRefKind],
    read_owner_ids: &[Option<uuid::Uuid>],
    schema_id_filter: Option<&str>,
) -> Result<(Vec<GoalRow>, Option<QueryCursor>), StorageError> {
    let goal_ids: Vec<uuid::Uuid> = req.goal_ids.iter().map(|id| id.into_inner()).collect();
    let id_hydration = !req.memory_ids.is_empty() || !req.goal_ids.is_empty();
    if id_hydration && goal_ids.is_empty() {
        return Ok((Vec::new(), None));
    }
    let cursor = match &req.page.after {
        Some(QueryCursor::Goal {
            created_at,
            goal_id,
        }) => Some((*created_at, goal_id.into_inner())),
        _ => None,
    };
    let single_goal_stream = matches!(req.entity_kind, Some(EntityKind::Goal));
    let sql = goal_page_sql(req, schema_id_filter.is_some());

    // SQL-POLICY: fixed-fragment
    let mut q = sqlx::query_as::<_, GoalRowDb>(sqlx::AssertSqlSafe(sql))
        .bind(read_owner_kinds)
        .bind(read_owner_ids);
    if let Some(sid) = schema_id_filter {
        q = q.bind(sid.to_string());
    }
    if !goal_ids.is_empty() {
        q = q.bind(goal_ids);
    }
    if let Some((created_at, goal_id)) = cursor {
        q = q.bind(created_at).bind(goal_id);
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

/// The goals keyset page statement for one tuning arm. `$1`/`$2` are the
/// read owner arrays; the remaining parameter numbers are allocated here in
/// the same order [`query_goals`] binds them.
#[allow(clippy::too_many_lines)]
fn goal_page_sql(req: &QueryRequest, has_schema_filter: bool) -> String {
    let payload_projection = if req.include_payloads {
        "g.payload"
    } else {
        "''::bytea"
    };
    let has_goal_ids = !req.goal_ids.is_empty();
    let has_cursor = matches!(&req.page.after, Some(QueryCursor::Goal { .. }));
    let single_goal_stream = matches!(req.entity_kind, Some(EntityKind::Goal));
    let fetch_limit = if single_goal_stream {
        u64::from(req.limit) + 1
    } else {
        u64::from(req.limit)
    };
    // Bindings: $1=owner_kind, $2=owner_id; the remaining params are pushed
    // in order, so optional filters and keyset cursors remain bound values.
    let mut next_param = 3;
    let schema_param = has_schema_filter.then(|| {
        let param = next_param;
        next_param += 1;
        param
    });
    let goal_ids_param = has_goal_ids.then(|| {
        let param = next_param;
        next_param += 1;
        param
    });
    let cursor_params = has_cursor.then(|| {
        let created_at = next_param;
        next_param += 1;
        let goal_id = next_param;
        (created_at, goal_id)
    });

    // One owner-scoped page body, shared by every arm below.
    let inner_page = |owner_predicate: &str| -> String {
        let mut inner = format!(
            "SELECT g.goal_id, g.created_at, g.schema_id, g.schema_version, \
                    g.owner_kind, g.owner_id, \
                    g.title, g.text, g.state, \
                    g.supersedes, {payload_projection} AS payload, \
                    g.dependency_goal_ids \
               FROM proxima_core.goals g \
              WHERE {owner_predicate}"
        );
        if let Some(p) = schema_param {
            write!(inner, " AND g.schema_id = ${p}").expect("write to String is infallible");
        }
        if let Some(state) = req.goal_state {
            // SQL-POLICY: fixed-fragment — closed enum match, no caller text.
            inner.push_str(match state {
                GoalState::Active => " AND g.state = 'Active'",
                GoalState::Paused => " AND g.state = 'Paused'",
                GoalState::Achieved => " AND g.state = 'Achieved'",
                GoalState::Abandoned => " AND g.state = 'Abandoned'",
            });
        }
        if let Some(p) = goal_ids_param {
            write!(inner, " AND g.goal_id = ANY(${p})").expect("write to String is infallible");
        } else if matches!(req.supersession, SupersessionStatus::HeadsOnly) {
            super::push_goal_heads_only_predicate(&mut inner);
        }
        if let Some((created_at_param, goal_id_param)) = cursor_params {
            write!(
                inner,
                " AND (g.created_at, g.goal_id) < (${created_at_param}, ${goal_id_param})"
            )
            .expect("write to String is infallible");
        }
        write!(
            inner,
            " ORDER BY g.created_at DESC, g.goal_id DESC LIMIT {fetch_limit}"
        )
        .expect("write to String is infallible");
        inner
    };

    // `goals.owner_id` is NULL for a World-owned row (0008_v005 dropped
    // goals_world_not_write_owner_chk; World is a persisted owner via
    // Engine::publish_to_world), and `s.id` is NULL for the World member of
    // the caller's read-owner set. An unguarded `=` would silently hide
    // every published Goal (NULL = NULL is NULL) — the same trap memories.rs
    // describes. So this joins with `=` and carries an explicit World arm
    // only when World is in the read set, exactly as memories.rs does
    // (sql-sweep S4); see the disjointness argument there.
    {
        let member_arm = format!(
            "SELECT lat.* \
             FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id) \
             JOIN LATERAL ( {inner}) lat ON TRUE",
            inner = inner_page(&super::read_owner_equality_predicate("g", "s")),
        );
        let world_in_read_set = req
            .read_owners
            .iter()
            .any(|owner| matches!(owner, proxima_core::OwnerRef::World));
        let body = if world_in_read_set {
            // Parenthesized so the World arm's per-arm ORDER/LIMIT binds
            // to its branch, not the union; see memories.rs.
            format!(
                "({member_arm}) UNION ALL ({world_arm})",
                world_arm = inner_page("g.owner_kind = 'world' AND g.owner_id IS NULL"),
            )
        } else {
            member_arm
        };
        format!(
            "SELECT page.goal_id, page.created_at, page.schema_id, page.schema_version, \
                    page.owner_kind, page.owner_id, \
                    page.title, page.text, page.state, \
                    page.supersedes, page.payload, page.dependency_goal_ids \
             FROM ( {body}) page \
             ORDER BY page.created_at DESC, page.goal_id DESC LIMIT {fetch_limit}"
        )
    }
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

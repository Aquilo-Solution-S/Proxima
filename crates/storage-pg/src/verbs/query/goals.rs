use std::fmt::Write as _;

use proxima_core::verbs::query::{
    EntityKind, GoalRow, QueryCursor, QueryRequest, SupersessionStatus,
};
use proxima_core::{OwnerRefKind, StorageError};
use sqlx::PgPool;

use crate::error::internal;

use super::rows::{GoalRowDb, goal_row_from_db};

#[allow(clippy::too_many_lines)]
pub(super) async fn query_goals(
    pool: &PgPool,
    req: &QueryRequest,
    read_owner_kinds: &[OwnerRefKind],
    read_owner_ids: &[Option<uuid::Uuid>],
    schema_id_filter: Option<&str>,
) -> Result<(Vec<GoalRow>, Option<QueryCursor>), StorageError> {
    let goal_ids: Vec<uuid::Uuid> = req.goal_ids.iter().map(|id| id.into_inner()).collect();
    let id_hydration =
        !req.memory_ids.is_empty() || !req.goal_ids.is_empty() || !req.edge_ids.is_empty();
    if id_hydration && goal_ids.is_empty() {
        return Ok((Vec::new(), None));
    }
    let payload_projection = if req.include_payloads {
        "g.payload"
    } else {
        "''::bytea"
    };
    let cursor = match &req.page.after {
        Some(QueryCursor::Goal {
            created_at,
            goal_id,
        }) => Some((*created_at, goal_id.into_inner())),
        _ => None,
    };
    let single_goal_stream = matches!(req.entity_kind, Some(EntityKind::Goal));
    let fetch_limit = if single_goal_stream {
        u64::from(req.limit) + 1
    } else {
        u64::from(req.limit)
    };
    let mut sql = format!(
        "SELECT page.goal_id, page.created_at, page.schema_id, page.schema_version, \
                page.owner_kind, page.owner_id, \
                page.title, page.text, page.state, \
                page.supersedes, page.payload, page.dependency_goal_ids \
         FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id) \
         JOIN LATERAL ( \
             SELECT g.goal_id, g.created_at, g.schema_id, g.schema_version, \
                    g.owner_kind, g.owner_id, \
                    g.title, g.text, g.state, \
                    g.supersedes, {payload_projection} AS payload, \
                    COALESCE(array_agg(e.target_goal_id) FILTER \
                    (WHERE e.target_goal_id IS NOT NULL), '{{}}'::uuid[]) AS dependency_goal_ids \
               FROM proxima_core.goals g \
               LEFT JOIN proxima_core.edges e \
                 ON e.source_goal_id = g.goal_id \
                AND e.relation = 'core/depends-on' \
                AND e.target_goal_id IS NOT NULL \
              WHERE g.owner_kind = s.kind \
                AND g.owner_id = s.id"
    );
    // Bindings: $1=owner_kind, $2=owner_id; the remaining params are pushed
    // in order, so optional filters and keyset cursors remain bound values.
    let mut next_param = 3;
    let schema_param = schema_id_filter.map(|_| {
        let param = next_param;
        next_param += 1;
        param
    });
    let goal_ids_param = (!goal_ids.is_empty()).then(|| {
        let param = next_param;
        next_param += 1;
        param
    });
    let cursor_params = cursor.map(|_| {
        let created_at = next_param;
        next_param += 1;
        let goal_id = next_param;
        (created_at, goal_id)
    });

    if let Some(p) = schema_param {
        write!(sql, " AND g.schema_id = ${p}").expect("write to String is infallible");
    }
    if let Some(p) = goal_ids_param {
        write!(sql, " AND g.goal_id = ANY(${p})").expect("write to String is infallible");
    } else if matches!(req.supersession, SupersessionStatus::HeadsOnly) {
        // SQL-POLICY: fixed-fragment
        sql.push_str(
            " AND NOT EXISTS (SELECT 1 FROM proxima_core.goals g2 \
                              WHERE g2.supersedes = g.goal_id)",
        );
    }
    if let Some((created_at_param, goal_id_param)) = cursor_params {
        write!(
            sql,
            " AND (g.created_at, g.goal_id) < (${created_at_param}, ${goal_id_param})"
        )
        .expect("write to String is infallible");
    }
    // SQL-POLICY: fixed-fragment
    sql.push_str(
        " GROUP BY g.goal_id, g.owner_kind, g.owner_id \
          ORDER BY g.created_at DESC, g.goal_id DESC LIMIT ",
    );
    // SQL-POLICY: fixed-fragment
    sql.push_str(&fetch_limit.to_string());
    // SQL-POLICY: fixed-fragment
    sql.push_str(
        ") page ON TRUE \
          ORDER BY page.created_at DESC, page.goal_id DESC LIMIT ",
    );
    // SQL-POLICY: fixed-fragment
    sql.push_str(&fetch_limit.to_string());

    // SQL-POLICY: fixed-fragment
    let mut q = sqlx::query_as::<_, GoalRowDb>(&sql)
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
    let mut rows = q.fetch_all(pool).await.map_err(internal)?;
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
